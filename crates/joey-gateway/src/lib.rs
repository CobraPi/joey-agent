//! `joey-gateway` — the messaging gateway core (port of `gateway/` core).
//!
//! This crate provides the platform-neutral spine, mirroring the upstream
//! module layout:
//!
//! - [`config`] — the [`Platform`] enum (upstream `gateway/config.py`);
//! - [`whatsapp_identity`] — WhatsApp JID/LID canonicalisation (upstream
//!   `gateway/whatsapp_identity.py`);
//! - [`session`] — [`SessionSource`] and the session-key builder (upstream
//!   `gateway/session.py`);
//! - [`base`] — [`MessageEvent`], [`SendResult`], send-error classification,
//!   message truncation, and the [`PlatformAdapter`] trait (upstream
//!   `gateway/platforms/base.py::BasePlatformAdapter`).
//!
//! Concrete platform adapters (Telegram, Discord, Slack, …) are added
//! incrementally behind the trait; none ship in this first port, matching
//! the deferral plan.

pub mod base;
pub mod config;
pub mod session;
pub mod whatsapp_identity;

pub use base::{
    classify_send_error, is_chat_level_not_found, truncate_message, utf16_len, AutoSkill,
    MessageEvent, MessageType, PlatformAdapter, SendResult, RETRYABLE_ERROR_PATTERNS,
    SEND_ERROR_KINDS, TRUNCATE_DEFAULT_MAX_LENGTH,
};
pub use config::Platform;
pub use session::{
    build_session_key, build_session_key_with_defaults, SessionKeyOptions, SessionSource,
};
pub use whatsapp_identity::{
    canonical_whatsapp_identifier, expand_whatsapp_aliases, normalize_whatsapp_identifier,
};

#[cfg(test)]
pub(crate) mod testutil {
    use std::sync::{Mutex, MutexGuard};

    /// Tests that override the joey home (WhatsApp lid-mapping lookups) share
    /// process-global state; serialize them so parallel test threads can't
    /// observe each other's temporary homes.
    static HOME_LOCK: Mutex<()> = Mutex::new(());

    pub fn lock_home() -> MutexGuard<'static, ()> {
        HOME_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
