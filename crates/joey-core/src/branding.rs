//! Single source of truth for the joey-agent brand.
//!
//! joey-agent is a Rust port of Hermes Agent (Nous Research, MIT). Every
//! user-visible name, path, and env-var prefix flows through here so the
//! brand never leaks piecemeal into the rest of the codebase.

/// Human-readable agent name.
pub const AGENT_NAME: &str = "Joey Agent";
/// Package / repo name.
pub const PACKAGE_NAME: &str = "joey-agent";
/// The main CLI binary name.
pub const CLI_NAME: &str = "joey";
/// Environment-variable prefix (`JOEY_HOME`, `JOEY_LOG`, ...).
pub const ENV_PREFIX: &str = "JOEY_";
/// Dot-directory name under `$HOME` on POSIX (`~/.joey`).
pub const HOME_DIR_NAME: &str = ".joey";
/// Directory name under `%LOCALAPPDATA%` on Windows.
pub const WINDOWS_DIR_NAME: &str = "joey";
/// Toolset name prefix (`joey-cli`, `joey-telegram`, ...). Upstream: `hermes-*`.
pub const TOOLSET_PREFIX: &str = "joey-";
/// Current version (kept in lockstep with the upstream baseline we ported).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Upstream project this port derives from (MIT — attribution retained).
pub const UPSTREAM_ATTRIBUTION: &str =
    "Rust port of Hermes Agent by Nous Research (https://github.com/NousResearch/hermes-agent, MIT)";

/// `JOEY_HOME` — overrides the state directory location.
pub const ENV_HOME: &str = "JOEY_HOME";
/// `JOEY_REAL_HOME` — explicit OS-user home override for subprocesses.
pub const ENV_REAL_HOME: &str = "JOEY_REAL_HOME";
/// `JOEY_LOG` — tracing filter (like `RUST_LOG`).
pub const ENV_LOG: &str = "JOEY_LOG";
/// `JOEY_OPTIONAL_SKILLS` — packaged optional-skills dir override.
pub const ENV_OPTIONAL_SKILLS: &str = "JOEY_OPTIONAL_SKILLS";
/// `JOEY_BUNDLED_SKILLS` — packaged bundled-skills dir override.
pub const ENV_BUNDLED_SKILLS: &str = "JOEY_BUNDLED_SKILLS";
/// `JOEY_OPTIONAL_MCPS` — packaged optional-mcps dir override.
pub const ENV_OPTIONAL_MCPS: &str = "JOEY_OPTIONAL_MCPS";
