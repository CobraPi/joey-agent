# joey-gateway — platform-neutral messaging spine

`joey-gateway` is the platform-neutral messaging core of the joey-agent workspace: a port of upstream Hermes Agent's `gateway/` package that defines everything a chat platform integration needs — the `Platform` vocabulary, the `SessionSource` identity record, the deterministic session-key grammar, the normalized `MessageEvent`/`SendResult` envelope pair, send-error classification, fence-preserving message truncation, WhatsApp JID/LID identity canonicalisation, and the `PlatformAdapter` trait that concrete integrations implement. The crate is deliberately free of any specific platform SDK: it is the shared spine that adapters (and the session store, cron delivery, and system-prompt assembly) are built against.

> See also: [../gateway.md](../gateway.md)

## Overview

The crate mirrors the upstream module layout file-for-file where it exists: `config.rs` carries the `Platform` enum (upstream `gateway/config.py`), `session.rs` carries `SessionSource` and the session-key builder (upstream `gateway/session.py`), `whatsapp_identity.rs` carries WhatsApp identity canonicalisation (upstream `gateway/whatsapp_identity.py`), and `base.rs` carries `MessageEvent`, `SendResult`, send-error classification, truncation, and the `PlatformAdapter` trait (upstream `gateway/platforms/base.py::BasePlatformAdapter`).

One important scoping fact: **no concrete platform adapters ship in this crate**. `lib.rs` states it verbatim — concrete adapters (Telegram, Discord, Slack, …) "are added incrementally behind the trait; none ship in this first port, matching the deferral plan". The only `PlatformAdapter` implementation in the tree is the test-only `NullAdapter` inside `base.rs`'s `#[cfg(test)]` module, used to pin the trait's default capabilities. Everything else here is the vocabulary and the rules.

Because it sits low in the workspace DAG, the crate's only workspace dependency is `joey-core` (home directory resolution for the WhatsApp lid-mapping lookups, and the `HomeOverrideGuard` used by tests).

## Module map

Six files (five Rust modules + the manifest):

| File | Purpose |
|---|---|
| `src/lib.rs` | Crate root; module declarations, public re-exports, doc map, `testutil::lock_home()` test mutex |
| `src/config.rs` | `Platform` enum (24 named variants + `Other(String)`), wire values, case-insensitive parsing, serde as the lowercase wire string |
| `src/session.rs` | `SessionSource` (20 public fields, upstream `to_dict`/`from_dict` serde parity), `SessionKeyOptions`, `build_session_key`, `build_session_key_with_defaults` |
| `src/whatsapp_identity.rs` | `normalize_whatsapp_identifier`, `expand_whatsapp_aliases`, `canonical_whatsapp_identifier`; walks the bridge's `lid-mapping-*.json` files |
| `src/base.rs` | `MessageType`, `AutoSkill`, `MessageEvent`, `SendResult`, `SEND_ERROR_KINDS`, `classify_send_error`, `is_chat_level_not_found`, `RETRYABLE_ERROR_PATTERNS`, `utf16_len`, `TRUNCATE_DEFAULT_MAX_LENGTH`, `truncate_message`, the `PlatformAdapter` trait |
| `Cargo.toml` | Manifest; workspace deps: `joey-core` only |

## Public API

### `Platform` (config.rs)

An enum with **24 named variants plus the catch-all `Other(String)`**. `Display`, `Serialize`, and `Deserialize` all use the lowercase wire value (upstream `platform.value`):

| Variant | Wire value | Variant | Wire value |
|---|---|---|---|
| `Local` | `local` | `Webhook` | `webhook` |
| `Telegram` | `telegram` | `MsgraphWebhook` | `msgraph_webhook` |
| `Discord` | `discord` | `Feishu` | `feishu` |
| `Whatsapp` | `whatsapp` | `Wecom` | `wecom` |
| `WhatsappCloud` | `whatsapp_cloud` | `WecomCallback` | `wecom_callback` |
| `Slack` | `slack` | `Weixin` | `weixin` |
| `Signal` | `signal` | `Bluebubbles` | `bluebubbles` |
| `Mattermost` | `mattermost` | `Qqbot` | `qqbot` |
| `Matrix` | `matrix` | `Yuanbao` | `yuanbao` |
| `Homeassistant` | `homeassistant` | `Relay` | `relay` (EXPERIMENTAL relay adapter) |
| `Email` | `email` | `Other(value)` | the stored (normalized) name |
| `Sms` | `sms` | | |
| `Dingtalk` | `dingtalk` | | |
| `ApiServer` | `api_server` | | |

Wire rules:

- Parsing (`FromStr`) **trims surrounding whitespace and lowercases** before matching (upstream `_missing_` normalization), so `"TELEGRAM"` and `" Telegram "` both parse to `Platform::Telegram`.
- Unknown non-empty names parse to `Platform::Other(normalized)` — upstream consults a plugin registry here; with no registry in the port, any non-empty name is accepted (the one intentional relaxation vs upstream, noted in the module docs).
- Empty/whitespace-only names are rejected with `InvalidPlatformError` (upstream: `Platform("")` raises `ValueError`).
- Serde round-trips the wire string: `Platform::WhatsappCloud` ⇄ `"whatsapp_cloud"`; `"irc"` ⇄ `Platform::Other("irc")`; `""` fails to deserialize.

### `SessionSource` (session.rs)

Describes where a message originated. Used to (1) route responses back to the right place, (2) inject context into the system prompt, and (3) track origin for cron job delivery. All fields:

| Field | Type | Notes |
|---|---|---|
| `platform` | `Platform` | always serialized |
| `chat_id` | `String` | always serialized; deserialization coerces via Python `str()` (numbers → their digits) |
| `chat_name` | `Option<String>` | always serialized (`null` when absent) |
| `chat_type` | `String` | `"dm"`, `"group"`, `"channel"`, `"thread"`; default `"dm"` |
| `user_id` | `Option<String>` | always serialized |
| `user_name` | `Option<String>` | always serialized |
| `thread_id` | `Option<String>` | forum topics, Discord threads, …; always serialized |
| `chat_topic` | `Option<String>` | channel topic/description (Discord, Slack); always serialized |
| `user_id_alt` | `Option<String>` | platform-stable alt ID (Signal UUID, Feishu union_id); only when truthy |
| `chat_id_alt` | `Option<String>` | Signal group internal ID; only when truthy |
| `is_bot` | `bool` | author is a bot/webhook (Discord); **wire-invisible** |
| `scope_id` | `Platform`-neutral scope discriminator | Discord guild / Slack workspace / Matrix server; canonical name (D-Q2.5) |
| `guild_id` | `Option<String>` | **deprecated legacy alias** for `scope_id` (D-Q2.5), dual-written/dual-read during migration |
| `parent_chat_id` | `Option<String>` | parent channel when `chat_id` is a thread; only when truthy |
| `message_id` | `Option<String>` | triggering message id (pin/reply/react); only when truthy |
| `role_authorized` | `bool` | access granted via role, not user ID; **wire-invisible** |
| `profile` | `Option<String>` | profile this inbound message routes to in a multiplexing gateway; only when truthy |
| `auto_thread_created` | `bool` | true only for gateway-auto-created Discord threads; serialized only when `true` |
| `auto_thread_initial_name` | `Option<String>` | safe-rename target for auto threads; only when truthy |
| `delivered_via_upstream_relay` | `bool` | per-instance-authenticated relay trust signal; **wire-invisible by design** (a peer must never forge it) |

Serde matches upstream `to_dict`/`from_dict` exactly:

- The first eight keys (`platform` … `chat_topic`) are **always emitted**, with `null` for absent optionals.
- The remaining keys are emitted **only when truthy** (Python truthiness: `Some("")` counts as absent).
- `is_bot`, `role_authorized`, and `delivered_via_upstream_relay` are **never serialized and never restored** from a dict — they are in-process trust signals.
- `scope_id`/`guild_id` are dual-written on serialize (both keys carry the same value) and dual-read on deserialize: `scope_id` wins, an **empty-string** `scope_id` must not shadow a real legacy `guild_id`, and `reconcile_scope_alias()` mirrors whichever was provided onto the other after construction (port of `__post_init__`).
- Unknown keys are ignored on read.

`SessionSource::new(platform, chat_id)` constructs with upstream dataclass defaults (`chat_type="dm"`, everything else absent/false). `description()` renders the human-readable source ("CLI terminal" for `Local`; else `"DM with {user}"` / `"group: {name}"` / `"channel: {name}"`, plus `", thread: {id}"` when present).

### `SessionKeyOptions`

The two keyword parameters of upstream `build_session_key`, with defaults:

| Option | Default | Meaning |
|---|---|---|
| `group_sessions_per_user` | `true` | isolate group participants into per-user sessions |
| `thread_sessions_per_user` | `false` | apply that isolation inside threads too (default: threads are shared) |

### `MessageType`, `AutoSkill` (base.rs)

`MessageType` has nine variants, serialized lowercase (`#[serde(rename_all = "lowercase")]`): `Text` (default), `Location`, `Photo`, `Video`, `Audio`, `Voice`, `Document`, `Sticker`, `Command` (`/command` style). `as_str()` returns the wire value.

`AutoSkill` models upstream `auto_skill: Optional[str | list[str]]` — `One(String)` for a single topic/channel-bound skill, `Many(Vec<String>)` for an ordered list.

### `MessageEvent`

The normalized incoming-message representation all adapters produce. Fields:

| Field | Type | Notes |
|---|---|---|
| `text` | `String` | message content |
| `message_type` | `MessageType` | defaults to `Text` |
| `source` | `SessionSource` | routing/identity |
| `raw_message` | `serde_json::Value` | original platform data (upstream `Any`) |
| `message_id` | `Option<String>` | |
| `platform_update_id` | `Option<i64>` | Telegram `update_id`; used by `/restart` to advance the offset |
| `media_urls` / `media_types` | `Vec<String>` | attachments as local file paths (vision tool access) |
| `reply_to_message_id` | `Option<String>` | reply context |
| `reply_to_text` | `Option<String>` | text of the replied-to message |
| `reply_to_author_id` / `reply_to_author_name` | `Option<String>` | replied-to author |
| `reply_to_is_own_message` | `bool` | user replied to this bot's message |
| `auto_skill` | `Option<AutoSkill>` | topic/channel skill binding |
| `channel_prompt` | `Option<String>` | per-channel ephemeral system prompt; applied at API call time, never persisted |
| `channel_context` | `Option<String>` | history-backfill context kept separate from `text` |
| `internal` | `bool` | synthetic events (background-process completion) bypass user authorization |
| `metadata` | `serde_json::Map` | free-form per-event signals (e.g. `whatsapp_from_owner=true`) |
| `timestamp` | `DateTime<Local>` | defaults to now |

Command helpers mirror upstream exactly:

- `is_command()` — true iff `text` starts with `/` (a leading space disqualifies).
- `get_command()` — first whitespace token, leading `/` stripped, lowercased, `@botname` suffix removed (e.g. `/cmd@MyBot` → `cmd`); names that still **contain `/` are rejected with `None` (file paths are not commands); bare `/` yields the empty name, like upstream.
- `get_command_args()` — the whitespace remainder after the command; non-command text is returned unchanged. iOS auto-correct dash mapping is applied **to commands only**: `— —`/`—` (em dash) → `--`, `–` (en dash) → `-`.

### `SendResult`

| Field | Type | Notes |
|---|---|---|
| `success` | `bool` | |
| `message_id` | `Option<String>` | |
| `error` | `Option<String>` | |
| `raw_response` | `serde_json::Value` | adapter-specific metadata; cross-layer contracts documented at producer/consumer (e.g. Telegram `partial_overflow`) |
| `retryable` | `bool` | transient connection error — the base retries automatically |
| `retry_after` | `Option<f64>` | server-requested delay in seconds (e.g. Telegram FloodWait `retry_after`); honored over default backoff |
| `continuation_message_ids` | `Vec<String>` | extra message ids in send order when the payload was split; `message_id` is the LAST visible id |
| `error_kind` | `Option<String>` | one of `SEND_ERROR_KINDS`, set only when `success` is false |

Constructors: `SendResult::ok(message_id)` and `SendResult::err(error)` fill upstream defaults for the remaining fields.

### Send-error classification

`SEND_ERROR_KINDS` (7 machine-readable categories, platform-neutral):

| Kind | Meaning |
|---|---|
| `too_long` | content exceeded the platform's per-message cap |
| `bad_format` | platform rejected the markup/entities |
| `forbidden` | blocked/kicked/no permission — the bot cannot reach the target |
| `not_found` | target chat/thread/message no longer exists |
| `rate_limited` | platform throttled the send (flood control) |
| `transient` | connection-level failure safe to retry |
| `unknown` | classification matched nothing (conservative default) |

`classify_send_error(error, error_text)` lowercases the error text(s) into a single blob (the explicit `error_text` plus the error's `Display`, joined and lowercased — the port of upstream `_error_blob`, the single source of truth so the two classifiers can never drift) and matches, in order:

| Result kind | Matched substrings (lowercased blob) |
|---|---|
| `too_long` | `message_too_long`, `too long`, `message is too long` |
| `bad_format` | `can't parse entities`, `cant parse entities`, `can't find end`, `unsupported start tag`, (`entity` AND `parse`), (`bad request` AND `entit`) |
| `forbidden` | `forbidden`, `bot was blocked`, `blocked by the user`, `user is deactivated`, `not enough rights`, `have no rights`, `not a member` |
| `not_found` | any chat-level or sub-chat not-found substring (tables below) |
| `rate_limited` | `flood`, `too many requests`, `retry after`, `rate limit` |
| `transient` | any `RETRYABLE_ERROR_PATTERNS` entry (plus the redundant `connecttimeout`) |
| `unknown` | empty blob or nothing matched — conservative, never mistake an unclassified failure for a benign one |

Rust errors carry no portable runtime type name (upstream also appends the exception class name), so callers must fold type information into the error's `Display` text.

`is_chat_level_not_found(error, error_text)` distinguishes blast radius using two substring sets:

| Set | Substrings | Meaning |
|---|---|---|
| Chat-level (`_CHAT_LEVEL_NOT_FOUND_SUBSTRINGS`) | `chat not found` | the **whole chat** is gone — the delivery target is dead |
| Sub-chat (`_SUBCHAT_NOT_FOUND_SUBSTRINGS`) | `message to edit not found`, `message to reply not found`, `thread not found`, `topic_deleted`, `message_id_invalid` | a deleted topic / edited-away message — the **parent chat is still reachable** |

When both a chat-level and a sub-chat marker appear in the same blob, the **sub-chat reading wins** (conservative: never kill a chat that may still be reachable); `"totally different"` text is simply not chat-level.

Worked examples (from the crate's tests): `"Message is too long"` → `too_long`; `"Bad Request: can't parse entities: Can't find end"` → `bad_format`; `"Forbidden: bot was blocked by the user"` → `forbidden`; `"Bad Request: chat not found"` → `not_found` (and chat-level); `"thread not found"` → `not_found` (not chat-level); `"Too Many Requests: retry after 5"` → `rate_limited`; `"ConnectionResetError"` / `"broken pipe"` → `transient`; `""` / `"something odd"` → `unknown`; `"chat not found; thread not found"` → **not** chat-level (sub-chat wins).

`RETRYABLE_ERROR_PATTERNS` — 9 transient-connection substrings:

`connecterror`, `connectionerror`, `connectionreset`, `connectionrefused`, `connecttimeout`, `network`, `broken pipe`, `remotedisconnected`, `eoferror`

Plain `"timeout"` is **intentionally excluded**: a read/write timeout on a non-idempotent call may have reached the server — retrying risks duplicate delivery. `connecttimeout` is safe because the connection was never established.

### Truncation

- `utf16_len(s)` — UTF-16 code-unit count (Telegram's 4096 limit is measured in UTF-16 units, so non-BMP characters cost two each).
- `TRUNCATE_DEFAULT_MAX_LENGTH = 4096` — upstream default `max_length`.
- `truncate_message(content, max_length, len_fn)` — splits long content into chunks while **preserving code-block boundaries**: when a split falls inside a triple-backtick block, the fence is closed (`\n``` ```) at the end of the chunk and reopened with the original language tag at the start of the next. Natural split points prefer newlines, then spaces; splits inside inline-code spans (odd unescaped backtick count) are shifted before the backtick; at least one codepoint is always consumed so the loop advances. `len_fn` measures length (`None` = Unicode codepoints, Python `len`; pass `utf16_len` for UTF-16 platforms). Multi-chunk output gets `(i/n)` indicators appended to each chunk (e.g. `(1/3)`), with a 10-codepoint reserve budgeted for the indicator.

### WhatsApp identity (whatsapp_identity.rs)

- `normalize_whatsapp_identifier(value)` — strip JID/LID/device/plus syntax to the bare numeric identifier: trim, remove the first `+` anywhere, take the prefix before the first `:`, then before the first `@`. `"60123456789@s.whatsapp.net"`, `"60123456789:47@s.whatsapp.net"`, `"999999999999999@lid"`, and `"+601****6789"` all normalize to comparable digits.
- `expand_whatsapp_aliases(identifier)` — BFS through the bridge's mapping files under `<JOEY_HOME>/whatsapp/session/`, i.e. `platforms/whatsapp/session` / `whatsapp/session` via `joey_core::constants::joey_dir`. For each id, it reads **`lid-mapping-{id}.json` and `lid-mapping-{id}_reverse.json`** and enqueues the mapped ids transitively. The result always includes the normalized input (when it passes the safe-identifier gate `^[A-Za-z0-9@.+\-]+$`, a defense against path traversal in the filename); empty normalization yields an empty set.
- `canonical_whatsapp_identifier(identifier)` — expand the alias set, then pick the **shortest (then lexicographically smallest)** alias (`min(aliases, key=(len, c))` upstream) as the stable identity. With no mapping files yet it degrades to the normalized input. `build_session_key` uses this for WhatsApp DM chat_ids and group participant ids so phone-JID/LID alias flips never split one human across two sessions.

## Session-key grammar

`build_session_key(source, profile, opts)` is the single source of truth (port of `gateway/session.py::build_session_key`). Exact grammar:

```
{ns}:{platform}:{chat_type}[:{chat_id}[:{thread_id}][:{participant}]]
```

- `{ns}` — profile namespace (port of `_session_key_namespace`): default profile (`None`/`""`/`"default"`) → `agent:main`, byte-identical to every key ever generated; a named profile `coder` → `agent:coder`. The profile is a **caller decision** — the builder never reads `source.profile` implicitly; only a multiplexing gateway passes a non-default profile.
- `{platform}` — the lowercase wire value (`platform.as_str()`).
- Empty-string optionals behave exactly like `None` (Python falsiness) and are skipped — a key never contains an empty segment.

DM rules (`chat_type == "dm"`): DMs include `chat_id` when present (each private conversation isolated); `thread_id` further differentiates threaded DMs. Without a `chat_id`, the fallback chain is `user_id_alt` > `user_id` > `thread_id` > the bare per-platform sink `...:{platform}:dm` — a cross-user history-bleed guard. `user_id` is never mixed into a DM key that already has a `chat_id`.

Group/channel rules (any non-`"dm"` chat_type): `chat_id` identifies the parent chat (skipped when empty), `thread_id` differentiates threads within it, and the participant id (`user_id_alt` > `user_id`) is appended when isolation is active — which is `group_sessions_per_user` **unless** a `thread_id` is present and `thread_sessions_per_user` is false (default), in which case threads are deliberately **shared** and the participant is NOT appended.

WhatsApp chat_ids and participant ids are canonicalized (see above) on both the DM and group paths; non-WhatsApp platforms are never touched.

Examples (all verified by the crate's tests):

| Source | Key |
|---|---|
| Telegram DM, `chat_id="99"` | `agent:main:telegram:dm:99` |
| Telegram DM, `chat_id="99"`, `thread_id="topic-1"` | `agent:main:telegram:dm:99:topic-1` |
| Telegram DM, no chat_id, `user_id="jordan"` | `agent:main:telegram:dm:jordan` |
| Telegram DM, no chat_id, `user_id_alt="alt"`, `user_id="primary"` | `agent:main:telegram:dm:alt` |
| Telegram DM, no chat_id, `thread_id="7"` only | `agent:main:telegram:dm:7` |
| Telegram DM, nothing at all | `agent:main:telegram:dm` |
| Discord group `guild-123`, `user_id="alice"` (defaults) | `agent:main:discord:group:guild-123:alice` |
| Discord group `guild-123`, isolation off | `agent:main:discord:group:guild-123` |
| Telegram group `12345`, `thread_id="777"`, `user_id="user99"` (defaults: shared threads) | `agent:main:telegram:group:12345:777` |
| Same source, `thread_sessions_per_user=true` | `agent:main:telegram:group:-1002285219667:17585:42` shape — participant appended |
| Slack channel `C123`, `user_id="u1"` | `agent:main:slack:channel:C123:u1` |
| Named profile `coder`, Telegram DM `99` | `agent:coder:telegram:dm:99` |

`build_session_key_with_defaults(source)` is the convenience wrapper matching upstream's zero-keyword call shape (default namespace + default options).

## PlatformAdapter trait

Port of upstream `BasePlatformAdapter` (shorter Rust trait name). Required methods:

| Method | Signature | Upstream mapping |
|---|---|---|
| `platform()` | `fn platform(&self) -> Platform` | `self.platform` |
| `connect(is_reconnect)` | `async fn connect(&self, bool) -> Result<bool>` | `connect(*, is_reconnect)`; false on cold boot, true when the reconnect watcher re-establishes (buffering adapters should preserve server-side queues then) |
| `disconnect()` | `async fn disconnect(&self) -> Result<()>` | `disconnect` |
| `send(chat_id, content, reply_to, metadata)` | `async fn send(...) -> SendResult` | `send(chat_id, content, reply_to=None, metadata=None)` |
| `get_chat_info(chat_id)` | `async fn get_chat_info(...) -> Result<Map>` | dict with at least `name` and `type` (`"dm"`/`"group"`/`"channel"`) |

Capability getters (upstream class attributes, same defaults):

| Getter | Default | Meaning |
|---|---|---|
| `supports_code_blocks()` | `false` | platform renders triple-backtick fenced blocks |
| `supports_status_text()` | `false` | typing indicator renders TEXT (live status line), not a textless bubble |
| `supports_async_delivery()` | `true` | can deliver an async notification after a turn ends (false for stateless request/response adapters) |
| `splits_long_messages()` | `false` | `send()` natively splits long content via `truncate_message()` |
| `typed_command_prefix()` | `"/"` | the prefix users can always TYPE (`"!"` where clients intercept `/`) |
| `supports_inchannel_continuable()` | `false` | supports the `in_channel` continuable-cron surface; default fails SAFE |
| `interactive_resume()` | `true` | a human is interactively present to answer a "session restored — what next?" prompt |
| `requires_edit_finalize()` | `false` | upstream `REQUIRES_EDIT_FINALIZE`; rich card / AI assistant surfaces set true |

Optional methods with default bodies mirroring upstream: `edit_message(...)` → `SendResult::err("Not supported")`; `delete_message(...)` → `false`; `send_typing(...)` → no-op; `format_message(content)` → identity; `truncate_message(...)` → the shared fence-preserving splitter. The rest of the upstream surface (message-handler registration, media pipeline, typing loops, busy/debounce handling, retry wrapper, ephemeral deletes, handoff threads, …) is deferred along with the platform adapters themselves.

## Message flow & error handling

The crate defines the envelope, not the pipeline: an adapter turns a platform payload into a `MessageEvent`, the agent produces a reply, and the adapter's `send`/`edit_message` return a `SendResult`. Two fields exist specifically for the caller's retry policy:

- `SendResult.retryable` — mark transient connection failures so the surrounding runtime retries automatically;
- `SendResult.retry_after` — server-requested delay in seconds (flood control), honored instead of the default backoff when present.

Producers should set `error_kind` via `classify_send_error`, and consumers deciding whether a delivery target is permanently dead should consult `is_chat_level_not_found` rather than string-matching themselves. There is **no queueing, retry loop, or dispatch machinery in this crate** — those live with the adapters/gateway runtime that is yet to be ported.

## Testing

Each module carries inline `#[cfg(test)]` unit tests that pin upstream behavior:

- `config.rs` — wire-value round-trips for all 24 named platforms, case-insensitive/trimming parse, `Other` normalization, empty-name rejection, serde wire values.
- `session.rs` — the full session-key grammar table above (DM, fallback chain, group/thread isolation, profile namespaces, WhatsApp canonicalisation), serde parity with `to_dict`/`from_dict` (always-emitted vs truthy-only keys, `scope_id`/`guild_id` dual-read with the empty-string rule, wire-invisible flags).
- `base.rs` — `is_command`/`get_command`/`get_command_args` (including the iOS dash mappings and file-path rejection), `classify_send_error` vocabulary, `is_chat_level_not_found` blast-radius cases, `truncate_message` (indicators, length budgets, fence reopen with language tag, UTF-16 budgets with no emoji lost), and `NullAdapter` pinning every trait default, plus `MessageType` wire values.
- `whatsapp_identity.rs` — normalization shapes, canonical-without-mappings degradation, and transitive `lid-mapping-*.json` walks. Tests that override the joey home share a process-global mutex (`testutil::lock_home`) so parallel threads can't observe each other's temporary homes.

Run with: `cargo test -p joey-gateway`.

## See also

- [../gateway.md](../gateway.md) — gateway subsystem overview
- [README.md](README.md) — the docs/features/ crate-by-crate index
- [joey-core.md](joey-core.md) — `~/.joey` home resolution and layered config used here
- [joey-cron.md](joey-cron.md) — cron delivery targets reference `SessionSource`
- [joey-cli.md](joey-cli.md) — the CLI consumer of session identity (`/platforms`, handoff surface)
