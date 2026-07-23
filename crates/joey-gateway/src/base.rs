//! Platform-adapter core types (port of the relevant parts of upstream
//! `gateway/platforms/base.py`): [`MessageType`], [`MessageEvent`],
//! [`SendResult`], the send-error classification vocabulary, message
//! truncation, and the [`PlatformAdapter`] trait.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::config::Platform;
use crate::session::SessionSource;

/// Types of incoming messages (port of `MessageType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageType {
    #[default]
    Text,
    Location,
    Photo,
    Video,
    Audio,
    Voice,
    Document,
    Sticker,
    /// `/command` style.
    Command,
}

impl MessageType {
    /// The lowercase wire value (upstream `message_type.value`).
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageType::Text => "text",
            MessageType::Location => "location",
            MessageType::Photo => "photo",
            MessageType::Video => "video",
            MessageType::Audio => "audio",
            MessageType::Voice => "voice",
            MessageType::Document => "document",
            MessageType::Sticker => "sticker",
            MessageType::Command => "command",
        }
    }
}

/// Auto-loaded skill(s) for topic/channel bindings (upstream
/// `auto_skill: Optional[str | list[str]]` — a single name or ordered list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoSkill {
    One(String),
    Many(Vec<String>),
}

/// Incoming message from a platform (port of `MessageEvent`) — the
/// normalized representation that all adapters produce.
#[derive(Debug, Clone)]
pub struct MessageEvent {
    /// Message content.
    pub text: String,
    pub message_type: MessageType,
    /// Source information.
    pub source: SessionSource,
    /// Original platform data (upstream `raw_message: Any`).
    pub raw_message: serde_json::Value,
    pub message_id: Option<String>,
    /// Platform-specific update identifier (Telegram `update_id`; other
    /// platforms currently ignore it). Used by `/restart` to advance the
    /// Telegram offset past the triggering update.
    pub platform_update_id: Option<i64>,
    /// Media attachments — local file paths (for vision tool access).
    pub media_urls: Vec<String>,
    pub media_types: Vec<String>,
    /// Reply context.
    pub reply_to_message_id: Option<String>,
    /// Text of the replied-to message (for context injection).
    pub reply_to_text: Option<String>,
    pub reply_to_author_id: Option<String>,
    pub reply_to_author_name: Option<String>,
    /// True when the user replied to this bot/assistant's message.
    pub reply_to_is_own_message: bool,
    /// Auto-loaded skill(s) for topic/channel bindings.
    pub auto_skill: Option<AutoSkill>,
    /// Per-channel ephemeral system prompt (e.g. Discord channel_prompts).
    /// Applied at API call time and never persisted to transcript history.
    pub channel_prompt: Option<String>,
    /// Channel context recovered by history backfill; kept separate from
    /// `text` so sender-prefix logic can operate on the trigger message
    /// alone, then prepend this context afterward.
    pub channel_context: Option<String>,
    /// Internal flag — set for synthetic events (e.g. background process
    /// completion notifications) that must bypass user authorization checks.
    pub internal: bool,
    /// Free-form per-event metadata; adapters may set platform-specific
    /// signals here (e.g. WhatsApp `whatsapp_from_owner=true`).
    pub metadata: serde_json::Map<String, serde_json::Value>,
    /// Event timestamp (upstream `datetime.now()` default).
    pub timestamp: chrono::DateTime<chrono::Local>,
}

impl MessageEvent {
    /// Construct an event with upstream dataclass defaults (text type, empty
    /// media/metadata, `timestamp=now`).
    pub fn new(text: impl Into<String>, source: SessionSource) -> Self {
        Self {
            text: text.into(),
            message_type: MessageType::Text,
            source,
            raw_message: serde_json::Value::Null,
            message_id: None,
            platform_update_id: None,
            media_urls: Vec::new(),
            media_types: Vec::new(),
            reply_to_message_id: None,
            reply_to_text: None,
            reply_to_author_id: None,
            reply_to_author_name: None,
            reply_to_is_own_message: false,
            auto_skill: None,
            channel_prompt: None,
            channel_context: None,
            internal: false,
            metadata: serde_json::Map::new(),
            timestamp: chrono::Local::now(),
        }
    }

    /// Check if this is a command message (e.g., /new, /reset).
    pub fn is_command(&self) -> bool {
        self.text.starts_with('/')
    }

    /// Extract the command name if this is a command message.
    ///
    /// Mirrors upstream exactly: first whitespace token, leading `/`
    /// stripped, lowercased, `@botname` suffix removed; names containing `/`
    /// (file paths) are rejected with `None`.
    pub fn get_command(&self) -> Option<String> {
        if !self.is_command() {
            return None;
        }
        // Split on whitespace and get the first word, strip the "/".
        let (first, _) = split_whitespace_once(&self.text);
        let mut raw = match first {
            Some(token) => token[1..].to_lowercase(),
            None => return None,
        };
        if !raw.is_empty() && raw.contains('@') {
            raw = raw.split('@').next().unwrap_or("").to_string();
        }
        // Reject file paths: valid command names never contain "/".
        if !raw.is_empty() && raw.contains('/') {
            return None;
        }
        Some(raw)
    }

    /// Get the arguments after a command. Non-command text is returned
    /// unchanged. iOS auto-corrects `--` to `—` (em dash) and `-` to `–`
    /// (en dash); those are mapped back.
    pub fn get_command_args(&self) -> String {
        if !self.is_command() {
            return self.text.clone();
        }
        let (_, rest) = split_whitespace_once(&self.text);
        let args = rest.unwrap_or("");
        args.replace("\u{2014}\u{2014}", "--")
            .replace('\u{2014}', "--")
            .replace('\u{2013}', "-")
    }
}

/// Python `str.split(None, 1)` — first whitespace-delimited token and the
/// remainder after the delimiter run (leading whitespace ignored, the
/// remainder keeps its trailing whitespace, all-whitespace remainder is
/// absent).
fn split_whitespace_once(s: &str) -> (Option<&str>, Option<&str>) {
    let start = s.trim_start();
    if start.is_empty() {
        return (None, None);
    }
    match start.find(char::is_whitespace) {
        None => (Some(start), None),
        Some(idx) => {
            let first = &start[..idx];
            // trim_start consumes the delimiter run only; the remainder keeps
            // its internal and trailing whitespace, like Python's split.
            let rest = start[idx..].trim_start();
            if rest.is_empty() {
                (Some(first), None)
            } else {
                (Some(first), Some(rest))
            }
        }
    }
}

/// Result of sending a message (port of `SendResult`).
#[derive(Debug, Clone)]
pub struct SendResult {
    pub success: bool,
    pub message_id: Option<String>,
    pub error: Option<String>,
    /// Adapter-specific metadata (upstream `raw_response: Any`).
    /// Cross-layer contracts that affect delivery semantics must be
    /// documented at the producer and consumer sites (e.g. Telegram edit
    /// overflow partials set `raw_response["partial_overflow"]`).
    pub raw_response: serde_json::Value,
    /// True for transient connection errors — the base retries automatically.
    pub retryable: bool,
    /// Server-requested retry delay in seconds (e.g. Telegram FloodWait
    /// retry_after); honored instead of the default backoff when present.
    pub retry_after: Option<f64>,
    /// When the adapter had to split an oversized payload across multiple
    /// platform messages, `message_id` is the LAST visible message id and
    /// these are the additional message ids in send order. Empty for the
    /// common single-message case.
    pub continuation_message_ids: Vec<String>,
    /// Machine-readable failure category (set only when `success` is false).
    /// One of [`SEND_ERROR_KINDS`] or `None` (unset / not classified).
    /// Producers should set this via [`classify_send_error`].
    pub error_kind: Option<String>,
}

impl SendResult {
    /// Successful send with upstream defaults for the remaining fields.
    pub fn ok(message_id: Option<String>) -> Self {
        Self {
            success: true,
            message_id,
            error: None,
            raw_response: serde_json::Value::Null,
            retryable: false,
            retry_after: None,
            continuation_message_ids: Vec::new(),
            error_kind: None,
        }
    }

    /// Failed send with upstream defaults for the remaining fields.
    pub fn err(error: impl Into<String>) -> Self {
        Self {
            success: false,
            message_id: None,
            error: Some(error.into()),
            raw_response: serde_json::Value::Null,
            retryable: false,
            retry_after: None,
            continuation_message_ids: Vec::new(),
            error_kind: None,
        }
    }
}

/// Machine-readable send-failure categories (port of `SEND_ERROR_KINDS`).
/// Kept platform-neutral so every adapter can populate
/// [`SendResult::error_kind`] from the same vocabulary:
///
/// - `too_long` — content exceeded the platform's per-message size cap.
/// - `bad_format` — the platform rejected the message markup/entities.
/// - `forbidden` — blocked/kicked/no permission; the bot cannot reach the target.
/// - `not_found` — the target chat/thread/message no longer exists.
/// - `rate_limited` — the platform throttled the send (flood control).
/// - `transient` — a connection-level failure that is safe to retry.
/// - `unknown` — classification did not match any known shape.
pub const SEND_ERROR_KINDS: [&str; 7] = [
    "too_long",
    "bad_format",
    "forbidden",
    "not_found",
    "rate_limited",
    "transient",
    "unknown",
];

/// `not_found` substrings meaning the *whole chat* is gone (upstream
/// `_CHAT_LEVEL_NOT_FOUND_SUBSTRINGS`).
const CHAT_LEVEL_NOT_FOUND_SUBSTRINGS: [&str; 1] = ["chat not found"];

/// `not_found` substrings for thread/topic/message-level failures that leave
/// the parent chat reachable (upstream `_SUBCHAT_NOT_FOUND_SUBSTRINGS`).
const SUBCHAT_NOT_FOUND_SUBSTRINGS: [&str; 5] = [
    "message to edit not found",
    "message to reply not found",
    "thread not found",
    "topic_deleted",
    "message_id_invalid",
];

/// Error substrings that indicate a transient *connection* failure worth
/// retrying (upstream `_RETRYABLE_ERROR_PATTERNS`). Plain "timeout" is
/// intentionally excluded: a read/write timeout on a non-idempotent call may
/// have reached the server — retrying risks duplicate delivery.
/// "connecttimeout" is safe because the connection was never established.
pub const RETRYABLE_ERROR_PATTERNS: [&str; 9] = [
    "connecterror",
    "connectionerror",
    "connectionreset",
    "connectionrefused",
    "connecttimeout",
    "network",
    "broken pipe",
    "remotedisconnected",
    "eoferror",
];

/// Build the lowercased text blob both send-error classifiers match against
/// (port of `_error_blob`; single source of truth so [`classify_send_error`]
/// and [`is_chat_level_not_found`] can never drift).
///
/// Upstream also appends the exception's class name; Rust errors carry no
/// portable runtime type name, so callers should include any type
/// information in the error's `Display` text.
fn error_blob(error: Option<&(dyn std::error::Error + 'static)>, error_text: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !error_text.is_empty() {
        parts.push(error_text.to_string());
    }
    if let Some(err) = error {
        let text = err.to_string();
        if !text.is_empty() {
            parts.push(text);
        }
    }
    parts.join(" ").to_lowercase()
}

/// Map a send error / error string to a [`SEND_ERROR_KINDS`] value (port of
/// `classify_send_error`).
///
/// Platform-neutral: matches the lowercased text of `error` (and/or the
/// explicit `error_text`) against the substrings the major messaging APIs
/// use. Conservative — anything unrecognized returns `"unknown"` so callers
/// never mistake an unclassified failure for a benign one.
pub fn classify_send_error(
    error: Option<&(dyn std::error::Error + 'static)>,
    error_text: &str,
) -> &'static str {
    let blob = error_blob(error, error_text);
    if blob.trim().is_empty() {
        return "unknown";
    }
    if blob.contains("message_too_long")
        || blob.contains("too long")
        || blob.contains("message is too long")
    {
        return "too_long";
    }
    if blob.contains("can't parse entities")
        || blob.contains("cant parse entities")
        || blob.contains("can't find end")
        || blob.contains("unsupported start tag")
        || (blob.contains("entity") && blob.contains("parse"))
        || (blob.contains("bad request") && blob.contains("entit"))
    {
        return "bad_format";
    }
    if blob.contains("forbidden")
        || blob.contains("bot was blocked")
        || blob.contains("blocked by the user")
        || blob.contains("user is deactivated")
        || blob.contains("not enough rights")
        || blob.contains("have no rights")
        || blob.contains("not a member")
    {
        return "forbidden";
    }
    if CHAT_LEVEL_NOT_FOUND_SUBSTRINGS.iter().any(|s| blob.contains(s))
        || SUBCHAT_NOT_FOUND_SUBSTRINGS.iter().any(|s| blob.contains(s))
    {
        return "not_found";
    }
    if blob.contains("flood")
        || blob.contains("too many requests")
        || blob.contains("retry after")
        || blob.contains("rate limit")
    {
        return "rate_limited";
    }
    for pattern in RETRYABLE_ERROR_PATTERNS {
        if blob.contains(pattern) {
            return "transient";
        }
    }
    if blob.contains("connecttimeout") {
        return "transient";
    }
    "unknown"
}

/// Whether a `not_found` failure means the *whole chat* is gone (port of
/// `is_chat_level_not_found`).
///
/// Only the chat-level case should mark a delivery target dead; a deleted
/// forum topic or an edited-away message leaves the parent chat reachable.
/// When both a chat-level and a sub-chat marker are present, the sub-chat
/// reading wins (conservative: never kill a chat that may still be
/// reachable).
pub fn is_chat_level_not_found(
    error: Option<&(dyn std::error::Error + 'static)>,
    error_text: &str,
) -> bool {
    let blob = error_blob(error, error_text);
    if SUBCHAT_NOT_FOUND_SUBSTRINGS.iter().any(|s| blob.contains(s)) {
        return false;
    }
    CHAT_LEVEL_NOT_FOUND_SUBSTRINGS.iter().any(|s| blob.contains(s))
}

/// Count UTF-16 code units in `s` (port of `utf16_len`). Telegram's
/// message-length limit (4096) is measured in UTF-16 code units, not
/// codepoints — characters outside the BMP consume two units each.
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Upstream default `max_length` for [`truncate_message`].
pub const TRUNCATE_DEFAULT_MAX_LENGTH: usize = 4096;

/// Return the largest codepoint offset `n` such that
/// `measure(s[..n]) <= budget` (port of `_custom_unit_to_cp`; binary
/// search, O(log n) calls to `measure`).
fn custom_unit_to_cp(chars: &[char], budget: usize, measure: &dyn Fn(&str) -> usize) -> usize {
    let full: String = chars.iter().collect();
    if measure(&full) <= budget {
        return chars.len();
    }
    let (mut lo, mut hi) = (0usize, chars.len());
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        let prefix: String = chars[..mid].iter().collect();
        if measure(&prefix) <= budget {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    lo
}

/// Highest codepoint index of `needle` within `hay[..end]`, or -1
/// (Python `str.rfind` semantics on codepoint indices).
fn rfind_char(hay: &[char], needle: char, end: usize) -> i64 {
    let end = end.min(hay.len());
    hay[..end]
        .iter()
        .rposition(|&c| c == needle)
        .map(|i| i as i64)
        .unwrap_or(-1)
}

/// Split a long message into chunks, preserving code-block boundaries
/// (port of `BasePlatformAdapter.truncate_message`).
///
/// When a split falls inside a triple-backtick code block, the fence is
/// closed at the end of the current chunk and reopened (with the original
/// language tag) at the start of the next chunk. Multi-chunk responses
/// receive indicators like `(1/3)`.
///
/// `len_fn` measures string length; `None` means Unicode codepoints
/// (Python `len`). Pass [`utf16_len`] for platforms that measure message
/// length in UTF-16 code units (e.g. Telegram). Upstream's default
/// `max_length` is [`TRUNCATE_DEFAULT_MAX_LENGTH`].
pub fn truncate_message(
    content: &str,
    max_length: usize,
    len_fn: Option<&dyn Fn(&str) -> usize>,
) -> Vec<String> {
    let measure = |s: &str| -> usize {
        match len_fn {
            Some(f) => f(s),
            None => s.chars().count(),
        }
    };
    if measure(content) <= max_length {
        return vec![content.to_string()];
    }

    const INDICATOR_RESERVE: usize = 10; // room for " (XX/XX)"
    const FENCE_CLOSE: &str = "\n```";

    let mut chunks: Vec<String> = Vec::new();
    let mut remaining: Vec<char> = content.chars().collect();
    // When the previous chunk ended mid-code-block, this holds the language
    // tag (possibly "") so we can reopen the fence.
    let mut carry_lang: Option<String> = None;

    while !remaining.is_empty() {
        // If we're continuing a code block from the previous chunk, prepend
        // a new opening fence with the same language tag.
        let prefix = match &carry_lang {
            Some(lang) => format!("```{lang}\n"),
            None => String::new(),
        };
        let remaining_str: String = remaining.iter().collect();

        // How much body text we can fit after accounting for the prefix, a
        // potential closing fence, and the chunk indicator.
        let mut headroom: i64 = max_length as i64
            - INDICATOR_RESERVE as i64
            - measure(&prefix) as i64
            - measure(FENCE_CLOSE) as i64;
        if headroom < 1 {
            // Floor at 1 so a pathologically small max_length can't stall
            // the loop below.
            headroom = std::cmp::max(1, (max_length / 2) as i64);
        }
        let headroom = headroom as usize;

        // Everything remaining fits in one final chunk.
        if (measure(&prefix) + measure(&remaining_str)) as i64
            <= max_length as i64 - INDICATOR_RESERVE as i64
        {
            chunks.push(format!("{prefix}{remaining_str}"));
            break;
        }

        // Find a natural split point (prefer newlines, then spaces). When
        // measuring in custom units (e.g. UTF-16), map the unit budget to a
        // codepoint slice limit first.
        let cp_limit: usize = if len_fn.is_some() {
            custom_unit_to_cp(&remaining, headroom, &measure)
        } else {
            headroom
        };
        let region_end = cp_limit.min(remaining.len());

        let mut split_at: i64 = rfind_char(&remaining, '\n', region_end);
        if split_at < (cp_limit / 2) as i64 {
            split_at = rfind_char(&remaining, ' ', region_end);
        }
        if split_at < 1 {
            // Consume at least one codepoint so the loop always advances
            // (the emitted chunk may exceed a degenerate sub-codepoint
            // budget by that one codepoint — intentional, mirrors upstream).
            split_at = std::cmp::max(1, cp_limit as i64);
        }

        // Avoid splitting inside an inline code span (`...`): if the text
        // before split_at has an odd number of unescaped backticks, the
        // split falls inside inline code.
        let candidate_end = (split_at as usize).min(remaining.len());
        let candidate = &remaining[..candidate_end];
        let backtick_count = candidate.iter().filter(|&&c| c == '`').count() as i64
            - candidate
                .windows(2)
                .filter(|w| w[0] == '\\' && w[1] == '`')
                .count() as i64;
        if backtick_count % 2 == 1 {
            // Find the last unescaped backtick and split before it.
            let mut last_bt = rfind_char(candidate, '`', candidate.len());
            while last_bt > 0 && candidate[(last_bt - 1) as usize] == '\\' {
                last_bt = rfind_char(candidate, '`', last_bt as usize);
            }
            if last_bt > 0 {
                // Try to find a space or newline just before the backtick.
                let safe_split = std::cmp::max(
                    rfind_char(candidate, ' ', last_bt as usize),
                    rfind_char(candidate, '\n', last_bt as usize),
                );
                if safe_split > (cp_limit / 4) as i64 {
                    split_at = safe_split;
                }
            }
        }

        let body_end = (split_at as usize).min(remaining.len());
        let chunk_body: String = remaining[..body_end].iter().collect();
        // remaining = remaining[split_at:].lstrip()
        let mut rest: Vec<char> = remaining[body_end..].to_vec();
        let skip = rest.iter().take_while(|c| c.is_whitespace()).count();
        rest.drain(..skip);
        remaining = rest;

        let mut full_chunk = format!("{prefix}{chunk_body}");

        // Walk only the chunk body (not the prefix we prepended) to
        // determine whether we end inside an open code block.
        let mut in_code = carry_lang.is_some();
        let mut lang: String = carry_lang.clone().unwrap_or_default();
        for line in chunk_body.split('\n') {
            let stripped = line.trim();
            if let Some(after_fence) = stripped.strip_prefix("```") {
                if in_code {
                    in_code = false;
                    lang = String::new();
                } else {
                    in_code = true;
                    let tag = after_fence.trim();
                    lang = if tag.is_empty() {
                        String::new()
                    } else {
                        tag.split_whitespace().next().unwrap_or("").to_string()
                    };
                }
            }
        }

        if in_code {
            // Close the orphaned fence so the chunk is valid on its own.
            full_chunk.push_str(FENCE_CLOSE);
            carry_lang = Some(lang);
        } else {
            carry_lang = None;
        }

        chunks.push(full_chunk);
    }

    // Append chunk indicators when the response spans multiple messages.
    if chunks.len() > 1 {
        let total = chunks.len();
        chunks = chunks
            .into_iter()
            .enumerate()
            .map(|(i, chunk)| format!("{chunk} ({}/{})", i + 1, total))
            .collect();
    }

    chunks
}

/// A messaging platform adapter — port of upstream `BasePlatformAdapter`
/// (`gateway/platforms/base.py`), keeping the shorter Rust trait name.
///
/// Mapping to upstream:
/// - required methods ↔ upstream abstract methods: [`connect`]
///   (`connect(*, is_reconnect)` → bool), [`disconnect`], [`send`]
///   (`send(chat_id, content, reply_to=None, metadata=None)` → SendResult),
///   [`get_chat_info`] (dict with at least `name` and `type`);
/// - capability getters ↔ upstream class attributes with the same defaults:
///   [`supports_code_blocks`] (False), [`supports_status_text`] (False),
///   [`supports_async_delivery`] (True), [`splits_long_messages`] (False),
///   [`typed_command_prefix`] ("/"), [`supports_inchannel_continuable`]
///   (False), [`interactive_resume`] (True), [`requires_edit_finalize`]
///   (`REQUIRES_EDIT_FINALIZE`, False);
/// - default-implemented optional methods mirror the upstream default
///   bodies: [`edit_message`] (→ `SendResult(success=False, error="Not
///   supported")`), [`delete_message`] (→ false), [`send_typing`] (no-op),
///   [`format_message`] (identity), [`truncate_message`] (the shared
///   fence-preserving splitter).
///
/// The rest of the upstream surface (message-handler registration, media
/// pipeline, typing loops, busy/debounce handling, retry wrapper, ephemeral
/// deletes, handoff threads, …) is deferred with the platform adapters
/// themselves.
///
/// [`connect`]: PlatformAdapter::connect
/// [`disconnect`]: PlatformAdapter::disconnect
/// [`send`]: PlatformAdapter::send
/// [`get_chat_info`]: PlatformAdapter::get_chat_info
/// [`supports_code_blocks`]: PlatformAdapter::supports_code_blocks
/// [`supports_status_text`]: PlatformAdapter::supports_status_text
/// [`supports_async_delivery`]: PlatformAdapter::supports_async_delivery
/// [`splits_long_messages`]: PlatformAdapter::splits_long_messages
/// [`typed_command_prefix`]: PlatformAdapter::typed_command_prefix
/// [`supports_inchannel_continuable`]: PlatformAdapter::supports_inchannel_continuable
/// [`interactive_resume`]: PlatformAdapter::interactive_resume
/// [`requires_edit_finalize`]: PlatformAdapter::requires_edit_finalize
/// [`edit_message`]: PlatformAdapter::edit_message
/// [`delete_message`]: PlatformAdapter::delete_message
/// [`send_typing`]: PlatformAdapter::send_typing
/// [`format_message`]: PlatformAdapter::format_message
/// [`truncate_message`]: PlatformAdapter::truncate_message
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// The platform this adapter serves (upstream `self.platform`).
    fn platform(&self) -> Platform;

    /// Connect to the platform and start receiving messages. `is_reconnect`
    /// is false on a cold first boot and true when the reconnect watcher is
    /// re-establishing a previously running platform (adapters that buffer a
    /// server-side update queue should preserve it then). Returns true if
    /// the connection was successful.
    async fn connect(&self, is_reconnect: bool) -> anyhow::Result<bool>;

    /// Disconnect from the platform.
    async fn disconnect(&self) -> anyhow::Result<()>;

    /// Send a message to a chat. `reply_to` optionally names a message id to
    /// reply to; `metadata` carries additional platform-specific options.
    async fn send(
        &self,
        chat_id: &str,
        content: &str,
        reply_to: Option<&str>,
        metadata: Option<&serde_json::Value>,
    ) -> SendResult;

    /// Get information about a chat/channel. Returns a map with at least
    /// `name` (chat name) and `type` ("dm", "group", "channel").
    async fn get_chat_info(
        &self,
        chat_id: &str,
    ) -> anyhow::Result<serde_json::Map<String, serde_json::Value>>;

    /// Whether this platform renders triple-backtick fenced code blocks.
    fn supports_code_blocks(&self) -> bool {
        false
    }

    /// Whether this adapter's typing indicator renders TEXT (a live status
    /// line) rather than a native textless bubble.
    fn supports_status_text(&self) -> bool {
        false
    }

    /// Whether this adapter can deliver an ASYNC notification back to the
    /// agent AFTER a turn ends (persistent outbound channel). False for
    /// stateless request/response adapters.
    fn supports_async_delivery(&self) -> bool {
        true
    }

    /// Whether this adapter's `send()` splits long content into multiple
    /// messages via `truncate_message()` natively.
    fn splits_long_messages(&self) -> bool {
        false
    }

    /// The command prefix users can always TYPE on this platform to reach
    /// commands ("/" on most platforms; "!" where clients intercept "/").
    fn typed_command_prefix(&self) -> &str {
        "/"
    }

    /// Whether this adapter supports the `in_channel` continuable-cron
    /// surface. Default false: unsupported platforms fail SAFE.
    fn supports_inchannel_continuable(&self) -> bool {
        false
    }

    /// Whether a human is interactively present on this platform to answer a
    /// "session restored — what next?" prompt.
    fn interactive_resume(&self) -> bool {
        true
    }

    /// Whether the adapter requires an explicit `finalize=true` edit to
    /// close out the message lifecycle (upstream `REQUIRES_EDIT_FINALIZE`;
    /// rich card / AI assistant surfaces set true).
    fn requires_edit_finalize(&self) -> bool {
        false
    }

    /// Edit a previously sent message. Optional — platforms that don't
    /// support editing return `success=false` and callers fall back to
    /// sending a new message. `finalize` signals the last edit in a
    /// streaming sequence (most platforms treat it as a no-op).
    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        content: &str,
        finalize: bool,
    ) -> SendResult {
        let _ = (chat_id, message_id, content, finalize);
        SendResult::err("Not supported")
    }

    /// Delete a previously sent message. Optional — platforms without a
    /// deletion API return false and callers leave the message in place.
    async fn delete_message(&self, chat_id: &str, message_id: &str) -> bool {
        let _ = (chat_id, message_id);
        false
    }

    /// Send a typing indicator. Override where the platform supports it;
    /// `metadata` carries platform-specific context (e.g. thread_id for
    /// Slack).
    async fn send_typing(&self, chat_id: &str, metadata: Option<&serde_json::Value>) {
        let _ = (chat_id, metadata);
    }

    /// Format a message for this platform (platform-specific markup).
    /// Default returns the content as-is.
    fn format_message(&self, content: &str) -> String {
        content.to_string()
    }

    /// Split a long message into chunks, preserving code-block boundaries
    /// (upstream staticmethod; see the free function [`truncate_message`]
    /// (`crate::base::truncate_message`) for details).
    fn truncate_message(
        &self,
        content: &str,
        max_length: usize,
        len_fn: Option<&dyn Fn(&str) -> usize>,
    ) -> Vec<String> {
        truncate_message(content, max_length, len_fn)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(text: &str) -> MessageEvent {
        MessageEvent::new(text, SessionSource::new(Platform::Telegram, "12345"))
    }

    // ---- MessageEvent command helpers ----

    #[test]
    fn is_command_detects_leading_slash() {
        assert!(event("/new").is_command());
        assert!(!event("hello").is_command());
        assert!(!event(" /new").is_command());
    }

    #[test]
    fn get_command_extracts_and_lowercases() {
        assert_eq!(event("/new").get_command().as_deref(), Some("new"));
        assert_eq!(event("/FOO args here").get_command().as_deref(), Some("foo"));
        assert_eq!(event("/cmd@MyBot args").get_command().as_deref(), Some("cmd"));
        assert_eq!(event("hello").get_command(), None);
        // Bare "/" yields the empty command name, like upstream.
        assert_eq!(event("/").get_command().as_deref(), Some(""));
    }

    #[test]
    fn get_command_rejects_file_paths() {
        assert_eq!(event("/path/to/file").get_command(), None);
        assert_eq!(event("/usr/bin/env x").get_command(), None);
    }

    #[test]
    fn get_command_args_returns_remainder() {
        assert_eq!(event("/cmd a  b").get_command_args(), "a  b");
        assert_eq!(event("/cmd").get_command_args(), "");
        assert_eq!(event("/cmd   ").get_command_args(), "");
        // Non-commands pass through unchanged (no dash mapping).
        assert_eq!(event("hello — world").get_command_args(), "hello — world");
    }

    #[test]
    fn get_command_args_maps_ios_dashes() {
        assert_eq!(event("/cmd \u{2014}\u{2014}flag").get_command_args(), "--flag");
        assert_eq!(event("/cmd \u{2014}flag").get_command_args(), "--flag");
        assert_eq!(event("/cmd a\u{2013}b").get_command_args(), "a-b");
    }

    // ---- classify_send_error / is_chat_level_not_found ----

    #[test]
    fn classify_send_error_matches_upstream_vocabulary() {
        assert_eq!(classify_send_error(None, "Message is too long"), "too_long");
        assert_eq!(classify_send_error(None, "MESSAGE_TOO_LONG"), "too_long");
        assert_eq!(
            classify_send_error(None, "Bad Request: can't parse entities: Can't find end"),
            "bad_format"
        );
        assert_eq!(
            classify_send_error(None, "Forbidden: bot was blocked by the user"),
            "forbidden"
        );
        assert_eq!(classify_send_error(None, "Bad Request: chat not found"), "not_found");
        assert_eq!(classify_send_error(None, "thread not found"), "not_found");
        assert_eq!(
            classify_send_error(None, "Too Many Requests: retry after 5"),
            "rate_limited"
        );
        assert_eq!(classify_send_error(None, "ConnectionResetError"), "transient");
        assert_eq!(classify_send_error(None, "broken pipe"), "transient");
        assert_eq!(classify_send_error(None, ""), "unknown");
        assert_eq!(classify_send_error(None, "something odd"), "unknown");
        for kind in ["too_long", "unknown"] {
            assert!(SEND_ERROR_KINDS.contains(&kind));
        }
    }

    #[test]
    fn classify_send_error_reads_error_display() {
        let err = std::io::Error::other("Chat not found");
        let dyn_err: &(dyn std::error::Error + 'static) = &err;
        assert_eq!(classify_send_error(Some(dyn_err), ""), "not_found");
    }

    #[test]
    fn chat_level_not_found_distinguishes_blast_radius() {
        assert!(is_chat_level_not_found(None, "Bad Request: chat not found"));
        assert!(!is_chat_level_not_found(None, "thread not found"));
        // Sub-chat reading wins when both are present.
        assert!(!is_chat_level_not_found(None, "chat not found; thread not found"));
        assert!(!is_chat_level_not_found(None, "totally different"));
    }

    // ---- truncate_message ----

    #[test]
    fn truncate_short_content_passes_through() {
        assert_eq!(truncate_message("hello", 4096, None), vec!["hello".to_string()]);
    }

    #[test]
    fn truncate_appends_chunk_indicators_and_respects_max_length() {
        let content = "word ".repeat(60); // 300 cp
        let chunks = truncate_message(&content, 100, None);
        assert!(chunks.len() > 1);
        let total = chunks.len();
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(chunk.chars().count() <= 100, "chunk {i} too long: {chunk:?}");
            assert!(
                chunk.ends_with(&format!("({}/{})", i + 1, total)),
                "chunk {i} missing indicator: {chunk:?}"
            );
        }
        // No content lost (modulo the whitespace consumed at split points).
        let rejoined: String = chunks
            .iter()
            .map(|c| c.rsplit_once(" (").unwrap().0.to_string() + " ")
            .collect();
        assert_eq!(
            rejoined.split_whitespace().count(),
            content.split_whitespace().count()
        );
    }

    #[test]
    fn truncate_reopens_code_fences_across_chunks() {
        let mut content = String::from("```python\n");
        for _ in 0..6 {
            content.push_str("aaaa bbbb cccc dddd\n");
        }
        content.push_str("```\nafter");
        let chunks = truncate_message(&content, 60, None);
        assert!(chunks.len() > 1);
        // First chunk closes the fence before its indicator...
        let first_body = chunks[0].rsplit_once(" (").unwrap().0;
        assert!(first_body.ends_with("\n```"), "no fence close: {:?}", chunks[0]);
        // ...and the continuation reopens it with the language tag.
        assert!(
            chunks[1].starts_with("```python\n"),
            "no fence reopen: {:?}",
            chunks[1]
        );
    }

    #[test]
    fn truncate_honors_custom_utf16_length() {
        let content = "\u{1F600}".repeat(100); // each emoji = 2 UTF-16 units
        assert_eq!(utf16_len(&content), 200);
        let chunks = truncate_message(&content, 50, Some(&utf16_len));
        assert!(chunks.len() > 1);
        let mut emoji_count = 0;
        for chunk in &chunks {
            assert!(utf16_len(chunk) <= 50, "chunk exceeds utf16 budget: {chunk:?}");
            emoji_count += chunk.chars().filter(|&c| c == '\u{1F600}').count();
        }
        assert_eq!(emoji_count, 100, "no emoji may be lost");
    }

    // ---- PlatformAdapter defaults ----

    struct NullAdapter;

    #[async_trait]
    impl PlatformAdapter for NullAdapter {
        fn platform(&self) -> Platform {
            Platform::Local
        }
        async fn connect(&self, _is_reconnect: bool) -> anyhow::Result<bool> {
            Ok(true)
        }
        async fn disconnect(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send(
            &self,
            _chat_id: &str,
            _content: &str,
            _reply_to: Option<&str>,
            _metadata: Option<&serde_json::Value>,
        ) -> SendResult {
            SendResult::ok(Some("1".into()))
        }
        async fn get_chat_info(
            &self,
            _chat_id: &str,
        ) -> anyhow::Result<serde_json::Map<String, serde_json::Value>> {
            Ok(serde_json::Map::new())
        }
    }

    #[tokio::test]
    async fn adapter_defaults_match_upstream() {
        let adapter = NullAdapter;
        assert!(!adapter.supports_code_blocks());
        assert!(!adapter.supports_status_text());
        assert!(adapter.supports_async_delivery());
        assert!(!adapter.splits_long_messages());
        assert_eq!(adapter.typed_command_prefix(), "/");
        assert!(!adapter.supports_inchannel_continuable());
        assert!(adapter.interactive_resume());
        assert!(!adapter.requires_edit_finalize());

        let edit = adapter.edit_message("c", "m", "text", true).await;
        assert!(!edit.success);
        assert_eq!(edit.error.as_deref(), Some("Not supported"));
        assert!(!adapter.delete_message("c", "m").await);
        adapter.send_typing("c", None).await; // no-op
        assert_eq!(adapter.format_message("**hi**"), "**hi**");
        assert_eq!(adapter.truncate_message("hi", 4096, None), vec!["hi".to_string()]);
    }

    #[test]
    fn message_type_wire_values() {
        assert_eq!(serde_json::to_string(&MessageType::Command).unwrap(), "\"command\"");
        let parsed: MessageType = serde_json::from_str("\"voice\"").unwrap();
        assert_eq!(parsed, MessageType::Voice);
        assert_eq!(MessageType::default(), MessageType::Text);
        assert_eq!(MessageType::Sticker.as_str(), "sticker");
    }
}
