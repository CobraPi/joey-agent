//! Logging setup (port of upstream `hermes_logging.py`).
//!
//! File policy mirrors upstream:
//!   - `~/.joey/logs/agent.log` — main activity log at `logging.level`
//!     (default INFO), size-rotated at `logging.max_size_mb` (default 5 MB)
//!     keeping `logging.backup_count` (default 3) backups `agent.log.1..N`.
//!   - `~/.joey/logs/errors.log` — WARNING+ triage log, 2 MB / 2 backups.
//!
//! Line format is the upstream `_LOG_FORMAT`:
//! `%(asctime)s %(levelname)s%(session_tag)s %(name)s: %(message)s`
//! and every formatted line passes through the redaction module before it
//! hits disk (port of `RedactingFormatter`). Console output is opt-in only
//! (`init_verbose`), never always-on.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Mutex;

use tracing::field::{Field, Visit};
use tracing::{Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::{EnvFilter, Layer};

use crate::{branding, constants, redact};

/// The directory logs are written to (`~/.joey/logs`).
pub fn logs_dir() -> PathBuf {
    constants::joey_home().join("logs")
}

thread_local! {
    static SESSION_TAG: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Set the per-thread session tag included in log lines (` [<id>]`), or
/// clear it with `None`. Port of upstream `set_session_context`.
pub fn set_session_context(session_id: Option<&str>) {
    SESSION_TAG.with(|slot| {
        *slot.borrow_mut() = session_id.map(|s| s.to_string());
    });
}

fn session_tag() -> String {
    SESSION_TAG.with(|slot| {
        slot.borrow()
            .as_deref()
            .map(|s| format!(" [{}]", s))
            .unwrap_or_default()
    })
}

/// A size-rotating log file writer: checks size on every write and cascades
/// `base.log.N-1 → base.log.N`, `base.log → base.log.1` when the write would
/// exceed `max_bytes` (port of stdlib `RotatingFileHandler` semantics).
struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    backup_count: u32,
    file: Option<File>,
    size: u64,
}

impl RotatingFile {
    fn new(path: PathBuf, max_bytes: u64, backup_count: u32) -> Self {
        Self {
            path,
            max_bytes,
            backup_count,
            file: None,
            size: 0,
        }
    }

    fn open(&mut self) -> std::io::Result<()> {
        if self.file.is_none() {
            let f = OpenOptions::new().create(true).append(true).open(&self.path)?;
            self.size = f.metadata().map(|m| m.len()).unwrap_or(0);
            self.file = Some(f);
        }
        Ok(())
    }

    fn backup_path(&self, i: u32) -> PathBuf {
        let mut os = self.path.clone().into_os_string();
        os.push(format!(".{}", i));
        PathBuf::from(os)
    }

    fn rollover(&mut self) {
        self.file = None;
        if self.backup_count > 0 {
            let _ = std::fs::remove_file(self.backup_path(self.backup_count));
            for i in (1..self.backup_count).rev() {
                let src = self.backup_path(i);
                if src.exists() {
                    let _ = std::fs::rename(&src, self.backup_path(i + 1));
                }
            }
            let _ = std::fs::rename(&self.path, self.backup_path(1));
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
        self.size = 0;
    }

    fn write_line(&mut self, line: &str) {
        if self.open().is_err() {
            return;
        }
        let bytes = line.as_bytes();
        if self.max_bytes > 0 && self.size + bytes.len() as u64 > self.max_bytes {
            self.rollover();
            if self.open().is_err() {
                return;
            }
        }
        if let Some(f) = self.file.as_mut() {
            if f.write_all(bytes).is_ok() {
                self.size += bytes.len() as u64;
            }
        }
    }
}

struct MessageVisitor {
    message: String,
    fields: String,
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        } else {
            use std::fmt::Write;
            let _ = write!(self.fields, " {}={:?}", field.name(), value);
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            use std::fmt::Write;
            let _ = write!(self.fields, " {}={}", field.name(), value);
        }
    }
}

fn level_name(level: &Level) -> &'static str {
    match *level {
        Level::TRACE => "DEBUG",
        Level::DEBUG => "DEBUG",
        Level::INFO => "INFO",
        Level::WARN => "WARNING",
        Level::ERROR => "ERROR",
    }
}

fn level_num(level: &Level) -> u8 {
    match *level {
        Level::TRACE | Level::DEBUG => 10,
        Level::INFO => 20,
        Level::WARN => 30,
        Level::ERROR => 40,
    }
}

fn parse_level_name(name: &str) -> u8 {
    match name.trim().to_uppercase().as_str() {
        "DEBUG" | "TRACE" => 10,
        "INFO" => 20,
        "WARNING" | "WARN" => 30,
        "ERROR" => 40,
        "CRITICAL" => 50,
        _ => 20,
    }
}

/// The tracing layer that owns both rotating files.
struct JoeyFileLayer {
    agent_log: Mutex<RotatingFile>,
    errors_log: Mutex<RotatingFile>,
    /// Numeric threshold for agent.log (python logging scale).
    level: u8,
}

impl<S> Layer<S> for JoeyFileLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let meta = event.metadata();
        let num = level_num(meta.level());
        if num < self.level && num < 30 {
            // Below the agent.log threshold and not a WARNING+ record.
            return;
        }

        let mut visitor = MessageVisitor {
            message: String::new(),
            fields: String::new(),
        };
        event.record(&mut visitor);

        // %(asctime)s %(levelname)s%(session_tag)s %(name)s: %(message)s
        let now = chrono::Local::now();
        let asctime = format!(
            "{},{:03}",
            now.format("%Y-%m-%d %H:%M:%S"),
            now.timestamp_subsec_millis()
        );
        let line = format!(
            "{} {}{} {}: {}{}\n",
            asctime,
            level_name(meta.level()),
            session_tag(),
            meta.target(),
            visitor.message,
            visitor.fields,
        );
        // Redact every line before it reaches disk (RedactingFormatter port).
        let line = redact::redact_sensitive_text(&line);

        if num >= self.level {
            if let Ok(mut f) = self.agent_log.lock() {
                f.write_line(&line);
            }
        }
        if num >= 30 {
            if let Ok(mut f) = self.errors_log.lock() {
                f.write_line(&line);
            }
        }
    }
}

/// Guard returned by [`init`]; kept for API stability (no background worker
/// remains, writes are synchronous like upstream's logging handlers).
pub struct LogGuard(());

fn read_logging_config() -> (u8, u64, u32) {
    // Best-effort read of logging.level / max_size_mb / backup_count.
    let (mut level, mut max_mb, mut backups) = (20u8, 5u64, 3u32);
    if let Ok(cfg) = crate::config::Config::load() {
        level = parse_level_name(&cfg.get_str("logging.level", "INFO"));
        max_mb = cfg.get_i64("logging.max_size_mb", 5).max(0) as u64;
        backups = cfg.get_i64("logging.backup_count", 3).max(0) as u32;
    }
    (level, max_mb, backups)
}

fn build_file_layer(component: &str) -> Option<JoeyFileLayer> {
    let dir = logs_dir();
    std::fs::create_dir_all(&dir).ok()?;
    let (level, max_mb, backups) = read_logging_config();
    let _ = component; // agent.log is the shared catch-all, as upstream.
    Some(JoeyFileLayer {
        agent_log: Mutex::new(RotatingFile::new(
            dir.join("agent.log"),
            max_mb * 1024 * 1024,
            backups,
        )),
        errors_log: Mutex::new(RotatingFile::new(
            dir.join("errors.log"),
            2 * 1024 * 1024,
            2,
        )),
        level,
    })
}

fn env_filter() -> EnvFilter {
    // JOEY_LOG first, then RUST_LOG, else pass everything through to the
    // file layer (which applies the configured level itself).
    EnvFilter::try_from_env(branding::ENV_LOG)
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("trace"))
}

/// Initialize logging: rotating `agent.log` + `errors.log` under
/// `~/.joey/logs`. Console output is NOT enabled here — it is opt-in via
/// [`init_verbose`] (mirrors upstream `setup_logging` / `setup_verbose_logging`).
pub fn init(component: &str) -> Option<LogGuard> {
    init_impl(component, false)
}

/// Initialize logging with an additional DEBUG console (stderr) layer —
/// the `--verbose` mode.
pub fn init_verbose(component: &str) -> Option<LogGuard> {
    init_impl(component, true)
}

fn init_impl(component: &str, verbose: bool) -> Option<LogGuard> {
    let file_layer = build_file_layer(component);

    let console_layer = if verbose {
        Some(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(atty_stderr()),
        )
    } else {
        None
    };

    let _ = tracing_subscriber::registry()
        .with(env_filter())
        .with(file_layer)
        .with(console_layer)
        .try_init();

    tracing::debug!("{} logging initialized ({})", branding::AGENT_NAME, component);
    Some(LogGuard(()))
}

fn atty_stderr() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: isatty on a stable fd number is always safe.
        unsafe { libc::isatty(libc::STDERR_FILENO) == 1 }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_cascades_backups() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("agent.log");
        let mut rf = RotatingFile::new(base.clone(), 64, 3);
        for i in 0..40 {
            rf.write_line(&format!("2026-01-01 00:00:00,000 INFO test: line {}\n", i));
        }
        assert!(base.exists());
        assert!(dir.path().join("agent.log.1").exists());
        assert!(dir.path().join("agent.log.2").exists());
        assert!(dir.path().join("agent.log.3").exists());
        assert!(!dir.path().join("agent.log.4").exists(), "backup_count respected");
        // Current file stays under the cap.
        assert!(std::fs::metadata(&base).unwrap().len() <= 64);
    }

    #[test]
    fn level_names_match_python() {
        assert_eq!(level_name(&Level::WARN), "WARNING");
        assert_eq!(level_name(&Level::INFO), "INFO");
        assert_eq!(parse_level_name("warning"), 30);
        assert_eq!(parse_level_name("bogus"), 20);
    }
}
