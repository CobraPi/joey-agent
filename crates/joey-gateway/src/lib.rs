//! `joey-gateway` — the messaging gateway core (port of `gateway/` core).
//!
//! This crate provides the platform-neutral spine: the session-key builder, the
//! normalized inbound/outbound message types, and the `PlatformAdapter` trait
//! every platform (Telegram, Discord, Slack, …) implements. Concrete platform
//! adapters are added incrementally behind this trait; none ship in this first
//! port, matching the deferral plan.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Identity + routing info for an inbound message (port of `SessionSource`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSource {
    pub platform: String,
    pub chat_id: String,
    /// "dm" | "group" | "channel" | "thread"
    pub chat_type: String,
    pub user_id: String,
    #[serde(default)]
    pub user_name: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

/// Options that affect session-key derivation.
#[derive(Debug, Clone, Copy)]
pub struct SessionKeyOptions {
    pub group_sessions_per_user: bool,
    pub thread_sessions_per_user: bool,
}

impl Default for SessionKeyOptions {
    fn default() -> Self {
        Self {
            group_sessions_per_user: true,
            thread_sessions_per_user: false,
        }
    }
}

/// Build the routing session key for a source (port of `gateway/session.py::
/// build_session_key`). Format:
/// `agent:<ns>:<platform>:<chat_type>[:<chat_id>][:<thread_id>][:<user_id>]`.
pub fn build_session_key(source: &SessionSource, opts: SessionKeyOptions) -> String {
    let ns = match source.profile.as_deref() {
        Some(p) if !p.is_empty() && p != "default" => p,
        _ => "main",
    };
    let mut key = format!("agent:{}:{}:{}", ns, source.platform, source.chat_type);

    match source.chat_type.as_str() {
        "dm" => {
            key.push(':');
            key.push_str(&source.chat_id);
        }
        "thread" => {
            key.push(':');
            key.push_str(&source.chat_id);
            if let Some(tid) = &source.thread_id {
                key.push(':');
                key.push_str(tid);
            }
            if opts.thread_sessions_per_user {
                key.push(':');
                key.push_str(&source.user_id);
            }
        }
        // group / channel
        _ => {
            key.push(':');
            key.push_str(&source.chat_id);
            if opts.group_sessions_per_user {
                key.push(':');
                key.push_str(&source.user_id);
            }
        }
    }
    key
}

/// A normalized inbound message (port of `MessageEvent`).
#[derive(Debug, Clone)]
pub struct MessageEvent {
    pub text: String,
    pub source: SessionSource,
    pub message_id: Option<String>,
}

/// The result of a send (port of `SendResult`).
#[derive(Debug, Clone)]
pub struct SendResult {
    pub success: bool,
    pub message_id: Option<String>,
    pub error: Option<String>,
}

impl SendResult {
    pub fn ok(message_id: Option<String>) -> Self {
        Self { success: true, message_id, error: None }
    }
    pub fn err(error: impl Into<String>) -> Self {
        Self { success: false, message_id: None, error: Some(error.into()) }
    }
}

/// A messaging platform adapter (port of `BasePlatformAdapter`).
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// The platform name (e.g. `telegram`).
    fn platform(&self) -> &str;
    /// Connect / start receiving.
    async fn connect(&self) -> anyhow::Result<()>;
    /// Send a message to a chat.
    async fn send(&self, chat_id: &str, content: &str) -> SendResult;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(chat_type: &str) -> SessionSource {
        SessionSource {
            platform: "telegram".into(),
            chat_id: "12345".into(),
            chat_type: chat_type.into(),
            user_id: "user99".into(),
            user_name: "joey".into(),
            thread_id: None,
            profile: None,
        }
    }

    #[test]
    fn dm_key_isolates_per_chat() {
        let key = build_session_key(&src("dm"), SessionKeyOptions::default());
        assert_eq!(key, "agent:main:telegram:dm:12345");
    }

    #[test]
    fn group_key_isolates_per_user_by_default() {
        let key = build_session_key(&src("group"), SessionKeyOptions::default());
        assert_eq!(key, "agent:main:telegram:group:12345:user99");
    }

    #[test]
    fn profile_changes_namespace() {
        let mut s = src("dm");
        s.profile = Some("coder".into());
        let key = build_session_key(&s, SessionKeyOptions::default());
        assert_eq!(key, "agent:coder:telegram:dm:12345");
    }
}
