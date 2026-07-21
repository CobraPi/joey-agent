//! `joey-core` — the shared foundation for joey-agent.
//!
//! joey-agent is a Rust port of Hermes Agent (Nous Research, MIT). This crate
//! owns the brand, path/profile resolution, layered config, the timezone clock,
//! logging, secret redaction, reasoning-effort parsing, and the SQLite session
//! store — everything the higher crates build on.

pub mod branding;
pub mod config;
pub mod constants;
pub mod logging;
pub mod reasoning;
pub mod redact;
pub mod state;
pub mod time;
pub mod utils;

pub use config::Config;
pub use constants::{joey_home, user_home_dir};
pub use state::{Role, Session, SessionDb, StoredMessage};

/// Ensure the joey home directory and its standard subdirectories exist.
/// Mirrors upstream `ensure_hermes_home()`.
pub fn ensure_home() -> anyhow::Result<std::path::PathBuf> {
    let home = joey_home();
    for sub in [
        "", "logs", "cron", "cron/output", "sessions", "memories", "skills", "plugins",
        "cache", "audio_cache", "pending",
    ] {
        let dir = if sub.is_empty() { home.clone() } else { home.join(sub) };
        std::fs::create_dir_all(&dir)?;
    }
    constants::secure_parent_dir(&home.join("x"));
    Ok(home)
}
