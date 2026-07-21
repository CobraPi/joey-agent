//! Cron job storage + schedule parsing (port of `cron/jobs.py`).
//!
//! Jobs live at `~/.joey/cron/jobs.json` (a JSON array). Output is written to
//! `~/.joey/cron/output/<job_id>/<timestamp>.md`.

use std::path::PathBuf;
use std::str::FromStr;

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Ticker interval (upstream `TICKER_INTERVAL_SECONDS`).
pub const TICKER_INTERVAL_SECONDS: u64 = 60;

/// The parsed schedule for a job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Schedule {
    /// Run once at a specific instant.
    Once { run_at: DateTime<Utc> },
    /// Run every N minutes.
    Interval { minutes: i64 },
    /// Run on a cron expression.
    Cron { expr: String },
}

/// A scheduled job.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub schedule: Schedule,
    #[serde(default)]
    pub schedule_display: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_scheduled")]
    pub state: String,
    #[serde(default)]
    pub deliver: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub next_run_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_status: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// {times, completed} — bounded repeats; None = unbounded.
    #[serde(default)]
    pub repeat_times: Option<u64>,
    #[serde(default)]
    pub repeat_completed: u64,
}

fn default_true() -> bool {
    true
}
fn default_scheduled() -> String {
    "scheduled".to_string()
}

impl Job {
    /// Whether this job is due to run at `now`.
    pub fn is_due(&self, now: DateTime<Utc>) -> bool {
        if !self.enabled || self.state == "paused" {
            return false;
        }
        match self.next_run_at {
            Some(next) => now >= next,
            None => true,
        }
    }
}

/// The cron store rooted at the active profile's home.
pub struct CronStore {
    dir: PathBuf,
}

impl CronStore {
    pub fn open_default() -> Self {
        Self {
            dir: joey_core::constants::joey_home().join("cron"),
        }
    }

    pub fn with_dir(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn jobs_file(&self) -> PathBuf {
        self.dir.join("jobs.json")
    }

    pub fn output_dir(&self, job_id: &str) -> PathBuf {
        self.dir.join("output").join(job_id)
    }

    /// Load all jobs (empty vec when the file is absent).
    pub fn load(&self) -> Result<Vec<Job>> {
        let path = self.jobs_file();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        Ok(serde_json::from_str(&text).with_context(|| "parsing jobs.json")?)
    }

    /// Persist all jobs atomically.
    pub fn save(&self, jobs: &[Job]) -> Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        joey_core::utils::atomic_json_write(&self.jobs_file(), &jobs)?;
        Ok(())
    }

    /// Add a job and persist.
    pub fn add(&self, job: Job) -> Result<()> {
        let mut jobs = self.load()?;
        jobs.push(job);
        self.save(&jobs)
    }

    /// Remove a job by id; returns whether it existed.
    pub fn remove(&self, id: &str) -> Result<bool> {
        let mut jobs = self.load()?;
        let before = jobs.len();
        jobs.retain(|j| j.id != id);
        let removed = jobs.len() != before;
        if removed {
            self.save(&jobs)?;
        }
        Ok(removed)
    }

    /// Set the enabled/paused state of a job.
    pub fn set_enabled(&self, id: &str, enabled: bool) -> Result<bool> {
        let mut jobs = self.load()?;
        let mut found = false;
        for j in jobs.iter_mut() {
            if j.id == id {
                j.enabled = enabled;
                j.state = if enabled { "scheduled" } else { "paused" }.to_string();
                found = true;
            }
        }
        if found {
            self.save(&jobs)?;
        }
        Ok(found)
    }

    /// Write a job's run output to disk and return the path.
    pub fn write_output(&self, job_id: &str, timestamp: &str, content: &str) -> Result<PathBuf> {
        let dir = self.output_dir(job_id);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.md", timestamp));
        joey_core::utils::atomic_replace(&path, content.as_bytes())?;
        Ok(path)
    }
}

/// Generate a new job id (uuid4 hex, first 12 chars — matches upstream shape).
pub fn new_job_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

/// Parse a schedule string into a [`Schedule`].
///
/// Accepts:
/// - `"30m"`, `"2h"`, `"1d"` → run once after that duration
/// - `"every 30m"`, `"every 2h"` → interval
/// - a 5/6-field cron expression → cron
/// - an ISO-8601 timestamp → run once at that instant
pub fn parse_schedule(input: &str, now: DateTime<Utc>) -> Result<Schedule> {
    let s = input.trim();
    if s.is_empty() {
        anyhow::bail!("empty schedule");
    }

    // "every <dur>" → interval
    if let Some(rest) = s.strip_prefix("every ") {
        let minutes = parse_duration_minutes(rest.trim())?;
        return Ok(Schedule::Interval { minutes });
    }

    // Bare duration → once
    if let Ok(minutes) = parse_duration_minutes(s) {
        return Ok(Schedule::Once {
            run_at: now + Duration::minutes(minutes),
        });
    }

    // ISO timestamp → once
    if s.contains('T') || s.chars().take(4).all(|c| c.is_ascii_digit()) {
        if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
            return Ok(Schedule::Once {
                run_at: dt.with_timezone(&Utc),
            });
        }
    }

    // Cron expression (5 or 6 fields of [0-9*\-,/])
    let fields: Vec<&str> = s.split_whitespace().collect();
    if (5..=6).contains(&fields.len())
        && fields
            .iter()
            .all(|f| f.chars().all(|c| c.is_ascii_digit() || "*-,/".contains(c)))
    {
        // Validate with the cron crate (it wants a 6/7-field seconds form; we
        // prepend a seconds field of 0 for a standard 5-field expression).
        let candidate = if fields.len() == 5 {
            format!("0 {}", s)
        } else {
            s.to_string()
        };
        cron::Schedule::from_str(&candidate)
            .with_context(|| format!("invalid cron expression: {}", s))?;
        return Ok(Schedule::Cron { expr: s.to_string() });
    }

    anyhow::bail!("unrecognized schedule: {}", s)
}

fn parse_duration_minutes(s: &str) -> Result<i64> {
    let s = s.trim();
    if s.len() < 2 {
        anyhow::bail!("bad duration: {}", s);
    }
    let (num, unit) = s.split_at(s.len() - 1);
    let n: i64 = num.parse().with_context(|| format!("bad duration number: {}", s))?;
    let minutes = match unit {
        "m" => n,
        "h" => n * 60,
        "d" => n * 60 * 24,
        _ => anyhow::bail!("bad duration unit: {}", unit),
    };
    Ok(minutes)
}

/// Compute the next run instant for a schedule, given the last run time.
pub fn compute_next_run(schedule: &Schedule, last_run: Option<DateTime<Utc>>, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    match schedule {
        Schedule::Once { run_at } => {
            // Once-jobs don't reschedule after they run.
            if last_run.is_some() {
                None
            } else {
                Some(*run_at)
            }
        }
        Schedule::Interval { minutes } => {
            let base = last_run.unwrap_or(now);
            Some(base + Duration::minutes(*minutes))
        }
        Schedule::Cron { expr } => {
            let candidate = if expr.split_whitespace().count() == 5 {
                format!("0 {}", expr)
            } else {
                expr.clone()
            };
            let sched = cron::Schedule::from_str(&candidate).ok()?;
            let after = last_run.unwrap_or(now);
            sched.after(&after).next()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-07-21T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn parses_bare_duration_as_once() {
        let s = parse_schedule("30m", now()).unwrap();
        match s {
            Schedule::Once { run_at } => {
                assert_eq!(run_at, now() + Duration::minutes(30));
            }
            _ => panic!("expected once"),
        }
    }

    #[test]
    fn parses_every_as_interval() {
        let s = parse_schedule("every 2h", now()).unwrap();
        matches!(s, Schedule::Interval { minutes: 120 });
    }

    #[test]
    fn parses_cron_expr() {
        let s = parse_schedule("0 9 * * *", now()).unwrap();
        match &s {
            Schedule::Cron { expr } => assert_eq!(expr, "0 9 * * *"),
            _ => panic!("expected cron"),
        }
        let next = compute_next_run(&s, None, now()).unwrap();
        assert!(next > now());
    }

    #[test]
    fn interval_reschedules_from_last_run() {
        let s = Schedule::Interval { minutes: 60 };
        let last = now();
        let next = compute_next_run(&s, Some(last), now()).unwrap();
        assert_eq!(next, last + Duration::minutes(60));
    }

    #[test]
    fn store_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = CronStore::with_dir(dir.path().to_path_buf());
        let job = Job {
            id: new_job_id(),
            name: "test".into(),
            prompt: "say hi".into(),
            schedule: Schedule::Interval { minutes: 30 },
            schedule_display: "every 30m".into(),
            enabled: true,
            state: "scheduled".into(),
            deliver: "local".into(),
            created_at: "2026-07-21T12:00:00Z".into(),
            next_run_at: None,
            last_run_at: None,
            last_status: None,
            last_error: None,
            repeat_times: None,
            repeat_completed: 0,
        };
        let id = job.id.clone();
        store.add(job).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, id);
        assert!(store.remove(&id).unwrap());
        assert_eq!(store.load().unwrap().len(), 0);
    }
}
