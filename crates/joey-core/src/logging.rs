//! Logging setup (port of upstream `hermes_logging.py`).
//!
//! Writes rotating logs under `~/.joey/logs/` and honors the `JOEY_LOG`
//! env-filter (like `RUST_LOG`). Secret redaction is applied by callers that
//! log tool output via [`crate::redact`].

use std::path::PathBuf;

use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

use crate::{branding, constants};

/// The directory logs are written to (`~/.joey/logs`).
pub fn logs_dir() -> PathBuf {
    constants::joey_home().join("logs")
}

/// Initialize logging. Console output goes to stderr (so it never corrupts a
/// stdout protocol stream); a non-blocking file appender writes `agent.log`.
/// Returns the appender guard, which must be kept alive for the process.
pub fn init(component: &str) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let dir = logs_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        // Still set up console logging even if the log dir is unavailable.
        let _ = tracing_subscriber::registry()
            .with(env_filter())
            .with(fmt::layer().with_writer(std::io::stderr).with_ansi(false))
            .try_init();
        return None;
    }

    let file_appender = tracing_appender::rolling::daily(&dir, format!("{}.log", component));
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let file_layer = fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true);

    let console_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_ansi(atty_stderr());

    let _ = tracing_subscriber::registry()
        .with(env_filter())
        .with(file_layer)
        .with(console_layer)
        .try_init();

    tracing::debug!("{} logging initialized ({})", branding::AGENT_NAME, component);
    Some(guard)
}

fn env_filter() -> EnvFilter {
    // JOEY_LOG first, then RUST_LOG, else a quiet default.
    EnvFilter::try_from_env(branding::ENV_LOG)
        .or_else(|_| EnvFilter::try_from_default_env())
        .unwrap_or_else(|_| EnvFilter::new("warn,joey=info"))
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
