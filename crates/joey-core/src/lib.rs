//! `joey-core` — the shared foundation for joey-agent.
//!
//! joey-agent is a Rust port of Hermes Agent (Nous Research, MIT). This crate
//! owns the brand, path/profile resolution, layered config, the timezone clock,
//! logging, secret redaction, reasoning-effort parsing, and the SQLite session
//! store — everything the higher crates build on.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use once_cell::sync::Lazy;

pub mod auth_store;
pub mod branding;
pub mod config;
pub mod constants;
pub mod default_soul;
pub mod logging;
pub mod reasoning;
pub mod redact;
pub mod state;
pub mod time;
pub mod utils;

pub use config::Config;
pub use constants::{joey_home, user_home_dir};
pub use state::{CompressionCooldown, Role, Session, SessionDb, StoredMessage};

/// Home paths whose directory skeleton has been created this process
/// (upstream `_HERMES_HOME_ENSURED`). Only successful passes are recorded.
static HOME_ENSURED: Lazy<Mutex<HashSet<PathBuf>>> = Lazy::new(|| Mutex::new(HashSet::new()));

fn secure_dir(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

fn secure_file(path: &std::path::Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// Seed a default SOUL.md into the joey home, upgrading legacy empty
/// templates in place. A SOUL.md the user customized is never touched
/// (port of `_ensure_default_soul_md`, config.py:895-913).
fn ensure_default_soul_md(home: &std::path::Path) {
    let soul_path = home.join("SOUL.md");
    if soul_path.exists() {
        let existing = match std::fs::read_to_string(&soul_path) {
            Ok(t) => t,
            Err(_) => return,
        };
        if !default_soul::is_legacy_template_soul(&existing) {
            return;
        }
        // Legacy empty template → upgrade to the real default in place.
    }
    if std::fs::write(&soul_path, default_soul::DEFAULT_SOUL_MD).is_ok() {
        secure_file(&soul_path);
    }
}

/// Ensure the joey home directory structure exists with secure permissions.
/// Mirrors upstream `ensure_hermes_home()` (config.py:922-969): the exact
/// subdirectory skeleton, 0700 modes, first-run SOUL.md seeding, the
/// named-profile guard, and per-home memoization.
pub fn ensure_home() -> anyhow::Result<std::path::PathBuf> {
    let home = joey_home();

    {
        let ensured = HOME_ENSURED.lock().expect("home ensured lock");
        if ensured.contains(&home) && home.is_dir() {
            return Ok(home);
        }
    }

    // Named profiles must be created explicitly (e.g. `joey profile create`).
    // Silently mkdir-ing a renamed/deleted profile home would resurrect an
    // empty skeleton and make the deleted profile reappear.
    if home
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n == "profiles")
        .unwrap_or(false)
        && !home.exists()
    {
        anyhow::bail!(
            "Named profile home does not exist: {}. Create the profile explicitly before using it.",
            home.display()
        );
    }

    std::fs::create_dir_all(&home)?;
    secure_dir(&home);
    for subdir in [
        "cron",
        "sessions",
        "logs",
        "logs/curator",
        "memories",
        "pairing",
        "hooks",
        "image_cache",
        "audio_cache",
        "skills",
    ] {
        let d = home.join(subdir);
        std::fs::create_dir_all(&d)?;
        secure_dir(&d);
    }
    ensure_default_soul_md(&home);

    HOME_ENSURED
        .lock()
        .expect("home ensured lock")
        .insert(home.clone());
    Ok(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that install a process-global home override
    /// (crate-wide — see `constants::TEST_HOME_OVERRIDE_LOCK`).
    use constants::TEST_HOME_OVERRIDE_LOCK as OVERRIDE_LOCK;

    #[test]
    fn ensure_home_skeleton_and_soul() {
        let _lock = OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("joey-home");
        let _guard = constants::HomeOverrideGuard::new(home.clone());

        let created = ensure_home().unwrap();
        assert_eq!(created, home);
        for sub in [
            "cron", "sessions", "logs", "logs/curator", "memories", "pairing", "hooks",
            "image_cache", "audio_cache", "skills",
        ] {
            assert!(home.join(sub).is_dir(), "missing {}", sub);
        }
        // Invented extras must NOT exist.
        for absent in ["plugins", "cache", "pending", "cron/output"] {
            assert!(!home.join(absent).exists(), "{} should not be created", absent);
        }
        // SOUL.md seeded with the default persona.
        let soul = std::fs::read_to_string(home.join("SOUL.md")).unwrap();
        assert_eq!(soul, default_soul::DEFAULT_SOUL_MD);

        // A customized SOUL.md is never touched.
        std::fs::write(home.join("SOUL.md"), "You are a pirate.").unwrap();
        // Bust the memoization by removing + re-adding a dir? ensure_home is
        // memoized per path — simulate a fresh process by clearing the set.
        HOME_ENSURED.lock().unwrap().clear();
        ensure_home().unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("SOUL.md")).unwrap(),
            "You are a pirate."
        );

        // A legacy template soul IS upgraded in place.
        let legacy = "# Hermes Agent Persona\n\n<!--\nThis file defines the agent's personality and tone.\nThe agent will embody whatever you write here.\nEdit this to customize how Hermes communicates with you.\n\nThis file is loaded fresh each message -- no restart needed.\nDelete the contents (or this file) to use the default personality.\n-->";
        std::fs::write(home.join("SOUL.md"), legacy).unwrap();
        HOME_ENSURED.lock().unwrap().clear();
        ensure_home().unwrap();
        assert_eq!(
            std::fs::read_to_string(home.join("SOUL.md")).unwrap(),
            default_soul::DEFAULT_SOUL_MD
        );
    }

    #[test]
    fn named_profile_guard() {
        let _lock = OVERRIDE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::tempdir().unwrap();
        let missing_profile = dir.path().join("profiles").join("work");
        let _guard = constants::HomeOverrideGuard::new(missing_profile.clone());
        let err = ensure_home().unwrap_err().to_string();
        assert!(err.contains("Named profile home does not exist"), "{}", err);

        // An EXISTING named profile home is fine.
        std::fs::create_dir_all(&missing_profile).unwrap();
        assert!(ensure_home().is_ok());
    }
}
