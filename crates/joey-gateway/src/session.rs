//! Session identity and session-key derivation (port of the relevant parts
//! of upstream `gateway/session.py`: `SessionSource` and
//! `build_session_key`).

use serde::de::Deserializer;
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

use crate::config::Platform;
use crate::whatsapp_identity::canonical_whatsapp_identifier;

/// Return `Some(&str)` only for a present, non-empty string (Python
/// truthiness for `Optional[str]` fields).
pub(crate) fn non_empty(opt: &Option<String>) -> Option<&str> {
    opt.as_deref().filter(|s| !s.is_empty())
}

/// Describes where a message originated from (port of
/// `gateway/session.py::SessionSource`).
///
/// This information is used to:
/// 1. Route responses back to the right place
/// 2. Inject context into the system prompt
/// 3. Track origin for cron job delivery
///
/// Serialization matches upstream `to_dict`/`from_dict` exactly: the first
/// eight keys (`platform` … `chat_topic`) are always emitted (with `null`
/// for absent optionals); the remaining keys are emitted only when truthy;
/// `is_bot`, `role_authorized` and `delivered_via_upstream_relay` are never
/// serialized (and never restored from a dict).
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSource {
    pub platform: Platform,
    pub chat_id: String,
    pub chat_name: Option<String>,
    /// "dm", "group", "channel", "thread" (default "dm").
    pub chat_type: String,
    pub user_id: Option<String>,
    pub user_name: Option<String>,
    /// For forum topics, Discord threads, etc.
    pub thread_id: Option<String>,
    /// Channel topic/description (Discord, Slack).
    pub chat_topic: Option<String>,
    /// Platform-specific stable alt ID (Signal UUID, Feishu union_id).
    pub user_id_alt: Option<String>,
    /// Signal group internal ID.
    pub chat_id_alt: Option<String>,
    /// True when the message author is a bot/webhook (Discord).
    pub is_bot: bool,
    /// Platform-neutral SCOPE discriminator (Discord guild / Slack workspace /
    /// Matrix server). Drives server/workspace isolation. Wire migration
    /// (D-Q2.5): `scope_id` is the canonical name; `guild_id` is a deprecated
    /// legacy alias kept during the cross-repo dual-read/dual-write overlap.
    /// Both are written by serialization and read by deserialization
    /// (`scope_id` wins).
    pub scope_id: Option<String>,
    /// @deprecated legacy alias for `scope_id` (D-Q2.5).
    pub guild_id: Option<String>,
    /// Parent channel when `chat_id` refers to a thread.
    pub parent_chat_id: Option<String>,
    /// ID of the triggering message (for pin/reply/react).
    pub message_id: Option<String>,
    /// True when adapter granted access via role (not user ID).
    pub role_authorized: bool,
    /// Profile this inbound message is routed to in a multiplexing gateway.
    /// `None` => the gateway's active/default profile.
    pub profile: Option<String>,
    /// Discord auto-thread metadata: True only for threads the gateway just
    /// auto-created (safe rename targets).
    pub auto_thread_created: bool,
    pub auto_thread_initial_name: Option<String>,
    /// Internal, wire-INVISIBLE trust signal: True when this event was
    /// delivered over the per-instance-authenticated relay WebSocket.
    /// Deliberately excluded from serialization so a peer can never forge it
    /// across the wire or have it restored from persistence.
    pub delivered_via_upstream_relay: bool,
}

impl SessionSource {
    /// Construct a source with upstream dataclass defaults
    /// (`chat_type="dm"`, everything else absent/false).
    pub fn new(platform: Platform, chat_id: impl Into<String>) -> Self {
        Self {
            platform,
            chat_id: chat_id.into(),
            chat_name: None,
            chat_type: "dm".to_string(),
            user_id: None,
            user_name: None,
            thread_id: None,
            chat_topic: None,
            user_id_alt: None,
            chat_id_alt: None,
            is_bot: false,
            scope_id: None,
            guild_id: None,
            parent_chat_id: None,
            message_id: None,
            role_authorized: false,
            profile: None,
            auto_thread_created: false,
            auto_thread_initial_name: None,
            delivered_via_upstream_relay: false,
        }
    }

    /// D-Q2.5 dual-field reconciliation (port of `__post_init__`): `scope_id`
    /// is canonical, `guild_id` is the deprecated alias. Mirror whichever was
    /// provided onto the other (`scope_id` wins on conflict) so readers of
    /// EITHER field see the same value during the wire-migration overlap.
    pub fn reconcile_scope_alias(&mut self) {
        if self.scope_id.is_none() && self.guild_id.is_some() {
            self.scope_id = self.guild_id.clone();
        } else if self.scope_id.is_some() {
            self.guild_id = self.scope_id.clone();
        }
    }

    /// Human-readable description of the source (port of the `description`
    /// property).
    pub fn description(&self) -> String {
        if self.platform == Platform::Local {
            return "CLI terminal".to_string();
        }

        let mut parts: Vec<String> = Vec::new();
        match self.chat_type.as_str() {
            "dm" => parts.push(format!(
                "DM with {}",
                non_empty(&self.user_name)
                    .or_else(|| non_empty(&self.user_id))
                    .unwrap_or("user")
            )),
            "group" => parts.push(format!(
                "group: {}",
                non_empty(&self.chat_name).unwrap_or(&self.chat_id)
            )),
            "channel" => parts.push(format!(
                "channel: {}",
                non_empty(&self.chat_name).unwrap_or(&self.chat_id)
            )),
            _ => parts.push(
                non_empty(&self.chat_name)
                    .unwrap_or(&self.chat_id)
                    .to_string(),
            ),
        }
        if let Some(thread_id) = non_empty(&self.thread_id) {
            parts.push(format!("thread: {thread_id}"));
        }
        parts.join(", ")
    }
}

impl Serialize for SessionSource {
    /// Port of `SessionSource.to_dict`.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        // Always-emitted keys (None serializes as null, as upstream emits
        // None values for these).
        map.serialize_entry("platform", self.platform.as_str())?;
        map.serialize_entry("chat_id", &self.chat_id)?;
        map.serialize_entry("chat_name", &self.chat_name)?;
        map.serialize_entry("chat_type", &self.chat_type)?;
        map.serialize_entry("user_id", &self.user_id)?;
        map.serialize_entry("user_name", &self.user_name)?;
        map.serialize_entry("thread_id", &self.thread_id)?;
        map.serialize_entry("chat_topic", &self.chat_topic)?;
        // Conditionally-emitted keys (only when truthy).
        if let Some(v) = non_empty(&self.user_id_alt) {
            map.serialize_entry("user_id_alt", v)?;
        }
        if let Some(v) = non_empty(&self.chat_id_alt) {
            map.serialize_entry("chat_id_alt", v)?;
        }
        // D-Q2.5 dual-write: emit BOTH the canonical `scope_id` and the
        // deprecated `guild_id` alias so a connector on either side of the
        // migration resolves the scope.
        let scope = self
            .scope_id
            .as_deref()
            .or(self.guild_id.as_deref())
            .filter(|s| !s.is_empty());
        if let Some(scope) = scope {
            map.serialize_entry("scope_id", scope)?;
            map.serialize_entry("guild_id", scope)?;
        }
        if let Some(v) = non_empty(&self.parent_chat_id) {
            map.serialize_entry("parent_chat_id", v)?;
        }
        if let Some(v) = non_empty(&self.message_id) {
            map.serialize_entry("message_id", v)?;
        }
        if let Some(v) = non_empty(&self.profile) {
            map.serialize_entry("profile", v)?;
        }
        if self.auto_thread_created {
            map.serialize_entry("auto_thread_created", &true)?;
        }
        if let Some(v) = non_empty(&self.auto_thread_initial_name) {
            map.serialize_entry("auto_thread_initial_name", v)?;
        }
        map.end()
    }
}

/// Wire shape for deserialization (mirrors `from_dict`'s reads; unknown keys
/// are ignored, `is_bot`/`role_authorized`/`delivered_via_upstream_relay`
/// are deliberately not read).
#[derive(Deserialize)]
struct SessionSourceWire {
    platform: Platform,
    chat_id: serde_json::Value,
    #[serde(default)]
    chat_name: Option<String>,
    #[serde(default)]
    chat_type: Option<String>,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    user_name: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    chat_topic: Option<String>,
    #[serde(default)]
    user_id_alt: Option<String>,
    #[serde(default)]
    chat_id_alt: Option<String>,
    #[serde(default)]
    scope_id: Option<String>,
    #[serde(default)]
    guild_id: Option<String>,
    #[serde(default)]
    parent_chat_id: Option<String>,
    #[serde(default)]
    message_id: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    #[serde(default)]
    auto_thread_created: Option<serde_json::Value>,
    #[serde(default)]
    auto_thread_initial_name: Option<String>,
}

/// Python `str(value)` for the `chat_id` coercion in `from_dict`
/// (`str(data["chat_id"])` — Telegram peers commonly send numbers).
fn coerce_to_python_str(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(true) => "True".to_string(),
        serde_json::Value::Bool(false) => "False".to_string(),
        serde_json::Value::Null => "None".to_string(),
        other => other.to_string(),
    }
}

/// Python `bool(value)` truthiness for JSON values.
fn python_truthy(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => false,
        serde_json::Value::Bool(b) => *b,
        serde_json::Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        serde_json::Value::String(s) => !s.is_empty(),
        serde_json::Value::Array(a) => !a.is_empty(),
        serde_json::Value::Object(o) => !o.is_empty(),
    }
}

impl<'de> Deserialize<'de> for SessionSource {
    /// Port of `SessionSource.from_dict` (including the `__post_init__`
    /// scope/guild reconciliation that runs on construction).
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = SessionSourceWire::deserialize(deserializer)?;
        // D-Q2.5 dual-read: prefer the canonical `scope_id`, fall back to the
        // deprecated `guild_id` alias (a peer not yet migrated still sends it).
        let scope_id = wire.scope_id.or(wire.guild_id);
        let mut source = SessionSource {
            platform: wire.platform,
            chat_id: coerce_to_python_str(&wire.chat_id),
            chat_name: wire.chat_name,
            chat_type: wire.chat_type.unwrap_or_else(|| "dm".to_string()),
            user_id: wire.user_id,
            user_name: wire.user_name,
            thread_id: wire.thread_id,
            chat_topic: wire.chat_topic,
            user_id_alt: wire.user_id_alt,
            chat_id_alt: wire.chat_id_alt,
            is_bot: false,
            scope_id,
            guild_id: None,
            parent_chat_id: wire.parent_chat_id,
            message_id: wire.message_id,
            role_authorized: false,
            profile: wire.profile,
            auto_thread_created: wire
                .auto_thread_created
                .as_ref()
                .map(python_truthy)
                .unwrap_or(false),
            auto_thread_initial_name: wire.auto_thread_initial_name,
            delivered_via_upstream_relay: false,
        };
        source.reconcile_scope_alias();
        Ok(source)
    }
}

/// Options that affect session-key derivation. Mirrors the two keyword
/// parameters of upstream `build_session_key` and their defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// Session-key namespace for a profile (port of `_session_key_namespace`):
/// default profile (`None`/`""`/`"default"`) → `agent:main` — byte-identical
/// to every key ever generated; named profile `coder` → `agent:coder`.
fn session_key_namespace(profile: Option<&str>) -> String {
    match profile {
        Some(p) if !p.is_empty() && p != "default" => format!("agent:{p}"),
        _ => "agent:main".to_string(),
    }
}

/// Build a deterministic session key from a message source (port of
/// `gateway/session.py::build_session_key`; single source of truth for
/// session-key construction).
///
/// `profile` selects the key namespace. It defaults to `None` ⇒ the legacy
/// `agent:main` namespace; only a multiplexing gateway passes a non-default
/// profile (upstream gates it on `multiplex_profiles`, preferring
/// `source.profile` then the active profile name — the profile is a caller
/// decision, never read implicitly from the source here).
///
/// DM rules:
/// - DMs include chat_id when present, so each private conversation is isolated.
/// - thread_id further differentiates threaded DMs within the same DM chat.
/// - Without chat_id, the sender's identifier (`user_id_alt` or `user_id`)
///   is used, then thread_id, then the bare per-platform sink.
///
/// Group/channel rules (any non-"dm" chat_type):
/// - chat_id identifies the parent group/channel (skipped when empty).
/// - thread_id differentiates threads within that parent chat.
/// - `group_sessions_per_user` isolates participants; when a thread_id is
///   present and `thread_sessions_per_user` is false (default), threads are
///   *shared* — the participant id is NOT appended.
///
/// WhatsApp chat/participant identifiers are canonicalized so phone-JID/LID
/// alias flips never split one human across two sessions.
pub fn build_session_key(
    source: &SessionSource,
    profile: Option<&str>,
    opts: SessionKeyOptions,
) -> String {
    let ns = session_key_namespace(profile);
    let platform = source.platform.as_str();
    let thread_id = non_empty(&source.thread_id);

    if source.chat_type == "dm" {
        let dm_chat_id = if source.platform == Platform::Whatsapp {
            canonical_whatsapp_identifier(&source.chat_id)
        } else {
            source.chat_id.clone()
        };

        if !dm_chat_id.is_empty() {
            return match thread_id {
                Some(tid) => format!("{ns}:{platform}:dm:{dm_chat_id}:{tid}"),
                None => format!("{ns}:{platform}:dm:{dm_chat_id}"),
            };
        }
        // No chat_id — fall back to the sender's own identifier before the
        // bare per-platform sink, keeping DMs isolated per user (upstream:
        // cross-user history-bleed guard).
        let mut dm_participant_id = non_empty(&source.user_id_alt)
            .or_else(|| non_empty(&source.user_id))
            .map(str::to_string);
        if let Some(pid) = &dm_participant_id {
            if source.platform == Platform::Whatsapp {
                let canonical = canonical_whatsapp_identifier(pid);
                if !canonical.is_empty() {
                    dm_participant_id = Some(canonical);
                }
            }
        }
        if let Some(pid) = dm_participant_id {
            return match thread_id {
                Some(tid) => format!("{ns}:{platform}:dm:{pid}:{tid}"),
                None => format!("{ns}:{platform}:dm:{pid}"),
            };
        }
        if let Some(tid) = thread_id {
            return format!("{ns}:{platform}:dm:{tid}");
        }
        return format!("{ns}:{platform}:dm");
    }

    let mut participant_id = non_empty(&source.user_id_alt)
        .or_else(|| non_empty(&source.user_id))
        .map(str::to_string);
    if let Some(pid) = &participant_id {
        if source.platform == Platform::Whatsapp {
            // Same JID/LID-flip bug as the DM case: without canonicalisation,
            // a single group member gets two isolated per-user sessions when
            // the bridge reshuffles alias forms.
            let canonical = canonical_whatsapp_identifier(pid);
            if !canonical.is_empty() {
                participant_id = Some(canonical);
            }
        }
    }

    let mut key_parts: Vec<String> =
        vec![ns, platform.to_string(), source.chat_type.clone()];
    if !source.chat_id.is_empty() {
        key_parts.push(source.chat_id.clone());
    }
    if let Some(tid) = thread_id {
        key_parts.push(tid.to_string());
    }

    // In threads, default to shared sessions (all participants see the same
    // conversation). Per-user isolation only applies when explicitly enabled
    // via thread_sessions_per_user, or when there is no thread.
    let mut isolate_user = opts.group_sessions_per_user;
    if thread_id.is_some() && !opts.thread_sessions_per_user {
        isolate_user = false;
    }

    if isolate_user {
        if let Some(pid) = participant_id {
            key_parts.push(pid);
        }
    }

    key_parts.join(":")
}

/// Convenience wrapper matching upstream's zero-keyword call shape
/// (`build_session_key(source)`): default profile namespace, default
/// isolation options.
pub fn build_session_key_with_defaults(source: &SessionSource) -> String {
    build_session_key(source, None, SessionKeyOptions::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::lock_home;
    use joey_core::constants::HomeOverrideGuard;
    use serde_json::json;

    fn src(platform: Platform, chat_id: &str, chat_type: &str) -> SessionSource {
        let mut s = SessionSource::new(platform, chat_id);
        s.chat_type = chat_type.to_string();
        s
    }

    fn key(source: &SessionSource) -> String {
        build_session_key_with_defaults(source)
    }

    // ---- session-key grammar: DM branch ----

    #[test]
    fn dm_includes_chat_id() {
        let source = src(Platform::Telegram, "99", "dm");
        assert_eq!(key(&source), "agent:main:telegram:dm:99");
    }

    #[test]
    fn dm_with_thread_id_appends_thread() {
        let mut source = src(Platform::Telegram, "99", "dm");
        source.thread_id = Some("topic-1".into());
        source.user_id = Some("42".into()); // user_id never included in DM keys
        assert_eq!(key(&source), "agent:main:telegram:dm:99:topic-1");
    }

    #[test]
    fn dm_without_chat_id_falls_back_to_user_id() {
        let mut source = src(Platform::Telegram, "", "dm");
        source.user_id = Some("jordan".into());
        assert_eq!(key(&source), "agent:main:telegram:dm:jordan");
    }

    #[test]
    fn dm_fallback_prefers_user_id_alt() {
        let mut source = src(Platform::Telegram, "", "dm");
        source.user_id = Some("primary".into());
        source.user_id_alt = Some("alt".into());
        assert_eq!(key(&source), "agent:main:telegram:dm:alt");
    }

    #[test]
    fn dm_participant_fallback_still_appends_thread() {
        let mut source = src(Platform::Telegram, "", "dm");
        source.user_id = Some("jordan".into());
        source.thread_id = Some("7".into());
        assert_eq!(key(&source), "agent:main:telegram:dm:jordan:7");
    }

    #[test]
    fn dm_without_identifiers_falls_back_to_thread_then_sink() {
        let mut threaded = src(Platform::Telegram, "", "dm");
        threaded.thread_id = Some("7".into());
        assert_eq!(key(&threaded), "agent:main:telegram:dm:7");

        let bare = src(Platform::Telegram, "", "dm");
        assert_eq!(key(&bare), "agent:main:telegram:dm");
    }

    #[test]
    fn dm_empty_string_options_are_skipped_like_none() {
        // Some("") must behave exactly like None (Python falsiness).
        let mut source = src(Platform::Telegram, "", "dm");
        source.user_id = Some("".into());
        source.user_id_alt = Some("".into());
        source.thread_id = Some("".into());
        assert_eq!(key(&source), "agent:main:telegram:dm");
    }

    // ---- session-key grammar: group/channel/any branch ----

    #[test]
    fn group_isolates_per_user_by_default() {
        let mut alice = src(Platform::Discord, "guild-123", "group");
        alice.user_id = Some("alice".into());
        let mut bob = alice.clone();
        bob.user_id = Some("bob".into());
        assert_eq!(key(&alice), "agent:main:discord:group:guild-123:alice");
        assert_eq!(key(&bob), "agent:main:discord:group:guild-123:bob");
    }

    #[test]
    fn group_shared_when_isolation_disabled() {
        let mut source = src(Platform::Discord, "guild-123", "group");
        source.user_id = Some("alice".into());
        let opts = SessionKeyOptions {
            group_sessions_per_user: false,
            thread_sessions_per_user: false,
        };
        assert_eq!(
            build_session_key(&source, None, opts),
            "agent:main:discord:group:guild-123"
        );
    }

    #[test]
    fn group_without_participant_shares_per_chat() {
        let source = src(Platform::Discord, "guild-123", "group");
        assert_eq!(key(&source), "agent:main:discord:group:guild-123");
    }

    #[test]
    fn group_thread_suppresses_per_user_isolation() {
        // The audit's key concrete case: defaults, group + thread → the
        // thread key is shared; user_id must NOT be appended.
        let mut source = src(Platform::Telegram, "12345", "group");
        source.thread_id = Some("777".into());
        source.user_id = Some("user99".into());
        assert_eq!(key(&source), "agent:main:telegram:group:12345:777");
    }

    #[test]
    fn group_thread_isolation_restored_by_thread_sessions_per_user() {
        let mut source = src(Platform::Telegram, "-1002285219667", "group");
        source.thread_id = Some("17585".into());
        source.user_id = Some("42".into());
        let opts = SessionKeyOptions {
            group_sessions_per_user: true,
            thread_sessions_per_user: true,
        };
        assert_eq!(
            build_session_key(&source, None, opts),
            "agent:main:telegram:group:-1002285219667:17585:42"
        );
    }

    #[test]
    fn group_empty_chat_id_is_skipped_never_an_empty_segment() {
        let mut source = src(Platform::Discord, "", "group");
        source.user_id = Some("alice".into());
        assert_eq!(key(&source), "agent:main:discord:group:alice");

        let bare = src(Platform::Discord, "", "group");
        assert_eq!(key(&bare), "agent:main:discord:group");
    }

    #[test]
    fn channel_and_custom_chat_types_use_generic_branch() {
        let mut channel = src(Platform::Slack, "C123", "channel");
        channel.user_id = Some("u1".into());
        assert_eq!(key(&channel), "agent:main:slack:channel:C123:u1");

        let mut custom = src(Platform::Telegram, "b1", "broadcast");
        custom.user_id = Some("u2".into());
        assert_eq!(key(&custom), "agent:main:telegram:broadcast:b1:u2");
    }

    #[test]
    fn group_participant_prefers_user_id_alt() {
        let mut source = src(Platform::Signal, "g1", "group");
        source.user_id = Some("+15551234567".into());
        source.user_id_alt = Some("uuid-abc".into());
        assert_eq!(key(&source), "agent:main:signal:group:g1:uuid-abc");
    }

    // ---- session-key grammar: profile namespace ----

    #[test]
    fn profile_selects_namespace() {
        let source = src(Platform::Telegram, "99", "dm");
        let opts = SessionKeyOptions::default();
        assert_eq!(build_session_key(&source, None, opts), "agent:main:telegram:dm:99");
        assert_eq!(build_session_key(&source, Some(""), opts), "agent:main:telegram:dm:99");
        assert_eq!(
            build_session_key(&source, Some("default"), opts),
            "agent:main:telegram:dm:99"
        );
        assert_eq!(
            build_session_key(&source, Some("coder"), opts),
            "agent:coder:telegram:dm:99"
        );
    }

    #[test]
    fn profile_is_a_parameter_not_read_from_source() {
        // Upstream only namespaces by source.profile when the multiplexing
        // caller passes it explicitly; the builder itself must ignore it.
        let mut source = src(Platform::Telegram, "99", "dm");
        source.profile = Some("coder".into());
        assert_eq!(key(&source), "agent:main:telegram:dm:99");
    }

    // ---- session-key grammar: whatsapp canonicalization ----

    #[test]
    fn whatsapp_canonicalization_in_keys() {
        let _l = lock_home();
        let home = tempfile::tempdir().unwrap();
        let mapping_dir = home.path().join("whatsapp").join("session");
        std::fs::create_dir_all(&mapping_dir).unwrap();
        std::fs::write(
            mapping_dir.join("lid-mapping-999999999999999.json"),
            serde_json::to_string("15551234567@s.whatsapp.net").unwrap(),
        )
        .unwrap();
        let _guard = HomeOverrideGuard::new(home.path().to_path_buf());

        // DM chat_id is canonicalized: both alias forms share one key.
        let lid = src(Platform::Whatsapp, "999999999999999@lid", "dm");
        let phone = src(Platform::Whatsapp, "15551234567@s.whatsapp.net", "dm");
        assert_eq!(key(&lid), "agent:main:whatsapp:dm:15551234567");
        assert_eq!(key(&phone), "agent:main:whatsapp:dm:15551234567");

        // Group participant ids are canonicalized too; the group chat_id is
        // NOT touched.
        let mut lid_member = src(Platform::Whatsapp, "120363000000000000@g.us", "group");
        lid_member.user_id = Some("999999999999999@lid".into());
        let mut phone_member = lid_member.clone();
        phone_member.user_id = Some("15551234567@s.whatsapp.net".into());
        let expected = "agent:main:whatsapp:group:120363000000000000@g.us:15551234567";
        assert_eq!(key(&lid_member), expected);
        assert_eq!(key(&phone_member), expected);

        // With isolation off the participant is absent entirely.
        let opts = SessionKeyOptions {
            group_sessions_per_user: false,
            thread_sessions_per_user: false,
        };
        assert_eq!(
            build_session_key(&lid_member, None, opts),
            "agent:main:whatsapp:group:120363000000000000@g.us"
        );

        // Non-WhatsApp platforms are never canonicalized.
        let telegram = src(Platform::Telegram, "15551234567@s.whatsapp.net", "dm");
        assert_eq!(key(&telegram), "agent:main:telegram:dm:15551234567@s.whatsapp.net");
    }

    // ---- SessionSource serde vs upstream to_dict/from_dict ----

    #[test]
    fn serializes_exactly_like_upstream_to_dict_minimal() {
        let source = SessionSource::new(Platform::Local, "cli");
        let value = serde_json::to_value(&source).unwrap();
        assert_eq!(
            value,
            json!({
                "platform": "local",
                "chat_id": "cli",
                "chat_name": null,
                "chat_type": "dm",
                "user_id": null,
                "user_name": null,
                "thread_id": null,
                "chat_topic": null,
            })
        );
    }

    #[test]
    fn serializes_exactly_like_upstream_to_dict_full() {
        let mut source = SessionSource::new(Platform::Discord, "789");
        source.chat_name = Some("Server / #project".into());
        source.chat_type = "group".into();
        source.user_id = Some("42".into());
        source.user_name = Some("bob".into());
        source.thread_id = Some("t9".into());
        source.chat_topic = Some("Planning".into());
        source.user_id_alt = Some("alt-42".into());
        source.chat_id_alt = Some("alt-789".into());
        source.scope_id = Some("guild-1".into());
        source.parent_chat_id = Some("parent-1".into());
        source.message_id = Some("m-5".into());
        source.profile = Some("coder".into());
        source.auto_thread_created = true;
        source.auto_thread_initial_name = Some("First title".into());
        // Wire-invisible fields must not leak.
        source.is_bot = true;
        source.role_authorized = true;
        source.delivered_via_upstream_relay = true;

        let value = serde_json::to_value(&source).unwrap();
        assert_eq!(
            value,
            json!({
                "platform": "discord",
                "chat_id": "789",
                "chat_name": "Server / #project",
                "chat_type": "group",
                "user_id": "42",
                "user_name": "bob",
                "thread_id": "t9",
                "chat_topic": "Planning",
                "user_id_alt": "alt-42",
                "chat_id_alt": "alt-789",
                "scope_id": "guild-1",
                "guild_id": "guild-1",
                "parent_chat_id": "parent-1",
                "message_id": "m-5",
                "profile": "coder",
                "auto_thread_created": true,
                "auto_thread_initial_name": "First title",
            })
        );
    }

    #[test]
    fn deserializes_literal_upstream_json_and_reserializes_compatibly() {
        // Byte-shape as produced by hermes to_dict (nulls for absent
        // always-keys, dual scope write).
        let upstream = json!({
            "platform": "telegram",
            "chat_id": "12345",
            "chat_name": "My Group",
            "chat_type": "group",
            "user_id": "99",
            "user_name": "alice",
            "thread_id": "t1",
            "chat_topic": null,
            "scope_id": "s-1",
            "guild_id": "s-1",
        });
        let source: SessionSource = serde_json::from_value(upstream.clone()).unwrap();
        assert_eq!(source.platform, Platform::Telegram);
        assert_eq!(source.chat_id, "12345");
        assert_eq!(source.chat_name.as_deref(), Some("My Group"));
        assert_eq!(source.chat_type, "group");
        assert_eq!(source.user_id.as_deref(), Some("99"));
        assert_eq!(source.user_name.as_deref(), Some("alice"));
        assert_eq!(source.thread_id.as_deref(), Some("t1"));
        assert_eq!(source.chat_topic, None);
        assert_eq!(source.scope_id.as_deref(), Some("s-1"));
        assert_eq!(source.guild_id.as_deref(), Some("s-1"));
        assert!(!source.is_bot);
        assert!(!source.role_authorized);
        assert!(!source.delivered_via_upstream_relay);

        // Lossless round trip.
        assert_eq!(serde_json::to_value(&source).unwrap(), upstream);
    }

    #[test]
    fn deserialize_minimal_applies_defaults() {
        let source: SessionSource =
            serde_json::from_value(json!({"platform": "discord", "chat_id": "abc"})).unwrap();
        assert_eq!(source.chat_type, "dm");
        assert_eq!(source.chat_name, None);
        assert_eq!(source.user_id, None);
        assert_eq!(source.user_name, None);
        assert_eq!(source.thread_id, None);
        assert_eq!(source.chat_topic, None);
        assert!(!source.auto_thread_created);
    }

    #[test]
    fn deserialize_coerces_numeric_chat_id() {
        let source: SessionSource =
            serde_json::from_value(json!({"platform": "telegram", "chat_id": 12345})).unwrap();
        assert_eq!(source.chat_id, "12345");
        let negative: SessionSource =
            serde_json::from_value(json!({"platform": "telegram", "chat_id": -1002285219667i64}))
                .unwrap();
        assert_eq!(negative.chat_id, "-1002285219667");
    }

    #[test]
    fn deserialize_reads_legacy_guild_id_and_scope_id_wins() {
        // Legacy peer sends only guild_id: both fields resolve to it and a
        // re-serialize dual-writes.
        let legacy: SessionSource = serde_json::from_value(json!({
            "platform": "discord",
            "chat_id": "1",
            "guild_id": "g-legacy",
        }))
        .unwrap();
        assert_eq!(legacy.scope_id.as_deref(), Some("g-legacy"));
        assert_eq!(legacy.guild_id.as_deref(), Some("g-legacy"));
        let value = serde_json::to_value(&legacy).unwrap();
        assert_eq!(value["scope_id"], "g-legacy");
        assert_eq!(value["guild_id"], "g-legacy");

        // On conflict, scope_id wins (and mirrors onto guild_id).
        let conflict: SessionSource = serde_json::from_value(json!({
            "platform": "discord",
            "chat_id": "1",
            "scope_id": "canonical",
            "guild_id": "stale",
        }))
        .unwrap();
        assert_eq!(conflict.scope_id.as_deref(), Some("canonical"));
        assert_eq!(conflict.guild_id.as_deref(), Some("canonical"));
    }

    #[test]
    fn wire_invisible_fields_are_never_restored() {
        let source: SessionSource = serde_json::from_value(json!({
            "platform": "discord",
            "chat_id": "1",
            "is_bot": true,
            "role_authorized": true,
            "delivered_via_upstream_relay": true,
        }))
        .unwrap();
        assert!(!source.is_bot);
        assert!(!source.role_authorized);
        assert!(!source.delivered_via_upstream_relay);
    }

    #[test]
    fn empty_optional_strings_are_omitted_like_falsy_values() {
        let mut source = SessionSource::new(Platform::Discord, "1");
        source.user_id_alt = Some("".into());
        source.scope_id = Some("".into());
        source.profile = Some("".into());
        let value = serde_json::to_value(&source).unwrap();
        let map = value.as_object().unwrap();
        assert!(!map.contains_key("user_id_alt"));
        assert!(!map.contains_key("scope_id"));
        assert!(!map.contains_key("guild_id"));
        assert!(!map.contains_key("profile"));
        assert!(!map.contains_key("auto_thread_created"));
    }

    #[test]
    fn description_matches_upstream_shapes() {
        let local = SessionSource::new(Platform::Local, "cli");
        assert_eq!(local.description(), "CLI terminal");

        let mut dm = SessionSource::new(Platform::Telegram, "99");
        dm.user_name = Some("alice".into());
        assert_eq!(dm.description(), "DM with alice");

        let mut group = SessionSource::new(Platform::Discord, "g1");
        group.chat_type = "group".into();
        group.chat_name = Some("My Group".into());
        group.thread_id = Some("t1".into());
        assert_eq!(group.description(), "group: My Group, thread: t1");
    }
}
