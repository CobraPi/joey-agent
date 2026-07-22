//! Cron job storage and management (port of `cron/jobs.py`).
//!
//! Jobs are stored in `~/.joey/cron/jobs.json` as `{"jobs": [...],
//! "updated_at": <iso>}` (hermes-compatible). Output is saved to
//! `~/.joey/cron/output/{job_id}/{timestamp}.md`.
//!
//! All schedule math and stored timestamps use the configured-timezone clock
//! (`joey_core::time::now()`, the port of `hermes_time.now`), serialized in
//! Python `datetime.isoformat()` shape (`+HH:MM` offset, microseconds).

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration as StdDuration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use fs2::FileExt;
use once_cell::sync::Lazy;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::croniter::{CronExpr, LocalZone};

/// Default ticker loop interval in seconds (upstream `TICKER_INTERVAL_SECONDS`).
pub const TICKER_INTERVAL_SECONDS: u64 = 60;

/// Grace window for one-shot jobs (upstream `ONESHOT_GRACE_SECONDS`).
pub const ONESHOT_GRACE_SECONDS: i64 = 120;

/// Fallback stale-recovery TTL for a one-shot's running-claim (seconds).
pub const ONESHOT_RUN_CLAIM_TTL_SECONDS: f64 = 1800.0;

const ONESHOT_RUN_CLAIM_TTL_HEADROOM: f64 = 3.0;
const DEFAULT_CRON_INACTIVITY_TIMEOUT: f64 = 600.0;

/// Upper bound on waiting for the cross-process `.jobs.lock` flock.
const JOBS_LOCK_TIMEOUT_SECONDS: f64 = 30.0;

/// Per-job output retention default (upstream `_CRON_OUTPUT_DEFAULT_KEEP`).
const CRON_OUTPUT_DEFAULT_KEEP: i64 = 50;

// =============================================================================
// Time helpers (Python `datetime.isoformat` / `fromisoformat` compatibility)
// =============================================================================

/// Current time on the configured-timezone clock.
pub(crate) fn time_now() -> DateTime<FixedOffset> {
    joey_core::time::now()
}

/// Format a datetime like Python `datetime.isoformat()`: seconds precision
/// with a `.ffffff` microsecond fraction when nonzero and a `+HH:MM` offset
/// (never `Z`), so hermes' `datetime.fromisoformat` can always parse it.
pub fn fmt_isoformat(dt: &DateTime<FixedOffset>) -> String {
    let micros = dt.timestamp_subsec_micros() % 1_000_000;
    if micros == 0 {
        format!("{}", dt.format("%Y-%m-%dT%H:%M:%S%:z"))
    } else {
        format!(
            "{}.{:06}{}",
            dt.format("%Y-%m-%dT%H:%M:%S"),
            micros,
            dt.format("%:z")
        )
    }
}

/// `fmt_isoformat` of the current configured-timezone instant.
pub fn now_isoformat() -> String {
    fmt_isoformat(&time_now())
}

/// A parsed ISO-8601 timestamp: local wall-clock fields plus the explicit
/// offset when one was present (naive timestamps carry `None`).
#[derive(Debug, Clone, Copy)]
pub struct IsoStamp {
    pub naive: NaiveDateTime,
    pub offset: Option<FixedOffset>,
}

/// Parse an ISO timestamp with Python-`fromisoformat` leniency:
/// `YYYY-MM-DD` (midnight), `T` or space separator, optional seconds,
/// optional `.fraction` (truncated to microseconds), trailing `Z`, and
/// `+HH[:MM[:SS]]` / `+HHMM` offsets.
pub fn parse_isoformat(input: &str) -> Result<IsoStamp> {
    let s = input.trim();
    parse_isoformat_inner(s).ok_or_else(|| anyhow!("Invalid isoformat string: '{}'", input))
}

fn parse_isoformat_inner(s: &str) -> Option<IsoStamp> {
    if s.len() < 10 || !s.is_ascii() {
        return None;
    }
    let date = NaiveDate::parse_from_str(&s[..10], "%Y-%m-%d").ok()?;
    if s.len() == 10 {
        return Some(IsoStamp {
            naive: date.and_time(NaiveTime::MIN),
            offset: None,
        });
    }
    // CPython's fromisoformat grammar is `YYYY-MM-DD[*HH[:MM...]]` where `*`
    // is ANY single separator character ('T' and ' ' in practice).
    let rest = &s[11..];
    let (rest, zulu) = match rest.strip_suffix('Z').or_else(|| rest.strip_suffix('z')) {
        Some(r) => (r, true),
        None => (rest, false),
    };
    let (time_part, off_part) = match rest.find(['+', '-']) {
        Some(i) => (&rest[..i], Some(&rest[i..])),
        None => (rest, None),
    };
    if zulu && off_part.is_some() {
        return None;
    }
    let time = parse_time_part(time_part)?;
    let offset = if zulu {
        Some(FixedOffset::east_opt(0)?)
    } else if let Some(o) = off_part {
        Some(parse_offset_part(o)?)
    } else {
        None
    };
    Some(IsoStamp {
        naive: date.and_time(time),
        offset,
    })
}

fn parse_two_digits(s: &str) -> Option<u32> {
    if s.len() != 2 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn parse_time_part(t: &str) -> Option<NaiveTime> {
    let mut parts = t.split(':');
    let hour = parse_two_digits(parts.next()?)?;
    let minute = parse_two_digits(parts.next()?)?;
    let (second, micro) = match parts.next() {
        None => (0, 0),
        Some(sec_part) => {
            let (sec_str, frac) = match sec_part.split_once('.') {
                Some((s, f)) => (s, Some(f)),
                None => (sec_part, None),
            };
            let second = parse_two_digits(sec_str)?;
            let micro = match frac {
                None => 0,
                Some(f) => {
                    if f.is_empty() || !f.bytes().all(|b| b.is_ascii_digit()) {
                        return None;
                    }
                    // Truncate to microseconds (Python 3.11 fromisoformat).
                    let digits: String = f.chars().take(6).collect();
                    let padded = format!("{:0<6}", digits);
                    padded.parse::<u32>().ok()?
                }
            };
            (second, micro)
        }
    };
    if parts.next().is_some() {
        return None;
    }
    NaiveTime::from_hms_micro_opt(hour, minute, second, micro)
}

fn parse_offset_part(o: &str) -> Option<FixedOffset> {
    let sign: i32 = match o.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let rest = &o[1..];
    let (h, m, s) = match rest.len() {
        2 => (parse_two_digits(rest)?, 0, 0),
        4 => (parse_two_digits(&rest[..2])?, parse_two_digits(&rest[2..])?, 0),
        5 if rest.as_bytes()[2] == b':' => {
            (parse_two_digits(&rest[..2])?, parse_two_digits(&rest[3..])?, 0)
        }
        8 if rest.as_bytes()[2] == b':' && rest.as_bytes()[5] == b':' => (
            parse_two_digits(&rest[..2])?,
            parse_two_digits(&rest[3..5])?,
            parse_two_digits(&rest[6..])?,
        ),
        _ => return None,
    };
    FixedOffset::east_opt(sign * (h * 3600 + m * 60 + s) as i32)
}

/// Port of `_ensure_aware`: return an aware datetime in the configured joey
/// timezone. Naive values are interpreted as *system-local wall time* (the
/// zone `datetime.now()` used when they were created) then converted.
pub fn ensure_aware(stamp: &IsoStamp) -> DateTime<FixedOffset> {
    let configured = LocalZone::configured();
    match stamp.offset {
        Some(off) => {
            let aware = off
                .from_local_datetime(&stamp.naive)
                .single()
                .unwrap_or_else(|| chrono::Utc.from_utc_datetime(&stamp.naive).fixed_offset());
            configured.to_fixed(aware)
        }
        None => configured.to_fixed(LocalZone::System.localize_lenient(stamp.naive)),
    }
}

/// Parse an ISO string and normalize it into the configured timezone.
pub fn ensure_aware_str(s: &str) -> Result<DateTime<FixedOffset>> {
    Ok(ensure_aware(&parse_isoformat(s)?))
}

/// Anchor a naive wall-clock time to the CONFIGURED timezone (used for naive
/// schedule inputs, port of `parse_schedule`'s `dt.replace(tzinfo=hermes_tz)`).
fn attach_configured_tz(naive: NaiveDateTime) -> DateTime<FixedOffset> {
    LocalZone::configured().localize_lenient(naive)
}

// =============================================================================
// Schedule / job data model
// =============================================================================

/// A job schedule (`{"kind": ..., ...}` dict upstream). Repaired/malformed
/// schedules are represented by an all-`None` value (upstream `{}`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Schedule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minutes: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Schedule {
    pub fn once(run_at: impl Into<String>, display: impl Into<String>) -> Self {
        Schedule {
            kind: Some("once".into()),
            run_at: Some(run_at.into()),
            display: Some(display.into()),
            ..Default::default()
        }
    }

    pub fn interval(minutes: i64, display: impl Into<String>) -> Self {
        Schedule {
            kind: Some("interval".into()),
            minutes: Some(minutes),
            display: Some(display.into()),
            ..Default::default()
        }
    }

    pub fn cron(expr: impl Into<String>, display: impl Into<String>) -> Self {
        Schedule {
            kind: Some("cron".into()),
            expr: Some(expr.into()),
            display: Some(display.into()),
            ..Default::default()
        }
    }

    /// The schedule kind, `""` when unset/repaired.
    pub fn kind_str(&self) -> &str {
        self.kind.as_deref().unwrap_or("")
    }

    fn is_recurring(&self) -> bool {
        matches!(self.kind_str(), "cron" | "interval")
    }
}

/// Bounded-repeat bookkeeping (`{"times": N|null, "completed": M}` upstream —
/// stored NESTED under the `repeat` key).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Repeat {
    #[serde(default)]
    pub times: Option<i64>,
    #[serde(default)]
    pub completed: i64,
}

fn default_true() -> bool {
    true
}
fn default_scheduled() -> String {
    "scheduled".to_string()
}

/// A cron job record. Field order mirrors upstream `create_job` so freshly
/// written files look like hermes'. Unknown keys round-trip via `extra`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub skills: Vec<String>,
    #[serde(default)]
    pub skill: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub provider_snapshot: Option<String>,
    #[serde(default)]
    pub model_snapshot: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub script: Option<String>,
    #[serde(default)]
    pub no_agent: bool,
    #[serde(default)]
    pub context_from: Option<Vec<String>>,
    #[serde(default)]
    pub schedule: Schedule,
    #[serde(default)]
    pub schedule_display: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat: Option<Repeat>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_scheduled")]
    pub state: String,
    #[serde(default)]
    pub paused_at: Option<String>,
    #[serde(default)]
    pub paused_reason: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub next_run_at: Option<String>,
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_delivery_error: Option<String>,
    #[serde(default)]
    pub deliver: String,
    #[serde(default)]
    pub origin: Option<Value>,
    #[serde(default)]
    pub enabled_toolsets: Option<Vec<String>>,
    #[serde(default)]
    pub workdir: Option<String>,
    /// Only persisted when explicitly set (upstream keeps the common case
    /// byte-identical by omitting the key).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_to_session: Option<bool>,
    /// Transient external-fire claim (`{"at": iso, "by": machine}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fire_claim: Option<Value>,
    /// Transient one-shot running claim (`{"at": iso, "by": machine}`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_claim: Option<Value>,
    /// Any fields this port doesn't model, preserved round-trip.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Generate a new job id (uuid4 hex, first 12 chars — upstream shape).
pub fn new_job_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

// =============================================================================
// Schedule parsing
// =============================================================================

static DURATION_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"^(\d+)\s*(m|min|mins|minute|minutes|h|hr|hrs|hour|hours|d|day|days)$").unwrap()
});
static CRON_FIELD_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[\d\*\-,/]+$").unwrap());
static ISO_DATE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}").unwrap());

/// Parse a duration string into minutes (`"30m"` → 30, `"2h"` → 120,
/// `"1d"` → 1440). Case-insensitive, optional whitespace before the unit.
pub fn parse_duration(s: &str) -> Result<i64> {
    let s = s.trim().to_lowercase();
    let caps = DURATION_RE.capture(&s)?;
    let value: i64 = caps
        .get(1)
        .unwrap()
        .as_str()
        .parse()
        .map_err(|_| duration_error(&s))?;
    let unit = caps.get(2).unwrap().as_str().chars().next().unwrap();
    let multiplier = match unit {
        'm' => 1,
        'h' => 60,
        'd' => 1440,
        _ => return Err(duration_error(&s)),
    };
    Ok(value * multiplier)
}

fn duration_error(s: &str) -> anyhow::Error {
    anyhow!("Invalid duration: '{}'. Use format like '30m', '2h', or '1d'", s)
}

trait CaptureOrDurationError {
    fn capture<'t>(&self, s: &'t str) -> Result<regex::Captures<'t>>;
}
impl CaptureOrDurationError for Regex {
    fn capture<'t>(&self, s: &'t str) -> Result<regex::Captures<'t>> {
        self.captures(s).ok_or_else(|| duration_error(s))
    }
}

/// Parse a schedule string into a structured [`Schedule`].
///
/// - `"30m"` / `"2h"` / `"1d"` → once, that far from now
/// - `"every 30m"` → recurring interval
/// - `"0 9 * * *"` → cron expression
/// - `"2026-02-03T14:00"` → once at timestamp (naive → configured timezone)
pub fn parse_schedule(schedule: &str) -> Result<Schedule> {
    let schedule = schedule.trim();
    let original = schedule;
    let schedule_lower = schedule.to_lowercase();

    // "every X" pattern → recurring interval.
    if schedule_lower.starts_with("every ") {
        let duration_str = schedule[6..].trim();
        let minutes = parse_duration(duration_str)?;
        return Ok(Schedule::interval(minutes, format!("every {}m", minutes)));
    }

    // Cron expression: 5+ space-separated fields whose first five contain
    // only digits/*-,/ (upstream detection — names are not routed here).
    let parts: Vec<&str> = schedule.split_whitespace().collect();
    if parts.len() >= 5 && parts[..5].iter().all(|p| CRON_FIELD_RE.is_match(p)) {
        if let Err(e) = CronExpr::parse(schedule) {
            bail!("Invalid cron expression '{}': {}", schedule, e);
        }
        return Ok(Schedule::cron(schedule, schedule));
    }

    // ISO timestamp (contains T or looks like a date).
    if schedule.contains('T') || ISO_DATE_RE.is_match(schedule) {
        let replaced = schedule.replace('Z', "+00:00");
        match parse_isoformat(&replaced) {
            Ok(stamp) => {
                // Naive timestamps anchor to the CONFIGURED joey timezone at
                // parse time (#51021) so "20:07" means 20:07 on the same
                // clock the scheduler checks against.
                let dt = match stamp.offset {
                    Some(off) => off
                        .from_local_datetime(&stamp.naive)
                        .single()
                        .unwrap_or_else(|| attach_configured_tz(stamp.naive)),
                    None => attach_configured_tz(stamp.naive),
                };
                return Ok(Schedule::once(
                    fmt_isoformat(&dt),
                    format!("once at {}", dt.format("%Y-%m-%d %H:%M")),
                ));
            }
            Err(e) => bail!("Invalid timestamp '{}': {}", schedule, e),
        }
    }

    // Duration like "30m", "2h", "1d" → one-shot from now.
    if let Ok(minutes) = parse_duration(schedule) {
        let run_at = time_now() + Duration::minutes(minutes);
        return Ok(Schedule::once(
            fmt_isoformat(&run_at),
            format!("once in {}", original),
        ));
    }

    bail!(
        "Invalid schedule '{}'. Use:\n  - Duration: '30m', '2h', '1d' (one-shot)\n  - Interval: 'every 30m', 'every 2h' (recurring)\n  - Cron: '0 9 * * *' (cron expression)\n  - Timestamp: '2026-02-03T14:00:00' (one-shot at time)",
        original
    )
}

// =============================================================================
// Next-run computation and grace windows
// =============================================================================

/// Port of `_recoverable_oneshot_run_at`: a one-shot run time still eligible
/// to fire (inside the grace window and never run before). Returns the
/// ORIGINAL stored string, unchanged.
fn recoverable_oneshot_run_at(
    schedule: &Schedule,
    now: DateTime<FixedOffset>,
    last_run_at: Option<&str>,
) -> Option<String> {
    if schedule.kind_str() != "once" {
        return None;
    }
    if last_run_at.is_some_and(|s| !s.is_empty()) {
        return None;
    }
    let run_at = schedule.run_at.as_deref().filter(|s| !s.is_empty())?;
    let run_at_dt = ensure_aware_str(run_at).ok()?;
    (run_at_dt >= now - Duration::seconds(ONESHOT_GRACE_SECONDS)).then(|| run_at.to_string())
}

/// Compute the next run time for a schedule (ISO string, or None when there
/// are no more runs). Mirrors upstream `compute_next_run(schedule, last_run_at)`.
pub fn compute_next_run(schedule: &Schedule, last_run_at: Option<&str>) -> Option<String> {
    let now = time_now();
    let last_run_at = last_run_at.filter(|s| !s.is_empty());
    match schedule.kind_str() {
        "once" => recoverable_oneshot_run_at(schedule, now, last_run_at),
        "interval" => {
            let minutes = schedule.minutes?;
            let next_run = match last_run_at {
                Some(last) => match ensure_aware_str(last) {
                    Ok(dt) => dt + Duration::minutes(minutes),
                    Err(_) => now + Duration::minutes(minutes),
                },
                // First run is now + interval.
                None => now + Duration::minutes(minutes),
            };
            Some(fmt_isoformat(&next_run))
        }
        "cron" => {
            let expr = schedule.expr.as_deref().filter(|e| !e.is_empty())?;
            let cron = CronExpr::parse(expr).ok()?;
            // Anchor on the actual last execution when available, so restarts
            // don't re-base the schedule on an arbitrary restart time.
            let base = last_run_at
                .and_then(|l| ensure_aware_str(l).ok())
                .unwrap_or(now);
            cron.next_after(base).map(|d| fmt_isoformat(&d))
        }
        _ => None,
    }
}

/// Port of `_compute_grace_seconds`: how late a recurring job can be and
/// still catch up instead of fast-forwarding. Half the period, clamped to
/// [120s, 7200s].
fn compute_grace_seconds(schedule: &Schedule) -> i64 {
    const MIN_GRACE: i64 = 120;
    const MAX_GRACE: i64 = 7200;
    match schedule.kind_str() {
        "interval" => {
            let period_seconds = schedule.minutes.unwrap_or(1) * 60;
            (period_seconds / 2).clamp(MIN_GRACE, MAX_GRACE)
        }
        "cron" => {
            if let Some(expr) = schedule.expr.as_deref() {
                if let Ok(cron) = CronExpr::parse(expr) {
                    let now = time_now();
                    if let Some(first) = cron.next_after(now) {
                        if let Some(second) = cron.next_after(first) {
                            let period_seconds = (second - first).num_seconds();
                            return (period_seconds / 2).clamp(MIN_GRACE, MAX_GRACE);
                        }
                    }
                }
            }
            MIN_GRACE
        }
        _ => MIN_GRACE,
    }
}

/// Resolve the one-shot running-claim stale-recovery TTL (seconds), derived
/// from `JOEY_CRON_TIMEOUT` exactly like upstream derives it from
/// `HERMES_CRON_TIMEOUT`.
fn oneshot_run_claim_ttl_seconds() -> f64 {
    let raw = std::env::var("JOEY_CRON_TIMEOUT").unwrap_or_default();
    let raw = raw.trim();
    let timeout = if raw.is_empty() {
        DEFAULT_CRON_INACTIVITY_TIMEOUT
    } else {
        raw.parse::<f64>().unwrap_or(DEFAULT_CRON_INACTIVITY_TIMEOUT)
    };
    if timeout <= 0.0 {
        return ONESHOT_RUN_CLAIM_TTL_SECONDS;
    }
    (timeout * ONESHOT_RUN_CLAIM_TTL_HEADROOM).max(ONESHOT_RUN_CLAIM_TTL_SECONDS)
}

/// Stable-ish machine identifier for claim attribution (NOT correctness).
fn machine_id() -> String {
    if let Ok(explicit) = std::env::var("JOEY_MACHINE_ID") {
        let explicit = explicit.trim().to_string();
        if !explicit.is_empty() {
            return explicit;
        }
    }
    let host = std::env::var("HOSTNAME")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    format!("{}:{}", host, std::process::id())
}

// =============================================================================
// Cross-process + in-process locking
// =============================================================================

static JOBS_PROCESS_LOCK: Mutex<()> = Mutex::new(());

/// Held for the duration of a load→modify→save critical section: the
/// process-wide mutex plus an advisory flock on `<cron_dir>/.jobs.lock`
/// (bounded 30s wait; degrades to in-process-only locking on timeout/failure,
/// matching upstream #60703 behaviour).
struct JobsLockGuard {
    _process: std::sync::MutexGuard<'static, ()>,
    file: Option<std::fs::File>,
}

impl Drop for JobsLockGuard {
    fn drop(&mut self) {
        if let Some(f) = self.file.take() {
            let _ = f.unlock();
        }
    }
}

// =============================================================================
// The cron store
// =============================================================================

/// The cron store rooted at one profile home's `cron/` directory.
#[derive(Debug, Clone)]
pub struct CronStore {
    dir: PathBuf,
}

impl CronStore {
    /// The store under the active `~/.joey` home.
    pub fn open_default() -> Self {
        Self {
            dir: joey_core::constants::joey_home().join("cron"),
        }
    }

    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The cron directory this store is rooted at.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn jobs_file(&self) -> PathBuf {
        self.dir.join("jobs.json")
    }

    fn output_root(&self) -> PathBuf {
        self.dir.join("output")
    }

    /// The lock file serializing whole ticks (`.tick.lock`).
    pub fn tick_lock_path(&self) -> PathBuf {
        self.dir.join(".tick.lock")
    }

    /// Ensure cron directories exist with owner-only permissions (0700).
    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        std::fs::create_dir_all(self.output_root())?;
        secure_dir(&self.dir);
        secure_dir(&self.output_root());
        Ok(())
    }

    /// Resolve a job's output directory, rejecting any path-escape attempt.
    pub fn job_output_dir(&self, job_id: &str) -> Result<PathBuf> {
        let text = job_id.trim();
        if text.is_empty()
            || text == "."
            || text == ".."
            || text.contains('/')
            || text.contains('\\')
            || Path::new(text).is_absolute()
        {
            bail!("Invalid cron job id for output path: '{}'", job_id);
        }
        Ok(self.output_root().join(text))
    }

    fn lock_jobs(&self) -> JobsLockGuard {
        let process = JOBS_PROCESS_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _ = self.ensure_dirs();
        let lock_path = self.dir.join(".jobs.lock");
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .append(true)
            .create(true)
            .open(&lock_path)
        {
            Ok(f) => {
                let deadline = Instant::now() + StdDuration::from_secs_f64(JOBS_LOCK_TIMEOUT_SECONDS);
                loop {
                    match f.try_lock_exclusive() {
                        Ok(()) => break Some(f),
                        Err(_) if Instant::now() >= deadline => {
                            tracing::error!(
                                "Timed out after {:.0}s waiting for the cron jobs lock ({}) — \
                                 another process is holding it. Proceeding with in-process \
                                 locking only so the scheduler stays alive.",
                                JOBS_LOCK_TIMEOUT_SECONDS,
                                lock_path.display()
                            );
                            break None;
                        }
                        Err(_) => std::thread::sleep(StdDuration::from_millis(100)),
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    "jobs.json cross-process lock unavailable ({}); proceeding with in-process lock only",
                    e
                );
                None
            }
        };
        JobsLockGuard {
            _process: process,
            file,
        }
    }

    // -------------------------------------------------------------------------
    // Load / save
    // -------------------------------------------------------------------------

    /// Load all jobs (tolerantly — see `load_repaired`).
    pub fn load(&self) -> Result<Vec<Job>> {
        Ok(self.load_repaired()?.0)
    }

    /// Tolerant load: BOM strip, control-char repair, bare-list wrap,
    /// per-record repair/containment. The bool reports whether repairs were
    /// made that should be persisted by a mutating caller.
    fn load_repaired(&self) -> Result<(Vec<Job>, bool)> {
        let (values, envelope_repaired) = self.read_jobs_values()?;
        let mut jobs = Vec::with_capacity(values.len());
        let mut needs_save = envelope_repaired;
        for value in values {
            match repair_record(value) {
                Some((job, record_repaired)) => {
                    needs_save |= record_repaired;
                    jobs.push(job);
                }
                None => {
                    // Per-record containment: one bad record never aborts the load.
                    tracing::warn!("Skipping malformed cron job record during load");
                }
            }
        }
        Ok((jobs, needs_save))
    }

    fn read_jobs_values(&self) -> Result<(Vec<Value>, bool)> {
        self.ensure_dirs()?;
        let path = self.jobs_file();
        if !path.exists() {
            return Ok((Vec::new(), false));
        }
        let bytes = std::fs::read(&path)
            .map_err(|e| anyhow!("Failed to read cron database: {}", e))?;
        let text = String::from_utf8_lossy(&bytes);
        // Strip a UTF-8 BOM (Windows Notepad / PowerShell writers).
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

        let mut strict_retry = false;
        let data: Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(_) => {
                // Retry after escaping bare control chars inside string values
                // (Python's json strict=False fallback).
                strict_retry = true;
                let sanitized = escape_control_chars_in_strings(text);
                serde_json::from_str(&sanitized).map_err(|e| {
                    anyhow!("Cron database corrupted and unrepairable: {}", e)
                })?
            }
        };

        match data {
            Value::Object(mut map) => {
                let jobs = match map.remove("jobs") {
                    Some(Value::Array(a)) => a,
                    _ => Vec::new(),
                };
                if strict_retry && !jobs.is_empty() {
                    // Hit control-character corruption — rewrite with escaping.
                    self.persist_values(&jobs)?;
                    tracing::warn!("Auto-repaired jobs.json (had invalid control characters)");
                }
                Ok((jobs, false))
            }
            Value::Array(a) => {
                // Bare array — wrap it back into {"jobs": [...]}.
                if !a.is_empty() {
                    self.persist_values(&a)?;
                    tracing::warn!("Auto-repaired jobs.json (bare list wrapped as dict)");
                }
                Ok((a, false))
            }
            other => bail!(
                "Cron database corrupted: expected {{'jobs': [...]}}, got {}",
                py_type_name(&other)
            ),
        }
    }

    /// Persist all jobs (takes the jobs lock).
    pub fn save(&self, jobs: &[Job]) -> Result<()> {
        let _guard = self.lock_jobs();
        self.save_unlocked(jobs)
    }

    fn save_unlocked(&self, jobs: &[Job]) -> Result<()> {
        self.write_envelope(&JobsEnvelope {
            jobs,
            updated_at: now_isoformat(),
        })
    }

    fn persist_values(&self, jobs: &[Value]) -> Result<()> {
        self.write_envelope(&JobsEnvelope {
            jobs,
            updated_at: now_isoformat(),
        })
    }

    fn write_envelope<T: Serialize>(&self, envelope: &JobsEnvelope<'_, T>) -> Result<()> {
        self.ensure_dirs()?;
        let text = serde_json::to_string_pretty(envelope)?;
        // json.dump-style: fsync before rename, no trailing newline.
        atomic_write_secure(&self.jobs_file(), text.as_bytes())
    }

    // -------------------------------------------------------------------------
    // CRUD
    // -------------------------------------------------------------------------

    /// Create a new cron job (defaults, snapshots and validation live HERE,
    /// mirroring upstream `create_job` — callers only pass options).
    pub fn create_job(
        &self,
        prompt: Option<&str>,
        schedule: &str,
        opts: CreateJobOptions,
    ) -> Result<Job> {
        let parsed_schedule = parse_schedule(schedule)?;

        // Normalize repeat: 0 or negative means infinite.
        let mut repeat = opts.repeat.filter(|r| *r > 0);
        // Auto-set repeat=1 for one-shot schedules if not specified.
        if parsed_schedule.kind_str() == "once" && repeat.is_none() {
            repeat = Some(1);
        }

        // Default delivery to origin if available, otherwise local.
        let deliver = opts
            .deliver
            .clone()
            .unwrap_or_else(|| if opts.origin.is_some() { "origin" } else { "local" }.to_string());

        let job_id = new_job_id();
        let now = now_isoformat();

        let normalized_skills = normalize_skill_list(opts.skill.as_deref(), opts.skills.as_deref());
        let normalized_model = normalize_optional_text(opts.model.as_deref(), false);
        let normalized_provider = normalize_optional_text(opts.provider.as_deref(), false);
        let normalized_base_url = normalize_optional_text(opts.base_url.as_deref(), true);
        let normalized_script = opts
            .script
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let normalized_toolsets = opts.enabled_toolsets.as_ref().and_then(|list| {
            let cleaned: Vec<String> = list
                .iter()
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
            (!cleaned.is_empty()).then_some(cleaned)
        });
        let normalized_workdir = normalize_workdir(opts.workdir.as_deref())?;
        let normalized_no_agent = opts.no_agent;

        // no_agent jobs are meaningless without a script.
        if normalized_no_agent && normalized_script.is_none() {
            bail!(
                "no_agent=True requires a script — with no agent and no script there is nothing for the job to run."
            );
        }

        let context_from = opts.context_from.as_ref().and_then(|list| {
            let cleaned: Vec<String> = list
                .iter()
                .map(|j| j.trim().to_string())
                .filter(|j| !j.is_empty())
                .collect();
            (!cleaned.is_empty()).then_some(cleaned)
        });

        let prompt_text = prompt.unwrap_or_default().to_string();

        let label_source = if !prompt_text.is_empty() {
            prompt_text.clone()
        } else if let Some(first) = normalized_skills.first() {
            first.clone()
        } else if normalized_no_agent {
            normalized_script.clone().unwrap_or_else(|| "cron job".to_string())
        } else {
            "cron job".to_string()
        };

        // Snapshot unpinned inference axes (fail-open: None when unresolvable).
        let (provider_snapshot, model_snapshot) = if normalized_no_agent {
            (None, None)
        } else {
            let model_snapshot = if normalized_model.is_none() {
                joey_core::Config::load()
                    .ok()
                    .map(|c| c.model().trim().to_string())
                    .filter(|m| !m.is_empty())
            } else {
                None
            };
            (None, model_snapshot)
        };

        let next_run_at = compute_next_run(&parsed_schedule, None);
        if parsed_schedule.kind_str() == "once" && next_run_at.is_none() {
            let run_at = parsed_schedule
                .run_at
                .clone()
                .unwrap_or_else(|| schedule.to_string());
            tracing::warn!(
                "Rejecting one-shot cron job '{}': run_at {} is outside the {}s grace window",
                opts.name
                    .clone()
                    .unwrap_or_else(|| truncate_chars(&label_source, 50).trim().to_string()),
                run_at,
                ONESHOT_GRACE_SECONDS
            );
            bail!(
                "Requested one-shot time {} is more than {}s in the past and cannot be scheduled.",
                run_at,
                ONESHOT_GRACE_SECONDS
            );
        }

        let job = Job {
            id: job_id,
            name: opts
                .name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| truncate_chars(&label_source, 50).trim().to_string()),
            prompt: prompt_text,
            skill: normalized_skills.first().cloned(),
            skills: normalized_skills,
            model: normalized_model,
            provider: normalized_provider,
            provider_snapshot,
            model_snapshot,
            base_url: normalized_base_url,
            script: normalized_script,
            no_agent: normalized_no_agent,
            context_from,
            schedule_display: parsed_schedule
                .display
                .clone()
                .unwrap_or_else(|| schedule.to_string()),
            schedule: parsed_schedule,
            repeat: Some(Repeat {
                times: repeat,
                completed: 0,
            }),
            enabled: true,
            state: "scheduled".to_string(),
            paused_at: None,
            paused_reason: None,
            created_at: Some(now),
            next_run_at,
            last_run_at: None,
            last_status: None,
            last_error: None,
            last_delivery_error: None,
            deliver,
            origin: opts.origin.clone(),
            enabled_toolsets: normalized_toolsets,
            workdir: normalized_workdir,
            attach_to_session: opts.attach_to_session,
            fire_claim: None,
            run_claim: None,
            extra: Map::new(),
        };

        let _guard = self.lock_jobs();
        let (mut jobs, _) = self.load_repaired()?;
        jobs.push(job.clone());
        self.save_unlocked(&jobs)?;
        Ok(job)
    }

    /// Get a job by exact ID.
    pub fn get_job(&self, job_id: &str) -> Result<Option<Job>> {
        Ok(self.load()?.into_iter().find(|j| j.id == job_id))
    }

    /// Resolve a job reference (ID or name). Exact ID wins; otherwise a
    /// case-insensitive name match; an ambiguous name is an error naming the
    /// matching IDs.
    pub fn resolve_job_ref(&self, job_ref: &str) -> Result<Option<Job>> {
        if job_ref.is_empty() {
            return Ok(None);
        }
        let jobs = self.load()?;
        if let Some(job) = jobs.iter().find(|j| j.id == job_ref) {
            return Ok(Some(job.clone()));
        }
        let ref_lower = job_ref.to_lowercase();
        let name_matches: Vec<&Job> = jobs
            .iter()
            .filter(|j| j.name.to_lowercase() == ref_lower)
            .collect();
        match name_matches.len() {
            0 => Ok(None),
            1 => Ok(Some(name_matches[0].clone())),
            n => {
                let ids: Vec<&str> = name_matches.iter().map(|j| j.id.as_str()).collect();
                bail!(
                    "Job name '{}' is ambiguous — matches {} jobs: {}. Use the job ID instead.",
                    job_ref,
                    n,
                    ids.join(", ")
                )
            }
        }
    }

    /// List jobs, optionally including disabled ones.
    pub fn list_jobs(&self, include_disabled: bool) -> Result<Vec<Job>> {
        let jobs = self.load()?;
        Ok(if include_disabled {
            jobs
        } else {
            jobs.into_iter().filter(|j| j.enabled).collect()
        })
    }

    fn update_locked<F>(&self, job_id: &str, mutate: F) -> Result<Option<Job>>
    where
        F: FnOnce(&mut Job),
    {
        let _guard = self.lock_jobs();
        let (mut jobs, _) = self.load_repaired()?;
        let mut updated: Option<Job> = None;
        for i in 0..jobs.len() {
            if jobs[i].id == job_id {
                mutate(&mut jobs[i]);
                updated = Some(jobs[i].clone());
                break;
            }
        }
        if let Some(updated) = updated {
            self.save_unlocked(&jobs)?;
            return Ok(Some(updated));
        }
        Ok(None)
    }

    /// Pause a job without deleting it. Accepts a job ID or name.
    pub fn pause_job(&self, job_ref: &str, reason: Option<&str>) -> Result<Option<Job>> {
        let Some(job) = self.resolve_job_ref(job_ref)? else {
            return Ok(None);
        };
        self.update_locked(&job.id, |j| {
            j.enabled = false;
            j.state = "paused".to_string();
            j.paused_at = Some(now_isoformat());
            j.paused_reason = reason.map(str::to_string);
        })
    }

    /// Resume a paused job, recomputing a FUTURE next run. Accepts ID or name.
    pub fn resume_job(&self, job_ref: &str) -> Result<Option<Job>> {
        let Some(job) = self.resolve_job_ref(job_ref)? else {
            return Ok(None);
        };
        let next_run_at = compute_next_run(&job.schedule, None);
        if next_run_at.is_none() && job.schedule.kind_str() == "once" {
            let run_at = job
                .schedule
                .run_at
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            bail!(
                "Cannot resume: one-shot time {} is in the past (grace window: {}s) and will never fire.",
                run_at,
                ONESHOT_GRACE_SECONDS
            );
        }
        self.update_locked(&job.id, |j| {
            j.enabled = true;
            j.state = "scheduled".to_string();
            j.paused_at = None;
            j.paused_reason = None;
            j.next_run_at = next_run_at;
        })
    }

    /// Schedule a job to run on the next scheduler tick. Accepts ID or name.
    pub fn trigger_job(&self, job_ref: &str) -> Result<Option<Job>> {
        let Some(job) = self.resolve_job_ref(job_ref)? else {
            return Ok(None);
        };
        self.update_locked(&job.id, |j| {
            j.enabled = true;
            j.state = "scheduled".to_string();
            j.paused_at = None;
            j.paused_reason = None;
            j.next_run_at = Some(now_isoformat());
        })
    }

    /// Remove a job by ID or name; deletes its output directory.
    pub fn remove_job(&self, job_ref: &str) -> Result<bool> {
        let Some(job) = self.resolve_job_ref(job_ref)? else {
            return Ok(false);
        };
        let canonical_id = job.id;
        let _guard = self.lock_jobs();
        let (mut jobs, _) = self.load_repaired()?;
        let original_len = jobs.len();
        jobs.retain(|j| j.id != canonical_id);
        if jobs.len() < original_len {
            // Resolve the output dir BEFORE saving so a legacy unsafe ID
            // fails closed without half-applying the removal.
            let job_output_dir = self.job_output_dir(&canonical_id)?;
            self.save_unlocked(&jobs)?;
            if job_output_dir.exists() {
                std::fs::remove_dir_all(&job_output_dir)
                    .with_context(|| format!("removing {}", job_output_dir.display()))?;
            }
            return Ok(true);
        }
        Ok(false)
    }

    /// Mark a job as having been run: stamps `last_*`, clears claims,
    /// advances/increments repeat state, auto-DELETES when the repeat limit
    /// is reached, and recomputes `next_run_at` off the completion time.
    pub fn mark_job_run(
        &self,
        job_id: &str,
        success: bool,
        error: Option<&str>,
        delivery_error: Option<&str>,
    ) -> Result<()> {
        let _guard = self.lock_jobs();
        let (mut jobs, _) = self.load_repaired()?;
        for i in 0..jobs.len() {
            if jobs[i].id != job_id {
                continue;
            }
            let now = now_isoformat();
            let mut limit_reached = false;
            {
                let job = &mut jobs[i];
                job.last_run_at = Some(now.clone());
                job.last_status = Some(if success { "ok" } else { "error" }.to_string());
                job.last_error = if !success {
                    error.map(str::to_string)
                } else {
                    None
                };
                // Track delivery failures separately — cleared on success.
                job.last_delivery_error = delivery_error.map(str::to_string);
                // Clear any external-fire claim / one-shot running claim.
                job.fire_claim = None;
                if job.run_claim.is_some() {
                    job.run_claim = None;
                }

                let kind_once = job.schedule.kind_str() == "once";
                if let Some(repeat) = job.repeat.as_mut() {
                    let times = repeat.times;
                    let mut completed = repeat.completed;
                    // Finite one-shots are pre-claimed by claim_dispatch()
                    // BEFORE the side effect runs — do not double-count.
                    let preclaimed_oneshot =
                        kind_once && times.is_some_and(|t| t > 0) && completed > 0;
                    if !preclaimed_oneshot {
                        completed += 1;
                        repeat.completed = completed;
                    }
                    // Repeat limit reached → REMOVE the job.
                    limit_reached = times.is_some_and(|t| t > 0 && completed >= t);
                }
            }
            if limit_reached {
                jobs.remove(i);
                return self.save_unlocked(&jobs);
            }

            let next = compute_next_run(&jobs[i].schedule, Some(&now));
            let job = &mut jobs[i];
            job.next_run_at = next;
            if job.next_run_at.is_none() {
                if job.schedule.is_recurring() {
                    // Recurring jobs must NEVER be silently disabled (#16265).
                    job.state = "error".to_string();
                    if job.last_error.is_none() {
                        job.last_error = Some(
                            "Failed to compute next run for recurring schedule (invalid or \
                             unsupported cron expression)"
                                .to_string(),
                        );
                    }
                    tracing::error!(
                        "Job '{}' ({}) could not compute next_run_at; leaving enabled and \
                         marking state=error so the job is not silently disabled.",
                        job.name,
                        job.schedule.kind_str()
                    );
                } else {
                    job.enabled = false;
                    job.state = "completed".to_string();
                }
            } else if job.state != "paused" {
                job.state = "scheduled".to_string();
            }
            return self.save_unlocked(&jobs);
        }
        tracing::warn!("mark_job_run: job_id {} not found, skipping save", job_id);
        Ok(())
    }

    /// Atomically claim a finite one-shot job dispatch BEFORE execution
    /// (at-most-times semantics). Returns whether the caller may proceed.
    pub fn claim_dispatch(&self, job_id: &str) -> Result<bool> {
        let _guard = self.lock_jobs();
        let (mut jobs, _) = self.load_repaired()?;
        for i in 0..jobs.len() {
            if jobs[i].id != job_id {
                continue;
            }
            if jobs[i].schedule.kind_str() != "once" {
                return Ok(true); // recurring jobs use advance_next_run()
            }
            let Some(repeat) = jobs[i].repeat.clone() else {
                return Ok(true); // no repeat limit — always dispatch
            };
            let Some(times) = repeat.times.filter(|t| *t > 0) else {
                return Ok(true); // infinite — always dispatch
            };
            let completed = repeat.completed;
            if completed >= times {
                // A prior tick claimed then died before mark_job_run could
                // remove it — clean up so it stops appearing due.
                let name = jobs[i].name.clone();
                jobs.remove(i);
                self.save_unlocked(&jobs)?;
                tracing::info!(
                    "Job '{}': dispatch limit reached ({}/{}) — removing",
                    name,
                    completed,
                    times
                );
                return Ok(false);
            }
            // Claim this dispatch before the side effect runs.
            jobs[i].repeat.as_mut().unwrap().completed = completed + 1;
            self.save_unlocked(&jobs)?;
            tracing::debug!(
                "Job '{}': claimed dispatch {}/{}",
                jobs[i].name,
                completed + 1,
                times
            );
            return Ok(true);
        }
        tracing::debug!(
            "claim_dispatch: job_id {} not in store — proceeding without claim",
            job_id
        );
        Ok(true)
    }

    /// Preemptively advance `next_run_at` for a recurring job BEFORE
    /// execution (at-most-once). One-shots are left unchanged.
    pub fn advance_next_run(&self, job_id: &str) -> Result<bool> {
        let _guard = self.lock_jobs();
        let (mut jobs, _) = self.load_repaired()?;
        for i in 0..jobs.len() {
            if jobs[i].id == job_id {
                if !jobs[i].schedule.is_recurring() {
                    return Ok(false);
                }
                let now = now_isoformat();
                let new_next = compute_next_run(&jobs[i].schedule, Some(&now));
                if let Some(new_next) = new_next {
                    if jobs[i].next_run_at.as_deref() != Some(new_next.as_str()) {
                        jobs[i].next_run_at = Some(new_next);
                        self.save_unlocked(&jobs)?;
                        return Ok(true);
                    }
                }
                return Ok(false);
            }
        }
        Ok(false)
    }

    /// All jobs due to run now. Persists repairs, one-shot run-claims and
    /// fast-forwarded schedules; stale recurring jobs are fast-forwarded but
    /// still fire ONCE.
    pub fn get_due_jobs(&self) -> Result<Vec<Job>> {
        let _guard = self.lock_jobs();
        let now = time_now();
        let (mut jobs, mut needs_save) = self.load_repaired()?;
        let run_claim_ttl = oneshot_run_claim_ttl_seconds();
        let mut due: Vec<Job> = Vec::new();
        let mut removals: Vec<usize> = Vec::new();

        for idx in 0..jobs.len() {
            let decision = due_decision(&mut jobs[idx], now, run_claim_ttl, &mut needs_save);
            match decision {
                DueDecision::Skip => {}
                DueDecision::Due => due.push(jobs[idx].clone()),
                DueDecision::Remove => removals.push(idx),
            }
        }

        for idx in removals.into_iter().rev() {
            jobs.remove(idx);
            needs_save = true;
        }
        if needs_save {
            self.save_unlocked(&jobs)?;
        }
        Ok(due)
    }

    // -------------------------------------------------------------------------
    // Run output
    // -------------------------------------------------------------------------

    /// Save one run's output document (written for EVERY run, including
    /// failures) as `output/<job_id>/%Y-%m-%d_%H-%M-%S.md` in the configured
    /// timezone, then prune to the retention cap.
    pub fn save_job_output(&self, job_id: &str, output: &str) -> Result<PathBuf> {
        self.ensure_dirs()?;
        let job_output_dir = self.job_output_dir(job_id)?;
        std::fs::create_dir_all(&job_output_dir)?;
        secure_dir(&job_output_dir);

        let timestamp = time_now().format("%Y-%m-%d_%H-%M-%S").to_string();
        let output_file = job_output_dir.join(format!("{}.md", timestamp));
        atomic_write_secure(&output_file, output.as_bytes())?;

        prune_job_output(&job_output_dir, cron_output_keep());
        Ok(output_file)
    }

    // -------------------------------------------------------------------------
    // Ticker heartbeat
    // -------------------------------------------------------------------------

    /// Record a ticker liveness signal; `success` additionally bumps the
    /// last-successful-tick marker. Best-effort: never disrupts the loop.
    pub fn record_ticker_heartbeat(&self, success: bool) {
        let _ = self.write_epoch_file("ticker_heartbeat");
        if success {
            let _ = self.write_epoch_file("ticker_last_success");
        }
    }

    fn write_epoch_file(&self, name: &str) -> Result<()> {
        self.ensure_dirs()?;
        let epoch = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        atomic_write_secure(&self.dir.join(name), format!("{}", epoch).as_bytes())
    }

    /// Seconds since the ticker loop last iterated, or None if unknown.
    pub fn get_ticker_heartbeat_age(&self) -> Option<f64> {
        epoch_file_age(&self.dir.join("ticker_heartbeat"))
    }

    /// Seconds since the last tick that completed without error, or None.
    pub fn get_ticker_success_age(&self) -> Option<f64> {
        epoch_file_age(&self.dir.join("ticker_last_success"))
    }
}

/// Options for [`CronStore::create_job`] — everything beyond prompt+schedule.
#[derive(Debug, Clone, Default)]
pub struct CreateJobOptions {
    pub name: Option<String>,
    /// How many times to run (None = forever; one-shots default to 1).
    pub repeat: Option<i64>,
    pub deliver: Option<String>,
    pub origin: Option<Value>,
    pub skill: Option<String>,
    pub skills: Option<Vec<String>>,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub base_url: Option<String>,
    pub script: Option<String>,
    pub context_from: Option<Vec<String>>,
    pub enabled_toolsets: Option<Vec<String>>,
    pub workdir: Option<String>,
    pub no_agent: bool,
    pub attach_to_session: Option<bool>,
}

#[derive(Serialize)]
struct JobsEnvelope<'a, T: Serialize> {
    jobs: &'a [T],
    updated_at: String,
}

enum DueDecision {
    Skip,
    Due,
    Remove,
}

/// Per-job due-scan body (port of the loop in `_get_due_jobs_locked`).
fn due_decision(
    job: &mut Job,
    now: DateTime<FixedOffset>,
    run_claim_ttl: f64,
    needs_save: &mut bool,
) -> DueDecision {
    if !job.enabled {
        return DueDecision::Skip;
    }

    // Cross-process running-claim guard (#59229): skip a one-shot whose run
    // is still in flight under a fresh claim from another scheduler.
    if job.schedule.kind_str() == "once" {
        if let Some(claim) = job.run_claim.as_ref() {
            if let Some(at) = claim.get("at").and_then(Value::as_str) {
                if let Ok(claimed_at) = ensure_aware_str(at) {
                    let age = (now - claimed_at).num_milliseconds() as f64 / 1000.0;
                    if age >= 0.0 && age < run_claim_ttl {
                        return DueDecision::Skip; // fresh claim held by an in-flight run
                    }
                }
            }
        }
    }

    let mut next_run = job.next_run_at.clone().filter(|s| !s.is_empty());
    if next_run.is_none() {
        let kind = job.schedule.kind_str().to_string();
        let mut recovery_kind = "one-shot";
        let mut recovered =
            recoverable_oneshot_run_at(&job.schedule, now, job.last_run_at.as_deref());
        if recovered.is_none() && job.schedule.is_recurring() {
            // Recompute a silently-skipped recurring job's next run.
            recovered = compute_next_run(&job.schedule, Some(&fmt_isoformat(&now)));
            recovery_kind = &kind;
        }
        let Some(recovered) = recovered else {
            return DueDecision::Skip;
        };
        tracing::info!(
            "Job '{}' had no next_run_at; recovering {} run at {}",
            job.name,
            recovery_kind,
            recovered
        );
        job.next_run_at = Some(recovered.clone());
        next_run = Some(recovered);
        *needs_save = true;
    }
    let next_run = next_run.unwrap();

    // Containment: an unparseable timestamp skips this job, never the scan.
    let Ok(stamp) = parse_isoformat(&next_run) else {
        tracing::warn!(
            "Skipping malformed cron job '{}' during due scan",
            job.name
        );
        return DueDecision::Skip;
    };
    let next_run_dt = ensure_aware(&stamp);
    let kind = job.schedule.kind_str().to_string();

    // Migration repair (#28934): a cron next_run_at stored under a different
    // UTC offset whose local wall-clock is still in the future is recomputed
    // to preserve wall-clock intent instead of firing early.
    if kind == "cron"
        && next_run_dt <= now
        && stamp.offset.is_some_and(|o| o != *now.offset())
        && stamp.naive > now.naive_local()
    {
        if let Some(new_next) = compute_next_run(&job.schedule, Some(&fmt_isoformat(&now))) {
            tracing::info!(
                "Job '{}' next_run_at offset changed. Recomputing cron run to preserve \
                 local wall-clock intent: {}",
                job.name,
                new_next
            );
            job.next_run_at = Some(new_next);
            *needs_save = true;
            return DueDecision::Skip;
        }
    }

    if next_run_dt > now {
        return DueDecision::Skip;
    }

    // Stale recurring jobs fast-forward (persisted) but still fire once.
    if job.schedule.is_recurring() {
        let grace = compute_grace_seconds(&job.schedule);
        if (now - next_run_dt).num_seconds() > grace {
            if let Some(new_next) = compute_next_run(&job.schedule, Some(&fmt_isoformat(&now))) {
                tracing::info!(
                    "Job '{}' missed its scheduled time ({}, grace={}s). Running now; next \
                     run provisionally set to: {} (re-anchored on completion)",
                    job.name,
                    next_run,
                    grace,
                    new_next
                );
                job.next_run_at = Some(new_next);
                *needs_save = true;
            }
            // Fall through — execute once now.
        }
    }

    if kind == "once" {
        // One-shot dispatch-limit guard (#38758): claimed but never marked.
        if let Some(repeat) = job.repeat.as_ref() {
            if repeat.times.is_some_and(|t| t > 0 && repeat.completed >= t) {
                if crate::scheduler::is_job_running(&job.id) {
                    tracing::info!(
                        "Job '{}': dispatch limit reached ({}/{}) but its run is still in \
                         flight in this process — keeping entry",
                        job.name,
                        repeat.completed,
                        repeat.times.unwrap_or(0)
                    );
                    return DueDecision::Skip;
                }
                tracing::info!(
                    "Job '{}': one-shot dispatch limit reached ({}/{}) — removing stale due entry",
                    job.name,
                    repeat.completed,
                    repeat.times.unwrap_or(0)
                );
                return DueDecision::Remove;
            }
        }
        // Durably claim the one-shot for the duration of its run (#59229).
        job.run_claim = Some(json!({"at": fmt_isoformat(&now), "by": machine_id()}));
        *needs_save = true;
    }

    DueDecision::Due
}

// =============================================================================
// Record repair (tolerant load)
// =============================================================================

/// Repair one raw job record into a typed [`Job`]. Returns None when the
/// record is beyond repair (containment: it is skipped, not fatal).
/// The bool reports repairs that upstream persists (id regeneration,
/// non-dict schedule, invalid timestamps).
fn repair_record(value: Value) -> Option<(Job, bool)> {
    let Value::Object(mut map) = value else {
        return None;
    };
    let mut repaired = false;

    // --- id: recover from legacy "job_id", else synthesize (persisted). ---
    let id_valid = map
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty());
    if !id_valid {
        // Non-string scalar id: coerce to its text form (read-safe).
        let numeric_id = match map.get("id") {
            Some(Value::Number(n)) => Some(n.to_string()),
            _ => None,
        };
        if let Some(text) = numeric_id {
            map.insert("id".into(), Value::String(text));
        } else {
            let recovered = map
                .remove("job_id")
                .and_then(|v| v.as_str().map(str::to_string))
                .filter(|s| !s.is_empty())
                .unwrap_or_else(new_job_id);
            map.insert("id".into(), Value::String(recovered));
            repaired = true;
        }
    }
    let job_id = map
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    // --- prompt: null → "" (read-safe). ---
    let prompt = match map.get("prompt") {
        Some(Value::String(s)) => s.clone(),
        _ => {
            map.insert("prompt".into(), Value::String(String::new()));
            String::new()
        }
    };

    // --- skills / skill: canonical list + legacy first-entry alignment. ---
    let skill_val = map.get("skill").and_then(Value::as_str).map(str::to_string);
    let skills_val: Option<Vec<String>> = match map.get("skills") {
        Some(Value::Array(a)) => Some(
            a.iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .collect(),
        ),
        Some(Value::String(s)) => Some(vec![s.clone()]),
        _ => None,
    };
    let skills = normalize_skill_list(skill_val.as_deref(), skills_val.as_deref());
    map.insert(
        "skills".into(),
        Value::Array(skills.iter().cloned().map(Value::String).collect()),
    );
    map.insert(
        "skill".into(),
        skills
            .first()
            .map(|s| Value::String(s.clone()))
            .unwrap_or(Value::Null),
    );

    // --- schedule: must be an object; keep a scalar's text for display. ---
    let mut scalar_schedule_text: Option<String> = None;
    let schedule_is_dict = matches!(map.get("schedule"), Some(Value::Object(_)));
    if !schedule_is_dict {
        match map.get("schedule") {
            Some(Value::Null) | None => {}
            Some(other) => {
                scalar_schedule_text = Some(match other {
                    Value::String(s) => s.clone(),
                    v => v.to_string(),
                });
            }
        }
        map.insert("schedule".into(), Value::Object(Map::new()));
        repaired = true;
    }

    // --- name: derive when missing/blank (read-safe). ---
    let name_ok = map
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty());
    if !name_ok {
        let script = map
            .get("script")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        let label_source = if !prompt.is_empty() {
            prompt.clone()
        } else if let Some(first) = skills.first() {
            first.clone()
        } else if !script.is_empty() {
            script
        } else if !job_id.is_empty() {
            job_id.clone()
        } else {
            "cron job".to_string()
        };
        let mut name = truncate_chars(&label_source, 50).trim().to_string();
        if name.is_empty() {
            name = "cron job".to_string();
        }
        map.insert("name".into(), Value::String(name));
    }

    // --- schedule_display: derive when missing/blank (read-safe). ---
    let display_ok = map
        .get("schedule_display")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty());
    if !display_ok {
        let derived = map
            .get("schedule")
            .and_then(Value::as_object)
            .and_then(|sched| {
                ["display", "value", "expr", "run_at"].iter().find_map(|k| {
                    sched
                        .get(*k)
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                })
            })
            .or(scalar_schedule_text)
            .unwrap_or_else(|| "?".to_string());
        map.insert("schedule_display".into(), Value::String(derived));
    }

    // --- state: default from enabled when missing/blank (read-safe). ---
    let state_ok = map
        .get("state")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.trim().is_empty());
    if !state_ok {
        let enabled = map.get("enabled").map(json_truthy).unwrap_or(true);
        map.insert(
            "state".into(),
            Value::String(if enabled { "scheduled" } else { "paused" }.to_string()),
        );
    }

    // --- next_run_at / last_run_at: strip invalid values (persisted). ---
    for key in ["next_run_at", "last_run_at"] {
        let bad = match map.get(key) {
            None | Some(Value::Null) => false,
            Some(Value::String(s)) => parse_isoformat(s).is_err(),
            Some(_) => true,
        };
        if bad {
            map.remove(key);
            repaired = true;
        }
    }

    // --- light coercions so strict typed fields can't abort the record
    // (upstream reads through dict.get + str() and never crashes on odd
    // types; we coerce at load so serde matches that tolerance). ---
    let ctx_replacement = match map.get("context_from") {
        Some(Value::String(s)) => {
            let s = s.trim().to_string();
            Some(if s.is_empty() {
                Value::Null
            } else {
                Value::Array(vec![Value::String(s)])
            })
        }
        Some(Value::Array(a)) if a.iter().any(|v| !v.is_string()) => Some(Value::Array(
            a.iter().map(|v| Value::String(scalar_text(v))).collect(),
        )),
        Some(Value::Array(_)) | Some(Value::Null) | None => None,
        Some(_) => Some(Value::Null),
    };
    if let Some(new) = ctx_replacement {
        map.insert("context_from".into(), new);
    }
    let toolsets_fix = match map.get("enabled_toolsets") {
        Some(Value::Array(a)) if a.iter().any(|v| !v.is_string()) => Some(Value::Array(
            a.iter().map(|v| Value::String(scalar_text(v))).collect(),
        )),
        Some(Value::String(s)) => Some(Value::Array(vec![Value::String(s.clone())])),
        Some(v) if !matches!(v, Value::Array(_) | Value::Null) => Some(Value::Null),
        _ => None,
    };
    if let Some(new) = toolsets_fix {
        map.insert("enabled_toolsets".into(), new);
    }
    // Optional-text fields: scalars are stringified, containers nulled.
    for key in [
        "model",
        "provider",
        "provider_snapshot",
        "model_snapshot",
        "base_url",
        "script",
        "workdir",
        "paused_at",
        "paused_reason",
        "created_at",
        "last_status",
        "last_error",
        "last_delivery_error",
    ] {
        let fix = match map.get(key) {
            Some(Value::String(_)) | Some(Value::Null) | None => None,
            Some(Value::Array(_)) | Some(Value::Object(_)) => Some(Value::Null),
            Some(other) => Some(Value::String(scalar_text(other))),
        };
        if let Some(new) = fix {
            map.insert(key.into(), new);
        }
    }
    let deliver_fix = match map.get("deliver") {
        Some(Value::String(_)) | None => None,
        Some(Value::Null) | Some(Value::Array(_)) | Some(Value::Object(_)) => {
            Some(Value::String(String::new()))
        }
        Some(other) => Some(Value::String(scalar_text(other))),
    };
    if let Some(new) = deliver_fix {
        map.insert("deliver".into(), new);
    }
    if map
        .get("attach_to_session")
        .is_some_and(|v| !matches!(v, Value::Bool(_) | Value::Null))
    {
        map.remove("attach_to_session");
    }
    let repeat_bad = map
        .get("repeat")
        .is_some_and(|v| !matches!(v, Value::Object(_) | Value::Null));
    if repeat_bad {
        map.remove("repeat");
    }
    if let Some(Value::Object(repeat)) = map.get_mut("repeat") {
        let times_fix = match repeat.get("times") {
            Some(Value::Number(n)) if n.as_i64().is_none() => n
                .as_f64()
                .map(|f| Value::Number((f as i64).into()))
                .or(Some(Value::Null)),
            Some(Value::String(s)) => Some(
                s.trim()
                    .parse::<i64>()
                    .map(|i| Value::Number(i.into()))
                    .unwrap_or(Value::Null),
            ),
            Some(v) if !matches!(v, Value::Number(_) | Value::Null) => Some(Value::Null),
            _ => None,
        };
        if let Some(new) = times_fix {
            repeat.insert("times".into(), new);
        }
        let completed_fix = match repeat.get("completed") {
            Some(Value::Number(n)) if n.as_i64().is_none() => {
                Some(n.as_f64().map(|f| f as i64).unwrap_or(0))
            }
            Some(Value::String(s)) => Some(s.trim().parse::<i64>().unwrap_or(0)),
            Some(v) if !matches!(v, Value::Number(_)) => Some(0),
            None => Some(0),
            _ => None,
        };
        if let Some(new) = completed_fix {
            repeat.insert("completed".into(), Value::Number(new.into()));
        }
    }
    for key in ["enabled", "no_agent"] {
        let coerced = match map.get(key) {
            Some(v) if !matches!(v, Value::Bool(_)) => Some(json_truthy(v)),
            _ => None,
        };
        if let Some(b) = coerced {
            map.insert(key.into(), Value::Bool(b));
        }
    }
    if let Some(Value::Object(sched)) = map.get_mut("schedule") {
        // Inner text fields must be strings (or null).
        for key in ["kind", "run_at", "expr", "display"] {
            let fix = match sched.get(key) {
                Some(Value::String(_)) | Some(Value::Null) | None => None,
                Some(Value::Array(_)) | Some(Value::Object(_)) => Some(Value::Null),
                Some(other) => Some(Value::String(scalar_text(other))),
            };
            if let Some(new) = fix {
                sched.insert(key.into(), new);
            }
        }
        // Truncate float minutes to an int (hand-edited files).
        let coerced = match sched.get("minutes") {
            Some(Value::Number(n)) if n.as_i64().is_none() => {
                Some(Value::Number((n.as_f64().unwrap_or(0.0) as i64).into()))
            }
            Some(Value::String(s)) => Some(
                s.trim()
                    .parse::<i64>()
                    .map(|i| Value::Number(i.into()))
                    .unwrap_or(Value::Null),
            ),
            Some(v) if !matches!(v, Value::Number(_) | Value::Null) => Some(Value::Null),
            _ => None,
        };
        if let Some(m) = coerced {
            sched.insert("minutes".into(), m);
        }
    }

    match serde_json::from_value::<Job>(Value::Object(map)) {
        Ok(job) => Some((job, repaired)),
        Err(e) => {
            tracing::warn!("Unrepairable cron job record ({}): skipping", e);
            None
        }
    }
}

/// Text form of a scalar JSON value (Python `str()`-ish).
fn scalar_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Python truthiness for the JSON values we coerce.
fn json_truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

// =============================================================================
// Small shared helpers
// =============================================================================

fn normalize_skill_list(skill: Option<&str>, skills: Option<&[String]>) -> Vec<String> {
    let raw_items: Vec<String> = match skills {
        Some(list) => list.to_vec(),
        None => skill
            .filter(|s| !s.is_empty())
            .map(|s| vec![s.to_string()])
            .unwrap_or_default(),
    };
    let mut normalized: Vec<String> = Vec::new();
    for item in raw_items {
        let text = item.trim().to_string();
        if !text.is_empty() && !normalized.contains(&text) {
            normalized.push(text);
        }
    }
    normalized
}

fn normalize_optional_text(value: Option<&str>, strip_trailing_slash: bool) -> Option<String> {
    let text = value?.trim();
    let text = if strip_trailing_slash {
        text.trim_end_matches('/')
    } else {
        text
    };
    (!text.is_empty()).then(|| text.to_string())
}

/// Port of `_normalize_workdir`: absolute, existing directory (or None).
fn normalize_workdir(workdir: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = workdir.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let expanded = shellexpand_tilde(raw);
    let path = Path::new(&expanded);
    if !path.is_absolute() {
        bail!(
            "Cron workdir must be an absolute path (got '{}'). Cron jobs run detached from any shell cwd, so relative paths are ambiguous.",
            raw
        );
    }
    let resolved = path
        .canonicalize()
        .map_err(|_| anyhow!("Cron workdir does not exist: {}", path.display()))?;
    if !resolved.is_dir() {
        bail!("Cron workdir is not a directory: {}", resolved.display());
    }
    Ok(Some(resolved.to_string_lossy().to_string()))
}

fn shellexpand_tilde(raw: &str) -> String {
    if raw == "~" {
        return joey_core::constants::user_home_dir().to_string_lossy().to_string();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return joey_core::constants::user_home_dir()
            .join(rest)
            .to_string_lossy()
            .to_string();
    }
    raw.to_string()
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

fn py_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) => {
            if n.is_f64() {
                "float"
            } else {
                "int"
            }
        }
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn secure_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    let _ = path;
}

fn secure_file(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Atomic write with fsync-before-rename and 0600 permissions.
fn atomic_write_secure(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::Builder::new()
        .prefix(".joey_cron_")
        .suffix(".tmp")
        .tempfile_in(dir)
        .with_context(|| format!("creating temp file in {}", dir.display()))?;
    tmp.write_all(contents)?;
    tmp.flush()?;
    tmp.as_file().sync_all()?;
    let tmp_path = tmp.into_temp_path();
    tmp_path
        .persist(path)
        .with_context(|| format!("renaming into place: {}", path.display()))?;
    secure_file(path);
    Ok(())
}

/// Escape raw control characters inside JSON string values (the port of
/// Python's `json.loads(..., strict=False)` repair retry).
fn escape_control_chars_in_strings(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut in_string = false;
    let mut escaped = false;
    for c in text.chars() {
        if in_string {
            if escaped {
                out.push(c);
                escaped = false;
            } else if c == '\\' {
                out.push(c);
                escaped = true;
            } else if c == '"' {
                out.push(c);
                in_string = false;
            } else if (c as u32) < 0x20 {
                out.push_str(&format!("\\u{:04x}", c as u32));
            } else {
                out.push(c);
            }
        } else {
            out.push(c);
            if c == '"' {
                in_string = true;
            }
        }
    }
    out
}

fn cron_output_keep() -> i64 {
    joey_core::Config::load()
        .map(|c| c.get_i64("cron.output_retention", CRON_OUTPUT_DEFAULT_KEEP))
        .unwrap_or(CRON_OUTPUT_DEFAULT_KEEP)
}

/// Remove the oldest `*.md` run-output files beyond `keep` (newest kept by
/// reverse-lexical timestamp filename sort). Non-positive `keep` disables.
fn prune_job_output(job_output_dir: &Path, keep: i64) -> usize {
    if keep <= 0 {
        return 0;
    }
    let Ok(entries) = std::fs::read_dir(job_output_dir) else {
        return 0;
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    let mut deleted = 0;
    for stale in files.into_iter().skip(keep as usize) {
        match std::fs::remove_file(&stale) {
            Ok(()) => deleted += 1,
            Err(e) => tracing::debug!("Failed to prune cron output {}: {}", stale.display(), e),
        }
    }
    deleted
}

fn epoch_file_age(path: &Path) -> Option<f64> {
    let raw = std::fs::read_to_string(path).ok()?;
    let stamp: f64 = raw.trim().parse().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    Some((now - stamp).max(0.0))
}

/// A snapshot of job ids currently executing in this process (used by the
/// due scan's stale-entry guard). Delegates to the scheduler's running set.
pub fn get_running_job_ids() -> HashSet<String> {
    crate::scheduler::get_running_job_ids()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Local, Timelike};

    fn store() -> (tempfile::TempDir, CronStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = CronStore::with_dir(dir.path().join("cron"));
        (dir, store)
    }

    fn read_raw(store: &CronStore) -> Value {
        let text = std::fs::read_to_string(store.dir().join("jobs.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn write_raw(store: &CronStore, text: &str) {
        std::fs::create_dir_all(store.dir()).unwrap();
        std::fs::write(store.dir().join("jobs.json"), text).unwrap();
    }

    // -------------------------------------------------------------------
    // Duration grammar (upstream parse_duration)
    // -------------------------------------------------------------------

    #[test]
    fn duration_grammar_table() {
        for (input, expected) in [
            ("30m", 30),
            ("30 m", 30),
            ("30min", 30),
            ("30mins", 30),
            ("30minute", 30),
            ("30minutes", 30),
            ("2h", 120),
            ("2hr", 120),
            ("2hrs", 120),
            ("2hour", 120),
            ("2 hours", 120),
            ("1d", 1440),
            ("1day", 1440),
            ("2 days", 2880),
            ("  45M  ", 45),
            ("3H", 180),
            ("1D", 1440),
        ] {
            assert_eq!(parse_duration(input).unwrap(), expected, "input {input:?}");
        }
        for bad in ["", "m", "30", "30x", "h30", "30 apples", "-5m", "1.5h"] {
            let err = parse_duration(bad).unwrap_err().to_string();
            assert!(err.starts_with("Invalid duration: '"), "input {bad:?}: {err}");
            assert!(err.contains("Use format like '30m', '2h', or '1d'"));
        }
    }

    // -------------------------------------------------------------------
    // ISO leniency (upstream datetime.fromisoformat usage)
    // -------------------------------------------------------------------

    #[test]
    fn iso_leniency_table() {
        let naive = |y, mo, d, h, mi, s, us| {
            NaiveDate::from_ymd_opt(y, mo, d)
                .unwrap()
                .and_hms_micro_opt(h, mi, s, us)
                .unwrap()
        };
        // Bare date → midnight, naive.
        let st = parse_isoformat("2026-02-03").unwrap();
        assert_eq!(st.naive, naive(2026, 2, 3, 0, 0, 0, 0));
        assert!(st.offset.is_none());
        // T separator, minutes precision.
        let st = parse_isoformat("2026-02-03T14:00").unwrap();
        assert_eq!(st.naive, naive(2026, 2, 3, 14, 0, 0, 0));
        assert!(st.offset.is_none());
        // Space separator.
        let st = parse_isoformat("2026-02-03 14:00").unwrap();
        assert_eq!(st.naive, naive(2026, 2, 3, 14, 0, 0, 0));
        // Optional seconds and fraction (truncated to micros).
        let st = parse_isoformat("2026-02-03T14:00:30").unwrap();
        assert_eq!(st.naive, naive(2026, 2, 3, 14, 0, 30, 0));
        let st = parse_isoformat("2026-02-03T14:00:30.123456").unwrap();
        assert_eq!(st.naive, naive(2026, 2, 3, 14, 0, 30, 123456));
        let st = parse_isoformat("2026-02-03T14:00:30.123456789").unwrap();
        assert_eq!(st.naive, naive(2026, 2, 3, 14, 0, 30, 123456));
        let st = parse_isoformat("2026-02-03T14:00:30.5").unwrap();
        assert_eq!(st.naive, naive(2026, 2, 3, 14, 0, 30, 500000));
        // Trailing Z.
        let st = parse_isoformat("2026-02-03T14:00:00Z").unwrap();
        assert_eq!(st.offset, FixedOffset::east_opt(0));
        // Explicit offsets.
        let st = parse_isoformat("2026-02-03T14:00:00+05:30").unwrap();
        assert_eq!(st.offset, FixedOffset::east_opt(19800));
        let st = parse_isoformat("2026-02-03T14:00:00-0530").unwrap();
        assert_eq!(st.offset, FixedOffset::east_opt(-19800));
        let st = parse_isoformat("2026-02-03T14:00:00+05").unwrap();
        assert_eq!(st.offset, FixedOffset::east_opt(18000));
        // CPython quirk: ANY single character separates date and time.
        let st = parse_isoformat("2026-02-03X14:00").unwrap();
        assert_eq!(st.naive, naive(2026, 2, 3, 14, 0, 0, 0));
        // Rejections carry the Python error shape.
        for bad in ["nope", "2026-13-99", "2026-02-03T25:00", "2026-2-3", "2026-02-03T14:00abc"] {
            let err = parse_isoformat(bad).unwrap_err().to_string();
            assert_eq!(err, format!("Invalid isoformat string: '{}'", bad));
        }
    }

    #[test]
    fn isoformat_output_shape() {
        // +HH:MM offset, microseconds, never Z.
        let dt = FixedOffset::east_opt(19800)
            .unwrap()
            .with_ymd_and_hms(2026, 2, 3, 14, 0, 30)
            .unwrap();
        assert_eq!(fmt_isoformat(&dt), "2026-02-03T14:00:30+05:30");
        let dt = dt.with_nanosecond(123_456_000).unwrap();
        assert_eq!(fmt_isoformat(&dt), "2026-02-03T14:00:30.123456+05:30");
        let utc = DateTime::parse_from_rfc3339("2026-02-03T14:00:30Z").unwrap();
        assert_eq!(fmt_isoformat(&utc), "2026-02-03T14:00:30+00:00");
        // Round-trips through our own parser.
        assert!(parse_isoformat(&now_isoformat()).unwrap().offset.is_some());
    }

    #[test]
    fn naive_legacy_timestamps_anchor_to_local_wall_clock() {
        // A naive stored timestamp is interpreted as system-local wall time
        // (upstream _ensure_aware), whatever zone the clock is configured to.
        let naive = NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let expected = Local
            .from_local_datetime(&naive)
            .earliest()
            .unwrap()
            .fixed_offset();
        let got = ensure_aware_str("2026-01-01T00:00").unwrap();
        assert_eq!(got.timestamp(), expected.timestamp());
    }

    // -------------------------------------------------------------------
    // parse_schedule + schedule_display normalization
    // -------------------------------------------------------------------

    #[test]
    fn schedule_display_normalization() {
        // Interval → minutes form.
        let s = parse_schedule("every 2h").unwrap();
        assert_eq!(s.kind_str(), "interval");
        assert_eq!(s.minutes, Some(120));
        assert_eq!(s.display.as_deref(), Some("every 120m"));
        assert_eq!(parse_schedule("EVERY 30M").unwrap().display.as_deref(), Some("every 30m"));
        // Cron → the expression itself.
        let s = parse_schedule("0 9 * * 1").unwrap();
        assert_eq!(s.kind_str(), "cron");
        assert_eq!(s.expr.as_deref(), Some("0 9 * * 1"));
        assert_eq!(s.display.as_deref(), Some("0 9 * * 1"));
        // ISO → "once at YYYY-MM-DD HH:MM" (wall clock of the parsed time).
        let s = parse_schedule("2099-02-03T14:00").unwrap();
        assert_eq!(s.kind_str(), "once");
        assert_eq!(s.display.as_deref(), Some("once at 2099-02-03 14:00"));
        let run_at = s.run_at.unwrap();
        let stamp = parse_isoformat(&run_at).unwrap();
        assert!(stamp.offset.is_some(), "naive input must be anchored: {run_at}");
        assert_eq!(stamp.naive.format("%H:%M").to_string(), "14:00");
        // Aware ISO keeps its own offset's wall clock in the display.
        let s = parse_schedule("2099-02-03T14:00:00+05:30").unwrap();
        assert_eq!(s.display.as_deref(), Some("once at 2099-02-03 14:00"));
        assert_eq!(s.run_at.as_deref(), Some("2099-02-03T14:00:00+05:30"));
        // Duration one-shot → "once in {original}".
        let s = parse_schedule("30m").unwrap();
        assert_eq!(s.kind_str(), "once");
        assert_eq!(s.display.as_deref(), Some("once in 30m"));
        let run_at = ensure_aware_str(&s.run_at.unwrap()).unwrap();
        let delta = (run_at - time_now()).num_seconds();
        assert!((1790..=1810).contains(&delta), "delta {delta}");
    }

    #[test]
    fn parse_schedule_error_shapes() {
        let err = parse_schedule("garbage").unwrap_err().to_string();
        assert!(err.starts_with("Invalid schedule 'garbage'. Use:\n"));
        assert!(err.contains("- Cron: '0 9 * * *' (cron expression)"));
        let err = parse_schedule("61 9 * * *").unwrap_err().to_string();
        assert!(err.starts_with("Invalid cron expression '61 9 * * *':"), "{err}");
        let err = parse_schedule("2026-99-99T00:00").unwrap_err().to_string();
        assert!(err.starts_with("Invalid timestamp '2026-99-99T00:00':"), "{err}");
        // Upstream detection: names in the first five fields are NOT routed
        // to cron — they fall through to the generic schedule error.
        let err = parse_schedule("0 9 * * mon").unwrap_err().to_string();
        assert!(err.starts_with("Invalid schedule '0 9 * * mon'"), "{err}");
        // Seconds-last 6-field forms are accepted.
        assert_eq!(parse_schedule("0 9 * * 1 30").unwrap().kind_str(), "cron");
    }

    // -------------------------------------------------------------------
    // compute_next_run + grace
    // -------------------------------------------------------------------

    #[test]
    fn interval_reanchors_on_last_run() {
        let sched = Schedule::interval(60, "every 60m");
        let last = time_now() - Duration::minutes(10);
        let next = compute_next_run(&sched, Some(&fmt_isoformat(&last))).unwrap();
        let next_dt = ensure_aware_str(&next).unwrap();
        assert_eq!(next_dt.timestamp(), (last + Duration::minutes(60)).timestamp());
        // First run: now + interval.
        let next = compute_next_run(&sched, None).unwrap();
        let delta = (ensure_aware_str(&next).unwrap() - time_now()).num_seconds();
        assert!((3590..=3610).contains(&delta), "delta {delta}");
        // Unparseable last_run_at falls back to now + interval.
        let next = compute_next_run(&sched, Some("not-a-date")).unwrap();
        let delta = (ensure_aware_str(&next).unwrap() - time_now()).num_seconds();
        assert!((3590..=3610).contains(&delta), "delta {delta}");
    }

    #[test]
    fn oneshot_grace_window() {
        // Within the 120s grace: still eligible, returns the ORIGINAL string.
        let recent = fmt_isoformat(&(time_now() - Duration::seconds(60)));
        let sched = Schedule::once(recent.clone(), "once at x");
        assert_eq!(compute_next_run(&sched, None).as_deref(), Some(recent.as_str()));
        // Beyond the grace: no more runs.
        let stale = fmt_isoformat(&(time_now() - Duration::seconds(ONESHOT_GRACE_SECONDS + 30)));
        let sched = Schedule::once(stale, "once at x");
        assert_eq!(compute_next_run(&sched, None), None);
        // Already ran: never eligible again.
        let future = fmt_isoformat(&(time_now() + Duration::minutes(5)));
        let sched = Schedule::once(future, "once at x");
        assert_eq!(compute_next_run(&sched, Some("2026-01-01T00:00:00+00:00")), None);
    }

    #[test]
    fn recurring_grace_is_half_period_clamped() {
        assert_eq!(compute_grace_seconds(&Schedule::interval(10, "")), 300);
        assert_eq!(compute_grace_seconds(&Schedule::interval(2, "")), 120);
        assert_eq!(compute_grace_seconds(&Schedule::interval(1000, "")), 7200);
        // Hourly cron: period 3600 → grace 1800.
        assert_eq!(compute_grace_seconds(&Schedule::cron("0 * * * *", "")), 1800);
        // One-shots use the minimum.
        assert_eq!(compute_grace_seconds(&Schedule::once("2026-01-01T00:00:00+00:00", "")), 120);
    }

    // -------------------------------------------------------------------
    // Envelope + fixture round-trip
    // -------------------------------------------------------------------

    #[test]
    fn save_writes_upstream_envelope() {
        let (_tmp, store) = store();
        store.save(&[]).unwrap();
        let text = std::fs::read_to_string(store.dir().join("jobs.json")).unwrap();
        assert!(text.starts_with("{\n  \"jobs\": ["), "{text}");
        assert!(text.contains("\"updated_at\":"));
        assert!(!text.ends_with('\n'), "no trailing newline");
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert!(doc["jobs"].as_array().unwrap().is_empty());
        assert!(parse_isoformat(doc["updated_at"].as_str().unwrap()).is_ok());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.dir().join("jobs.json"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
            let dir_mode = std::fs::metadata(store.dir()).unwrap().permissions().mode();
            assert_eq!(dir_mode & 0o777, 0o700);
        }
    }

    const UPSTREAM_FIXTURE: &str = r#"{
  "jobs": [
    {
      "id": "abc123def456",
      "name": "daily report",
      "prompt": "Summarize the day",
      "skills": ["reporting"],
      "skill": "reporting",
      "model": null,
      "provider": null,
      "provider_snapshot": "openrouter",
      "model_snapshot": "moonshotai/kimi-k2",
      "base_url": null,
      "script": null,
      "no_agent": false,
      "context_from": null,
      "schedule": {"kind": "cron", "expr": "0 9 * * 1", "display": "0 9 * * 1"},
      "schedule_display": "0 9 * * 1",
      "repeat": {"times": null, "completed": 3},
      "enabled": true,
      "state": "scheduled",
      "paused_at": null,
      "paused_reason": null,
      "created_at": "2026-07-01T10:00:00.123456+05:30",
      "next_run_at": "2026-07-27T09:00:00+05:30",
      "last_run_at": "2026-07-20T09:00:00.654321+05:30",
      "last_status": "ok",
      "last_error": null,
      "last_delivery_error": null,
      "deliver": "origin",
      "origin": {"platform": "telegram", "chat_id": 12345},
      "enabled_toolsets": ["files"],
      "workdir": null,
      "attach_to_session": true,
      "custom_unknown_field": {"nested": [1, 2, "three"]},
      "another_unknown": "keep me"
    }
  ],
  "updated_at": "2026-07-20T09:00:01+05:30"
}"#;

    #[test]
    fn upstream_fixture_roundtrips_losslessly() {
        let (_tmp, store) = store();
        write_raw(&store, UPSTREAM_FIXTURE);
        let jobs = store.load().unwrap();
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.id, "abc123def456");
        assert_eq!(job.name, "daily report");
        assert_eq!(job.skills, vec!["reporting"]);
        assert_eq!(job.skill.as_deref(), Some("reporting"));
        assert_eq!(job.schedule.kind_str(), "cron");
        assert_eq!(job.schedule.expr.as_deref(), Some("0 9 * * 1"));
        assert_eq!(job.schedule.display.as_deref(), Some("0 9 * * 1"));
        let repeat = job.repeat.as_ref().unwrap();
        assert_eq!(repeat.times, None);
        assert_eq!(repeat.completed, 3);
        assert_eq!(job.deliver, "origin");
        assert_eq!(job.attach_to_session, Some(true));
        assert_eq!(job.extra["another_unknown"], "keep me");

        // Round-trip: every original key/value preserved (order-insensitive),
        // including unknown fields; repeat stays NESTED.
        store.save(&jobs).unwrap();
        let raw = read_raw(&store);
        let original: Value = serde_json::from_str(UPSTREAM_FIXTURE).unwrap();
        assert_eq!(raw["jobs"][0], original["jobs"][0]);
        assert!(raw["jobs"][0]["repeat"].is_object());
        assert!(raw["updated_at"].is_string());
    }

    // -------------------------------------------------------------------
    // Tolerant load / repairs
    // -------------------------------------------------------------------

    #[test]
    fn load_strips_utf8_bom() {
        let (_tmp, store) = store();
        write_raw(&store, "\u{feff}{\"jobs\": [], \"updated_at\": \"x\"}");
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn load_wraps_bare_list_and_persists_envelope() {
        let (_tmp, store) = store();
        write_raw(
            &store,
            r#"[{"id": "job00000001", "name": "n", "prompt": "p", "schedule": {"kind": "interval", "minutes": 5, "display": "every 5m"}}]"#,
        );
        let jobs = store.load().unwrap();
        assert_eq!(jobs.len(), 1);
        // The file was rewritten to the dict envelope shape.
        let raw = read_raw(&store);
        assert!(raw.is_object());
        assert_eq!(raw["jobs"][0]["id"], "job00000001");
        assert!(raw["updated_at"].is_string());
    }

    #[test]
    fn load_escapes_control_characters() {
        let (_tmp, store) = store();
        write_raw(
            &store,
            "{\"jobs\": [{\"id\": \"ctl000000001\", \"name\": \"a\u{0001}b\", \"prompt\": \"p\", \"schedule\": {\"kind\": \"interval\", \"minutes\": 5}}], \"updated_at\": \"x\"}",
        );
        let jobs = store.load().unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].name, "a\u{0001}b");
    }

    #[test]
    fn load_rejects_non_dict_non_list() {
        let (_tmp, store) = store();
        write_raw(&store, "\"what\"");
        let err = store.load().unwrap_err().to_string();
        assert_eq!(err, "Cron database corrupted: expected {'jobs': [...]}, got str");
        write_raw(&store, "42");
        let err = store.load().unwrap_err().to_string();
        assert_eq!(err, "Cron database corrupted: expected {'jobs': [...]}, got int");
    }

    #[test]
    fn load_repairs_records_without_aborting() {
        let (_tmp, store) = store();
        write_raw(
            &store,
            r#"{"jobs": [
                {"job_id": "legacy000001", "prompt": "p1", "schedule": {"kind": "interval", "minutes": 5, "display": "every 5m"}},
                {"id": "nulls0000001", "name": null, "prompt": null, "state": null, "schedule_display": null,
                 "schedule": {"kind": "interval", "minutes": 5, "display": "every 5m"},
                 "next_run_at": "not a timestamp", "last_run_at": 12345},
                {"id": "badsched0001", "name": "bs", "prompt": "p", "schedule": "every 9m"},
                42,
                "not a job"
            ], "updated_at": "x"}"#,
        );
        let jobs = store.load().unwrap();
        assert_eq!(jobs.len(), 3, "bad records are skipped, not fatal");
        // Legacy job_id key recovered as id.
        assert_eq!(jobs[0].id, "legacy000001");
        // Null coercions: prompt "", derived name, derived state + display;
        // invalid timestamps stripped.
        let j = &jobs[1];
        assert_eq!(j.prompt, "");
        assert_eq!(j.name, "nulls0000001"); // falls back to the id
        assert_eq!(j.state, "scheduled");
        assert_eq!(j.schedule_display, "every 5m"); // from schedule.display
        assert_eq!(j.next_run_at, None);
        assert_eq!(j.last_run_at, None);
        // Non-dict schedule repaired to empty, its text kept for display.
        let j = &jobs[2];
        assert_eq!(j.schedule.kind_str(), "");
        assert_eq!(j.schedule_display, "every 9m");
    }

    #[test]
    fn load_derives_name_from_prompt_prefix() {
        let (_tmp, store) = store();
        let long_prompt = "x".repeat(80);
        write_raw(
            &store,
            &format!(
                r#"{{"jobs": [{{"id": "n0000000001", "prompt": "{long_prompt}", "schedule": {{"kind": "interval", "minutes": 5}}}}], "updated_at": "x"}}"#
            ),
        );
        let jobs = store.load().unwrap();
        assert_eq!(jobs[0].name, "x".repeat(50));
    }

    #[test]
    fn missing_id_is_regenerated() {
        let (_tmp, store) = store();
        write_raw(
            &store,
            r#"{"jobs": [{"name": "noid", "prompt": "p", "schedule": {"kind": "interval", "minutes": 5}}], "updated_at": "x"}"#,
        );
        let jobs = store.load().unwrap();
        assert_eq!(jobs[0].id.len(), 12);
    }

    // -------------------------------------------------------------------
    // create_job / one-shot lifecycle
    // -------------------------------------------------------------------

    #[test]
    fn create_job_defaults() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("Say hello to everyone on the team, please, thanks a lot"), "every 30m", CreateJobOptions::default())
            .unwrap();
        assert_eq!(job.id.len(), 12);
        // Default name = first 50 chars of the prompt.
        assert_eq!(job.name, "Say hello to everyone on the team, please, thanks");
        assert_eq!(job.deliver, "local");
        assert_eq!(job.state, "scheduled");
        assert!(job.enabled);
        let repeat = job.repeat.as_ref().unwrap();
        assert_eq!(repeat.times, None); // recurring default: forever
        assert_eq!(repeat.completed, 0);
        assert!(job.next_run_at.is_some());
        assert!(job.created_at.is_some());
        // Origin present → deliver defaults to origin.
        let job2 = store
            .create_job(
                Some("p"),
                "every 1h",
                CreateJobOptions {
                    origin: Some(json!({"platform": "cli"})),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(job2.deliver, "origin");
        assert_eq!(store.load().unwrap().len(), 2);
    }

    #[test]
    fn oneshot_auto_repeat_and_delete_on_completion() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("once prompt"), "30m", CreateJobOptions::default())
            .unwrap();
        // Auto repeat.times = 1 for kind=once.
        assert_eq!(job.repeat.as_ref().unwrap().times, Some(1));
        assert_eq!(job.schedule.kind_str(), "once");
        // Completing the run REMOVES the job from the store.
        store.mark_job_run(&job.id, true, None, None).unwrap();
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn oneshot_in_past_is_rejected_with_upstream_message() {
        let (_tmp, store) = store();
        let err = store
            .create_job(Some("p"), "2020-01-01T00:00", CreateJobOptions::default())
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("is more than 120s in the past and cannot be scheduled."),
            "{err}"
        );
        assert!(err.starts_with("Requested one-shot time "), "{err}");
    }

    #[test]
    fn no_agent_requires_script() {
        let (_tmp, store) = store();
        let err = store
            .create_job(
                Some("p"),
                "every 5m",
                CreateJobOptions {
                    no_agent: true,
                    ..Default::default()
                },
            )
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "no_agent=True requires a script — with no agent and no script there is nothing for the job to run."
        );
    }

    #[test]
    fn claim_dispatch_semantics() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("p"), "30m", CreateJobOptions::default())
            .unwrap();
        // First claim wins and persists completed=1.
        assert!(store.claim_dispatch(&job.id).unwrap());
        let stored = store.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.repeat.as_ref().unwrap().completed, 1);
        // Second claim hits the limit and removes the job.
        assert!(!store.claim_dispatch(&job.id).unwrap());
        assert!(store.load().unwrap().is_empty());
        // Recurring jobs always pass.
        let rec = store
            .create_job(Some("p"), "every 5m", CreateJobOptions::default())
            .unwrap();
        assert!(store.claim_dispatch(&rec.id).unwrap());
        // A one-shot claimed then completed is not double-counted.
        let one = store
            .create_job(Some("p"), "30m", CreateJobOptions::default())
            .unwrap();
        assert!(store.claim_dispatch(&one.id).unwrap());
        store.mark_job_run(&one.id, true, None, None).unwrap();
        assert!(store.get_job(&one.id).unwrap().is_none());
    }

    // -------------------------------------------------------------------
    // Due scan
    // -------------------------------------------------------------------

    #[test]
    fn due_scan_recovers_missing_next_run() {
        let (_tmp, store) = store();
        let mut job = store
            .create_job(Some("p"), "every 10m", CreateJobOptions::default())
            .unwrap();
        job.next_run_at = None;
        store.save(&[job.clone()]).unwrap();
        // Missing next_run_at is NOT due; a future time is recovered+persisted.
        let due = store.get_due_jobs().unwrap();
        assert!(due.is_empty());
        let stored = store.get_job(&job.id).unwrap().unwrap();
        let next = ensure_aware_str(stored.next_run_at.as_deref().unwrap()).unwrap();
        assert!(next > time_now());
    }

    #[test]
    fn due_scan_fast_forwards_stale_recurring_but_fires_once() {
        let (_tmp, store) = store();
        let mut job = store
            .create_job(Some("p"), "every 10m", CreateJobOptions::default())
            .unwrap();
        // 400s late > grace (300s for a 10m interval) → fast-forward + fire.
        job.next_run_at = Some(fmt_isoformat(&(time_now() - Duration::seconds(400))));
        store.save(&[job.clone()]).unwrap();
        let due = store.get_due_jobs().unwrap();
        assert_eq!(due.len(), 1, "stale job still fires once");
        let stored = store.get_job(&job.id).unwrap().unwrap();
        let next = ensure_aware_str(stored.next_run_at.as_deref().unwrap()).unwrap();
        assert!(next > time_now(), "fast-forward persisted");
        // Within grace: due, and next_run_at NOT rewritten by the scan.
        let within = fmt_isoformat(&(time_now() - Duration::seconds(100)));
        let mut job2 = store.get_job(&job.id).unwrap().unwrap();
        job2.next_run_at = Some(within.clone());
        store.save(&[job2]).unwrap();
        let due = store.get_due_jobs().unwrap();
        assert_eq!(due.len(), 1);
        let stored = store.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.next_run_at.as_deref(), Some(within.as_str()));
    }

    #[test]
    fn due_scan_claims_oneshots_and_skips_claimed() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("p"), "30m", CreateJobOptions::default())
            .unwrap();
        store.trigger_job(&job.id).unwrap();
        let due = store.get_due_jobs().unwrap();
        assert_eq!(due.len(), 1);
        // The run_claim was stamped and persisted...
        let stored = store.get_job(&job.id).unwrap().unwrap();
        let claim = stored.run_claim.as_ref().expect("run_claim stamped");
        assert!(claim.get("at").and_then(Value::as_str).is_some());
        assert!(claim.get("by").and_then(Value::as_str).is_some());
        // ...so the next scan skips it while the claim is fresh.
        assert!(store.get_due_jobs().unwrap().is_empty());
    }

    #[test]
    fn due_scan_ignores_disabled_jobs() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("p"), "every 5m", CreateJobOptions::default())
            .unwrap();
        store.trigger_job(&job.id).unwrap();
        store.pause_job(&job.id, Some("testing")).unwrap();
        assert!(store.get_due_jobs().unwrap().is_empty());
    }

    // -------------------------------------------------------------------
    // Pause / resume / trigger / resolve
    // -------------------------------------------------------------------

    #[test]
    fn pause_resume_trigger_lifecycle() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("p"), "every 5m", CreateJobOptions::default())
            .unwrap();
        let paused = store.pause_job(&job.id, Some("vacation")).unwrap().unwrap();
        assert!(!paused.enabled);
        assert_eq!(paused.state, "paused");
        assert!(paused.paused_at.is_some());
        assert_eq!(paused.paused_reason.as_deref(), Some("vacation"));

        let resumed = store.resume_job(&job.id).unwrap().unwrap();
        assert!(resumed.enabled);
        assert_eq!(resumed.state, "scheduled");
        assert!(resumed.paused_at.is_none());
        assert!(resumed.paused_reason.is_none());
        let next = ensure_aware_str(resumed.next_run_at.as_deref().unwrap()).unwrap();
        assert!(next > time_now(), "resume recomputes a FUTURE next run");

        let triggered = store.trigger_job(&job.id).unwrap().unwrap();
        let next = ensure_aware_str(triggered.next_run_at.as_deref().unwrap()).unwrap();
        assert!((time_now() - next).num_seconds() < 5, "trigger sets next_run_at=now");

        // Unknown refs are None, not errors.
        assert!(store.pause_job("missing", None).unwrap().is_none());
    }

    #[test]
    fn resume_rejects_expired_oneshot() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("p"), "30m", CreateJobOptions::default())
            .unwrap();
        // Rewrite the run_at into the stale past, as if time had passed.
        let mut stored = store.get_job(&job.id).unwrap().unwrap();
        stored.schedule.run_at = Some(fmt_isoformat(&(time_now() - Duration::hours(2))));
        store.save(&[stored]).unwrap();
        let err = store.resume_job(&job.id).unwrap_err().to_string();
        assert!(err.starts_with("Cannot resume: one-shot time "), "{err}");
        assert!(
            err.ends_with("is in the past (grace window: 120s) and will never fire."),
            "{err}"
        );
    }

    #[test]
    fn resolve_by_name_and_ambiguity() {
        let (_tmp, store) = store();
        let a = store
            .create_job(
                Some("p"),
                "every 5m",
                CreateJobOptions {
                    name: Some("Morning Digest".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        // Case-insensitive name match.
        let found = store.resolve_job_ref("morning digest").unwrap().unwrap();
        assert_eq!(found.id, a.id);
        // Exact id still wins.
        assert_eq!(store.resolve_job_ref(&a.id).unwrap().unwrap().id, a.id);
        // Duplicate names are ambiguous with the upstream error text.
        let b = store
            .create_job(
                Some("p"),
                "every 5m",
                CreateJobOptions {
                    name: Some("Morning Digest".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let err = store.resolve_job_ref("Morning Digest").unwrap_err().to_string();
        assert_eq!(
            err,
            format!(
                "Job name 'Morning Digest' is ambiguous — matches 2 jobs: {}, {}. Use the job ID instead.",
                a.id, b.id
            )
        );
    }

    // -------------------------------------------------------------------
    // mark_job_run edge semantics
    // -------------------------------------------------------------------

    #[test]
    fn recurring_uncomputable_next_run_is_error_not_disabled() {
        let (_tmp, store) = store();
        let mut job = store
            .create_job(Some("p"), "every 5m", CreateJobOptions::default())
            .unwrap();
        // Break the schedule: interval without minutes cannot compute.
        job.schedule.minutes = None;
        store.save(&[job.clone()]).unwrap();
        store.mark_job_run(&job.id, true, None, None).unwrap();
        let stored = store.get_job(&job.id).unwrap().unwrap();
        assert!(stored.enabled, "recurring jobs are NEVER auto-disabled");
        assert_eq!(stored.state, "error");
        assert!(stored.last_error.is_some());
        assert_eq!(stored.last_status.as_deref(), Some("ok"));
    }

    #[test]
    fn mark_job_run_stamps_and_reanchors() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("p"), "every 30m", CreateJobOptions::default())
            .unwrap();
        store
            .mark_job_run(&job.id, false, Some("BoomError: it broke"), None)
            .unwrap();
        let stored = store.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("error"));
        assert_eq!(stored.last_error.as_deref(), Some("BoomError: it broke"));
        let last_run = ensure_aware_str(stored.last_run_at.as_deref().unwrap()).unwrap();
        // Interval next run re-anchors on the completion time (#21).
        let next = ensure_aware_str(stored.next_run_at.as_deref().unwrap()).unwrap();
        assert_eq!(next.timestamp(), (last_run + Duration::minutes(30)).timestamp());
        assert_eq!(stored.repeat.as_ref().unwrap().completed, 1);
        // A later success clears last_error.
        store.mark_job_run(&job.id, true, None, None).unwrap();
        let stored = store.get_job(&job.id).unwrap().unwrap();
        assert_eq!(stored.last_status.as_deref(), Some("ok"));
        assert!(stored.last_error.is_none());
    }

    // -------------------------------------------------------------------
    // Output files, retention, path safety
    // -------------------------------------------------------------------

    #[test]
    fn output_file_naming_and_modes() {
        let (_tmp, store) = store();
        let path = store.save_job_output("job00000001", "# doc\n").unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let re = Regex::new(r"^\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}\.md$").unwrap();
        assert!(re.is_match(&name), "{name}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# doc\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let dmode = std::fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dmode & 0o777, 0o700);
        }
    }

    #[test]
    fn output_retention_prunes_oldest() {
        let (_tmp, store) = store();
        let dir = store.job_output_dir("keepme000001").unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..7 {
            std::fs::write(dir.join(format!("2026-07-0{}_00-00-00.md", i + 1)), "x").unwrap();
        }
        std::fs::write(dir.join("notes.txt"), "untouched").unwrap();
        assert_eq!(prune_job_output(&dir, 3), 4);
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "2026-07-05_00-00-00.md",
                "2026-07-06_00-00-00.md",
                "2026-07-07_00-00-00.md",
                "notes.txt"
            ]
        );
        // Non-positive keep disables pruning.
        assert_eq!(prune_job_output(&dir, 0), 0);
    }

    #[test]
    fn job_id_path_safety() {
        let (_tmp, store) = store();
        for bad in ["../escape", "a/b", "a\\b", "..", ".", "", "/abs"] {
            let err = store.job_output_dir(bad).unwrap_err().to_string();
            assert_eq!(err, format!("Invalid cron job id for output path: '{}'", bad));
            assert!(store.save_job_output(bad, "x").is_err());
        }
        // A legacy unsafe id fails closed in remove_job WITHOUT removing.
        write_raw(
            &store,
            r#"{"jobs": [{"id": "../evil", "name": "evil", "prompt": "p", "schedule": {"kind": "interval", "minutes": 5}}], "updated_at": "x"}"#,
        );
        assert!(store.remove_job("../evil").is_err());
        assert_eq!(store.load().unwrap().len(), 1, "removal not half-applied");
    }

    #[test]
    fn remove_job_deletes_output_dir() {
        let (_tmp, store) = store();
        let job = store
            .create_job(Some("p"), "every 5m", CreateJobOptions::default())
            .unwrap();
        let out = store.save_job_output(&job.id, "content").unwrap();
        assert!(out.exists());
        assert!(store.remove_job(&job.id).unwrap());
        assert!(!store.job_output_dir(&job.id).unwrap().exists());
        assert!(store.load().unwrap().is_empty());
        assert!(!store.remove_job(&job.id).unwrap());
    }

    // -------------------------------------------------------------------
    // Heartbeat
    // -------------------------------------------------------------------

    #[test]
    fn heartbeat_files() {
        let (_tmp, store) = store();
        assert!(store.get_ticker_heartbeat_age().is_none());
        store.record_ticker_heartbeat(false);
        assert!(store.get_ticker_heartbeat_age().unwrap() < 5.0);
        assert!(store.get_ticker_success_age().is_none());
        store.record_ticker_heartbeat(true);
        assert!(store.get_ticker_success_age().unwrap() < 5.0);
        let raw = std::fs::read_to_string(store.dir().join("ticker_heartbeat")).unwrap();
        assert!(raw.trim().parse::<f64>().is_ok());
    }
}
