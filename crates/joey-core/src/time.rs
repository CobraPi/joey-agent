//! Timezone-aware clock (port of upstream `hermes_time.py`).
//!
//! Resolution order for the configured zone:
//!   1. `JOEY_TIMEZONE` env var
//!   2. `timezone` key in `~/.joey/config.yaml`
//!   3. server local time
//!
//! Invalid timezone strings warn and fall back — the clock never panics.

use chrono::{DateTime, Local, Utc};
use chrono_tz::Tz;

use crate::config::Config;

/// Current time as a fixed-offset datetime in the configured zone, or the
/// server's local zone when none is configured/valid.
pub fn now() -> DateTime<chrono::FixedOffset> {
    match configured_tz() {
        Some(tz) => Utc::now().with_timezone(&tz).fixed_offset(),
        None => Local::now().fixed_offset(),
    }
}

/// ISO-8601 timestamp string for the current instant in the configured zone.
pub fn now_iso() -> String {
    now().to_rfc3339()
}

/// Resolve the configured IANA timezone, if any and valid.
pub fn configured_tz() -> Option<Tz> {
    let name = timezone_name()?;
    match name.parse::<Tz>() {
        Ok(tz) => Some(tz),
        Err(_) => {
            tracing::warn!(
                "Invalid timezone '{}'. Falling back to server local time.",
                name
            );
            None
        }
    }
}

fn timezone_name() -> Option<String> {
    if let Ok(env) = std::env::var("JOEY_TIMEZONE") {
        let t = env.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    // config.yaml — best-effort; time is not on any hard failure path.
    let cfg = Config::load().ok()?;
    let tz = cfg.get_str("timezone", "");
    (!tz.trim().is_empty()).then(|| tz.trim().to_string())
}
