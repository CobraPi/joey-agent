//! Timezone-aware clock (port of upstream `hermes_time.py`).
//!
//! Resolution order for the configured zone:
//!   1. `JOEY_TIMEZONE` env var
//!   2. `timezone` key in `~/.joey/config.yaml` (raw read — no config merge)
//!   3. server local time
//!
//! The resolved zone is cached once per process (upstream `_cached_tz`);
//! call [`reset_cache`] after a config/env change to force re-resolution.
//! Invalid timezone strings warn and fall back — the clock never panics.

use std::sync::RwLock;

use chrono::{DateTime, Local, Utc};
use chrono_tz::Tz;

use crate::constants;

/// `None` = not yet resolved; `Some(None)` = resolved to server-local;
/// `Some(Some(tz))` = resolved to a configured zone.
static CACHED_TZ: RwLock<Option<Option<Tz>>> = RwLock::new(None);

/// Current time as a fixed-offset datetime in the configured zone, or the
/// server's local zone when none is configured/valid.
pub fn now() -> DateTime<chrono::FixedOffset> {
    match get_timezone() {
        Some(tz) => Utc::now().with_timezone(&tz).fixed_offset(),
        None => Local::now().fixed_offset(),
    }
}

/// ISO-8601 timestamp for the current instant, in Python
/// `datetime.isoformat()` shape: `YYYY-MM-DDTHH:MM:SS.ffffff+HH:MM`
/// (6-digit microseconds, colon in the offset).
pub fn now_iso() -> String {
    now().format("%Y-%m-%dT%H:%M:%S%.6f%:z").to_string()
}

/// Return the configured timezone, or `None` (meaning server-local).
/// Resolved once and cached; call [`reset_cache`] after config changes.
pub fn get_timezone() -> Option<Tz> {
    if let Some(resolved) = *CACHED_TZ.read().expect("tz cache lock") {
        return resolved;
    }
    let resolved = resolve_timezone();
    *CACHED_TZ.write().expect("tz cache lock") = Some(resolved);
    resolved
}

/// Clear the cached timezone so the next call re-resolves it.
pub fn reset_cache() {
    *CACHED_TZ.write().expect("tz cache lock") = None;
}

/// Backwards-compatible alias for [`get_timezone`].
pub fn configured_tz() -> Option<Tz> {
    get_timezone()
}

fn resolve_timezone() -> Option<Tz> {
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

/// Read the configured IANA timezone string (env first, then a RAW read of
/// `config.yaml` — deliberately not `Config::load`, which would recurse
/// into .env loading and the full merge pipeline on every cache refill).
fn timezone_name() -> Option<String> {
    if let Ok(env) = std::env::var("JOEY_TIMEZONE") {
        let t = env.trim();
        if !t.is_empty() {
            return Some(t.to_string());
        }
    }
    let config_path = constants::config_path();
    let text = std::fs::read_to_string(config_path).ok()?;
    let cfg: serde_yaml::Value = serde_yaml::from_str(&text).ok()?;
    let tz = cfg.get("timezone")?.as_str()?.trim().to_string();
    (!tz.is_empty()).then_some(tz)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_shape_has_six_digit_micros_and_colon_offset() {
        let s = now_iso();
        // e.g. 2026-07-21T12:34:56.123456+02:00
        let re = regex::Regex::new(
            r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{6}[+-]\d{2}:\d{2}$",
        )
        .unwrap();
        assert!(re.is_match(&s), "bad isoformat shape: {}", s);
    }
}
