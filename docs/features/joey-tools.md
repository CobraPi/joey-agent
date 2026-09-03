# joey-tools — the Tool trait, registry, toolsets, and every built-in tool

`joey-tools` is the tool system of the joey-agent workspace: it defines the `Tool` trait and the `ToolRegistry` that dispatches calls, resolves toolsets (`web`, `file`, `coding`, `joey-cli`, …) exactly as upstream Hermes Agent's `toolsets.py` does, sanitizes tool JSON Schemas for strict backends, truncates and persists oversized tool results, and ships the self-contained built-in tools (file, terminal, process, todo, memory, web, skills, LSP, browser, vision, clarify, session-search, NeuroCode). It is a port of upstream's `tools/` package plus `toolsets.py`, keeping schemas, result envelopes, guidance strings, and on-disk formats byte-compatible where possible.

> See also: [../tools.md](../tools.md)

## Overview

Everything the model can *do* flows through this crate. `joey-agent-core`'s turn loop asks the registry for sanitized tool definitions, dispatches model tool-calls through `ToolRegistry::dispatch`, and applies the per-turn output budget; `joey-cli` wires the conditionally-registered tools (session DB, clarify channel, NeuroCode backend) and the platform toolsets. The crate sits in the middle of the DAG: it depends on `joey-core` (config, redaction, paths) and `joey-browser` (browser session), and everything above (`joey-agent-core`, `joey-cron`, `joey-mcp`, `joey-gateway`, `joey-cli`, `joey-tui`, …) may depend on it.

Design rules inherited from upstream and kept deliberately:

- **Explicit registration, no reflection.** All built-ins are registered by hand (`builtins.rs::register_all`); tools needing broader context (session DB, cron store, agent itself) are registered by higher crates that own it.
- **Guidance strings are verbatim ports.** Descriptions shown to the model come from upstream Python; `tests/schema_snapshots.rs` pins them.
- **Untrusted content is guarded** at every ingestion point: file read guards, terminal env sanitization + ANSI strip + redaction, URL SSRF/secret checks, schema sanitization.

## Module map

22 top-level `src/` files, 16 files under `src/tools/`, 5 test files:

| File | Purpose |
|---|---|
| `src/lib.rs` | Crate root; re-exports `Tool`, `ToolRegistry`, `ToolResult`, `ToolContext`, toolset resolvers |
| `src/registry.rs` | `Tool` trait, `ToolResult`, `ToolRegistry`, check() TTL cache, `sanitize_tool_error` |
| `src/builtins.rs` | Hand registration of every built-in; conditional registration helpers |
| `src/toolsets.rs` | Toolset table, `CORE_TOOLS`, `resolve`/`resolve_multiple`, platform auto-toolsets |
| `src/context.rs` | `ToolContext`, `SessionState` (read/dedup/patch-failure trackers), `TurnBudget` |
| `src/storage.rs` | Layer-2/3 result persistence (`<tmp>/joey-results/`), thresholds, preview builder |
| `src/truncate.rs` | Tool-output limits, read/search pagination normalizers, terminal head/tail truncation |
| `src/sanitize.rs` | JSON-Schema sanitizer for tool parameters |
| `src/sanitize_input.rs` | Pre-execution tool-input JSON validation (port of crush's `sanitizeToolInput`) |
| `src/fuzzy.rs` | 9-strategy fuzzy find-and-replace behind `patch`/`multi_edit` |
| `src/patch_parser.rs` | V4A patch format parser/applier (`V4aFileOps` trait, validate-then-apply) |
| `src/difflib.rs` | `SequenceMatcher` + `unified_diff` (Python `difflib` port) for diff rendering |
| `src/guards.rs` | File-safety guards: device paths, binary extensions, credential blocks, sensitive writes, ANSI strip |
| `src/url_safety.rs` | SSRF checks (`is_safe_url`), sensitive query-param detector |
| `src/pyjson.rs` | `json.dumps`-compatible serialization (Python separators, indent=2) |
| `src/file_tracker.rs` | Session file read/write tracking + diff generation (port of crush's filetracker) |
| `src/lsp.rs` | LSP client infrastructure: lazy server management, diagnostics collection |
| `src/highlight.rs` | Per-line syntax highlighting for diff rendering (shared by CLI and TUI) |
| `src/safe_commands.rs` | Read-only command auto-approval allowlist (port of crush's `safe.go`) |
| `src/completion.rs` | Smart-completion engine (@-context refs, path completion, fuzzy project files) |
| `src/vcs.rs` | Shared-store git checkpointing at `~/.joey/checkpoints/store` |

| `src/tools/` file | Purpose |
|---|---|
| `mod.rs` | Module root |
| `file_tools.rs` | `read_file`, `write_file`, `patch`, `multi_edit`, `search_files` |
| `terminal_tool.rs` | `terminal` (foreground/background/PTY, env sanitization, exit-code table) |
| `terminal_governor.rs` | Process-global terminal concurrency governor (feature 018) |
| `process_tool.rs` | `process` — manage background sessions (list/poll/log/wait/kill/write/submit/close) |
| `todo_tool.rs` | `todo` — per-session task list |
| `memory_tool.rs` | `memory` — persistent MEMORY.md / USER.md |
| `web_tools.rs` | `web_search` + `web_extract` (Tavily) |
| `browser_tools.rs` | 16 browser automation tools sharing a `BrowserHandle` |
| `vision_tools.rs` | `vision_analyze` |
| `skills_tool.rs` | `skills_list` + `skill_view` |
| `lsp_tools.rs` | `lsp_diagnostics`, `lsp_definition`, `lsp_references`, `lsp_symbols` |
| `session_search_tool.rs` | `session_search` (FTS5 + scroll mode) |
| `clarify_tool.rs` | `clarify` — structured questions to the user |
| `neurocode_tools.rs` | `neurocode_index/query/status/ingest` + `neurocode_search` (RAG) |

| Test file | Purpose |
|---|---|
| `tests/schema_snapshots.rs` | Pins every built-in tool's name/description/parameters against upstream-derived literals |
| `tests/terminal_streaming.rs` | Terminal async streaming regressions (result schema unchanged, `{output, exit_code, error}`) |
| `tests/terminal_governor.rs` | Governor admission contract tests (cap enforcement, FIFO fairness, interrupt) |
| `tests/rayon_terminal_e2e.rs` | E2E exercise of the terminal rayon post-processing path (ANSI strip + redaction) |
| `tests/process_reaper.rs` | Background reaper fills the session ring buffers; dead-session reaping |

Inline `#[cfg(test)]` unit tests live alongside every module (fuzzy strategy chains, sanitizer repairs, guard blocklists, envelopes).

## The Tool trait

Defined in `src/registry.rs`:

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;                                  // wire name, e.g. "read_file"
    fn toolset(&self) -> &str;                               // owning toolset, e.g. "file"
    fn description(&self) -> &str;                           // model-visible description
    fn parameters(&self) -> Value;                           // JSON Schema of the parameters object
    fn emoji(&self) -> &str { "" }                           // progress display; default empty
    fn max_result_chars(&self) -> Option<usize> {
        Some(crate::storage::DEFAULT_RESULT_SIZE_CHARS)      // 100_000
    }
    fn check(&self, _ctx: &ToolContext) -> bool { true }     // availability gate
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult;
}
```

`ToolResult` has exactly three variants — handlers return only text or a multimodal envelope; the dispatcher enforces the contract:

- `Text(String)` — rendered as-is in the tool message.
- `Multimodal(Vec<Value>)` — text + `image_url` parts; `to_content_string()` renders text parts, `[image]` placeholders for images.
- `Error(String)` — serialized as `{"error": "..."}` with Python separators.

`tool_error(message)` builds the upstream error shape. `sanitize_tool_error` (port of `model_tools._sanitize_tool_error`) cleans any error string before it reaches the model: it strips role tags (`</tool_call>`, `<system>`, …), opening/closing code fences, and CDATA sections; caps length at 2000 chars (adding `...`); and prefixes the result with `"[TOOL_ERROR] "`. Empty input yields `"[TOOL_ERROR] "`.

## ToolRegistry

`ToolRegistry` (a `BTreeMap<String, Arc<dyn Tool>>`) with:

- `register(tool)` — replaces any prior tool of the same name.
- `with_builtins()` — `register_all` + browser tools (on the shared global `BrowserHandle`, hidden until connected) + `vision_analyze`.
- `get(name)`, `names()`.
- `get_emoji(name)` — the tool's emoji, defaulting to `"⚡"` when unset or missing.
- `get_max_result_size(name)` — resolved via `storage::resolve_threshold` (pinned thresholds, registry chain, default). `None` means unlimited; `read_file` is pinned to `None` so persistence can never create a persist→read→persist loop.
- `definitions(enabled, ctx)` — the OpenAI `{"type": "function", ...}` list for enabled tool names, **check-gated** (a tool whose `check` fails is omitted) and schema-**sanitized**.
- `dispatch(name, args, ctx)` / `dispatch_call(..., tool_use_id)` — dispatch semantics:
  - Unknown tool → `ToolResult::Error("Unknown tool: X")` (capital U, upstream envelope).
  - Any tool other than `read_file`/`search_files` resets the consecutive read/search loop counters (`note_other_tool`).
  - A panicking `execute` is caught and returned as a **sanitized** `[TOOL_ERROR] Tool execution failed: Panic: ...` envelope.
  - **Layer 2**: results over the tool's threshold are persisted to `<tmp>/joey-results/{id}.txt`, replaced in-context by a `<persisted-output>` preview + path envelope.
  - **Layer 3**: once the per-turn aggregate (default 200_000 chars, `TurnBudget` on the context) would be exceeded, further persistable results spill to disk immediately.
- `check()` results are TTL-cached for **30s**, with a **60s last-good grace window**: a failed probe within 60s of a success is treated as a flake (tool stays available, failure not cached). `invalidate_check_cache()` drops all cached results after config changes.

## Toolsets

Port of `toolsets.py`, memberships verbatim (including names of unimplemented tools — resolution may yield unregistered names the registry filters):

| Toolset | Description | Tools | Includes |
|---|---|---|---|
| `web` | Web research and content extraction tools | `web_search`, `web_extract` | — |
| `search` | Web search only (no content extraction/scraping) | `web_search` | — |
| `terminal` | Terminal/command execution and process management tools | `terminal`, `process` | — |
| `skills` | Access, create, edit, and manage skill documents with specialized instructions and knowledge | `skills_list`, `skill_view`, `skill_manage` | — |
| `cronjob` | Cronjob management tool | `cronjob` | — |
| `file` | File manipulation tools: read, write, patch (with fuzzy matching), and search (content + files) | `read_file`, `write_file`, `patch`, `search_files` | — |
| `file-read` | Read-only file tools: read and search, no writes (exploration/review) | `read_file`, `search_files` | — |
| `todo` | Task planning and tracking for multi-step work | `todo` | — |
| `memory` | Persistent memory across sessions (personal notes + user profile) | `memory` | — |
| `session_search` | Search and recall past conversations with summarization | `session_search` | — |
| `clarify` | Ask the user clarifying questions (multiple-choice or open-ended) | `clarify` | — |
| `delegation` | Spawn subagents with isolated context for complex subtasks | `delegate_task`, `subagent_control` | — |
| `team` | Agent-team collaboration (feature 022): shared task list, member mailboxes, team status | `team_status`, `team_message`, `team_tasks` | — |
| `debugging` | Debugging and troubleshooting toolkit | `terminal`, `process` | `web`, `file` |
| `safe` | Safe toolkit without terminal access | — | `web`, `vision`, `image_gen` |
| `vision` | Image analysis and vision tools | `vision_analyze` | — |
| `image_gen` | Creative generation tools (images) | `image_generate` | — |
| `coding` | Coding-focused toolset: files, terminal, search, web docs, skills, todo, delegate, vision, browser | 36 members: the file/web/terminal/browser/vision/skills/todo/memory/session_search/clarify/delegation tools above incl. all 16 browser verbs | — |
| `joey-cli` | Full interactive CLI toolset - all default tools plus cronjob management | `CORE_TOOLS` | — |
| `joey-cron` | Default cron toolset - same core tools as joey-cli; gated by `joey tools` | `CORE_TOOLS` | — |

`CORE_TOOLS` is the shared upstream core list (60 names) shared by the CLI and platform toolsets: the web/terminal/file quintet, `vision_analyze`/`image_generate`, skills, the 16 browser tools, `text_to_speech`, `todo`/`memory`/`session_search`/`clarify`, the 4 LSP tools, `execute_code`/`delegate_task`, `cronjob`, Home Assistant, kanban, and `computer_use`.

- `resolve("all")` / `resolve("*")` returns the sorted union of every toolset.
- Gateway platforms call `register_platform(name)`; `joey-<platform>` then resolves to `CORE_TOOLS` automatically.
- Note: `multi_edit` is registered by `register_all` but appears in **no** toolset — it is reachable only by explicit enablement.

## Built-in tools reference

### Core 17 (registered by `register_all`)

#### `read_file` — toolset `file`, read-only / parallel-safe

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `path` | string | yes | — | Path to the file to read (absolute, relative, or ~/path) |
| `offset` | integer | no | 1 | Line number to start reading from (1-indexed, default: 1) |
| `limit` | integer | no | 500 | Maximum number of lines to read (default: 500, max: 2000) |

Reads a text file and returns `LINE_NUM|CONTENT` numbered output in a JSON envelope (`content`, `total_lines`, `file_size`, `truncated`, `is_binary`, `is_image`, optional `hint`/`next_offset`). Guards: device paths, binary extensions, credential/internal paths; redacts secrets; suggests similar filenames on miss. Re-reads of an unchanged region return a `{"status": "unchanged"}` stub, escalate to a hard `BLOCKED` error on the third hit, and a fourth consecutive identical read is blocked. Emoji `📖`; `max_result_chars` 100_000 but pinned unpersistable.

#### `write_file` — toolset `file`, sequential

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `path` | string | yes | — | Path to the file to write (will be created if it doesn't exist, overwritten if it does) |
| `content` | string | yes | — | Complete content to write to the file |
| `cross_profile` | boolean | no | false | Opt out of the cross-profile soft guard (edits to another Joey profile's skills/plugins/cron/memories) |

Overwrites whole files, creates parent directories, returns `bytes_written`, `dirs_created`, `resolved_path`, `files_modified`. Refuses sensitive system paths, Joey config, and internal `read_file` display text; fail-closed syntax gate on `.json`/`.yaml`/`.toml` (and linted languages — only new errors surface). Preserves CRLF line endings and BOM of the replaced file. Emoji `✍️`.

#### `patch` — toolset `file`, sequential

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `mode` | string | yes | `replace` | Edit mode. 'replace' (default): requires path + old_string + new_string. 'patch': requires patch content only. |
| `path` | string | cond. | — | REQUIRED when mode='replace'. File path to edit. |
| `old_string` | string | cond. | — | REQUIRED when mode='replace'. Exact text to find and replace. Must be unique unless replace_all=true. |
| `new_string` | string | cond. | — | REQUIRED when mode='replace'. Replacement text. Pass empty string '' to delete the matched text. |
| `replace_all` | boolean | no | false | Replace all occurrences instead of requiring a unique match (default: false) |
| `patch` | string | cond. | — | REQUIRED when mode='patch'. V4A format patch content (`*** Begin Patch` … `*** End Patch`) |
| `cross_profile` | boolean | no | false | Opt out of cross-profile soft guard |

Find-and-replace via the 9-strategy fuzzy matcher, or multi-file V4A application. Returns `success`, `diff` (unified), `resolved_path`; tracks consecutive failures per path and emits a `_hint` on the third failure. V4A headers with `..` traversal are rejected. Emoji `🔧`.

#### `multi_edit` — toolset `file` (registered, in **no** toolset), sequential

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `file_path` | string | yes | — | Path to the file to edit. |
| `edits` | array | yes | — | Array of edit operations to apply sequentially (minItems 1). Each item: `old_string` (string, required), `new_string` (string, required), `replace_all` (boolean, default false). |
| `cross_profile` | boolean | no | false | Opt out of cross-profile soft guard |

Applies multiple find-and-replace edits to one file atomically — all edits are validated before any is applied; only the first edit may have an empty `old_string` (file creation). Emoji `📝`.

#### `search_files` — toolset `file`, read-only / parallel-safe

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `pattern` | string | yes | — | Regex pattern for content search, or glob pattern (e.g., '*.py') for file search |
| `target` | string | no | `content` | 'content' searches inside file contents, 'files' searches for files by name |
| `path` | string | no | `.` | Directory or file to search in (default: current working directory) |
| `file_glob` | string | no | — | Filter files by pattern in grep mode (e.g., '*.py' to only search Python files) |
| `limit` | integer | no | 50 | Maximum number of results to return (default: 50) |
| `offset` | integer | no | 0 | Skip first N results for pagination (default: 0) |
| `output_mode` | string | no | `content` | 'content' / 'files_only' / 'count' output format for grep mode |
| `context` | integer | no | 0 | Number of context lines before and after each match (grep mode only) |

Ripgrep-backed content/file search with a gitignore-aware fallback walk when `rg` is absent. With ≥5 matches the content mode densifies to a path-grouped `matches_text` format. Identical repeated searches escalate warnings/blocks like `read_file` (`already_searched`). Emoji `🔎`.

#### `terminal` — toolset `terminal`, sequential (governed)

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `command` | string | yes | — | The command to execute on the VM |
| `background` | boolean | no | false | Run in the background; returns a session_id. Pair with notify_on_complete for bounded jobs. |
| `timeout` | integer | no | 180 | Max seconds to wait (default: 180, foreground max: 600). Returns INSTANTLY when command finishes. Foreground timeout above the max is rejected; use background=true for longer commands. |
| `workdir` | string | no | session cwd | Working directory for this command (absolute path) |
| `pty` | boolean | no | false | Pseudo-terminal mode for interactive CLI tools. Local and SSH backends only. |
| `notify_on_complete` | boolean | no | false | background only: exactly one notification on process exit. Mutually exclusive with watch_patterns. |
| `watch_patterns` | array of string | no | — | background only: rare one-shot output signals; hard rate limit 1/15s, auto-disabled after 3 dropped-match windows. Mutually exclusive with notify_on_complete. |

Runs shell commands with a sanitized environment (see Security), session-persistent cwd (tracked via the `__JOEY_CWD_MARKER__` echoed by the wrapper script), stderr→stdout merge, head/tail truncation (40% head / 60% tail) at the `tool_output.max_bytes` budget with the upstream truncation marker, ANSI stripping, and secret redaction. Non-zero exit codes get human-readable notes (grep 1 = "No matches found (not an error)", curl 6/7/22/28, git 1, diff 1, …). Foreground default 180s (`terminal.timeout` / `TERMINAL_TIMEOUT`), hard cap 600s (`TERMINAL_MAX_FOREGROUND_TIMEOUT`). Concurrency is governed (see below). Emoji `💻`.

#### `process` — toolset `terminal`, sequential

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `action` | string | yes | — | One of `list`, `poll`, `log`, `wait`, `kill`, `write`, `submit`, `close` |
| `session_id` | string | cond. | — | Process session ID (required for all actions except 'list') |
| `data` | string | cond. | — | Data to send to stdin (for 'write' and 'submit' actions) |
| `timeout` | integer | no | — | Max seconds to block for 'wait' action |
| `limit` | integer | no | 200 | Max lines to return for 'log' action |
| `offset` | integer | no | — | Line offset for 'log' action (for pagination) |

Manages background processes started with `terminal(background=true)`. Each session retains bounded ring buffers (256KB stdout + 256KB stderr); at most 32 completed sessions are kept (oldest reaped first, running sessions never reaped); completion notices carry a 1024-char output tail.

#### `todo` — toolset `todo`, sequential

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `todos` | array | no | — | Task items to write. Omit to read current list. Each item: `id` (string), `content` (string), `status` (`pending`/`in_progress`/`completed`/`cancelled`) — all required per item. |
| `merge` | boolean | no | false | true: update existing items by id, add new ones. false (default): replace the entire list. |

Session-scoped task list. Limits: 4000 chars per item content, 256 items max. Only one `in_progress` at a time by convention. Emoji `📋`.

#### `memory` — toolset `memory`, sequential

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `target` | string | yes | — | Which memory store: 'memory' for personal notes, 'user' for user profile (`memory`/`user`) |
| `action` | string | no | — | `add`, `replace`, or `remove` (single-op shape). Omit when using 'operations'. |
| `content` | string | cond. | — | The entry content. Required for 'add' and 'replace' (single-op shape). |
| `old_text` | string | cond. | — | REQUIRED for 'replace' and 'remove' (single-op shape): a short unique substring identifying the existing entry. |
| `operations` | array | no | — | Batch shape: a list of `{action, content?, old_text?}` applied atomically against the final char budget. |

Persists curated entries to `<joey>/memories/MEMORY.md` / `USER.md`, joined by the `\n§\n` delimiter. Char budgets: 2200 (memory) / 1375 (user), configurable via `memory.memory_char_limit` / `memory.user_char_limit`; the batch limit is checked only on the final result. Mutations re-read under a file lock and refuse on external drift (backing up to `.bak.<ts>`); at most 3 consolidation failures per turn. Emoji `🧠`.

#### `web_search` — toolset `web`, read-only / parallel-safe

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `query` | string | yes | — | The search query. Backend-supported operators (site:, filetype:, intitle:, -term, "exact phrase") pass through. |
| `limit` | integer | no | 5 | Maximum number of results to return. min 1, max 100. |

Tavily `/search` with the envelope `{"success": true, "data": {"web": [{title, url, description, position}]}}`. `check()` requires `TAVILY_API_KEY`. Emoji `🔍`.

#### `web_extract` — toolset `web`, read-only / parallel-safe

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `urls` | array of string | yes | — | List of URLs to extract content from (max 5 URLs per call) |
| `char_limit` | integer | no | 15000 | Optional per-page character budget (minimum 2000). Larger pages return head+tail with the full text stored to disk. |

Extracts clean markdown/text (also PDFs). Pages over the budget return a head+tail window plus a footer with the saved file path and the exact `read_file` call to page through the middle (full text stored up to 2,000,000 chars). Inline base64 images become `[IMAGE: alt]` placeholders. Blocked: secret-looking URLs and credential query params; per-URL SSRF filtering. Emoji `📄`.

#### `skills_list` — toolset `skills`, read-only / parallel-safe

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `category` | string | no | — | Optional category filter to narrow results |

Lists skills (name + description) discovered under the joey home and `skills.external_dirs` (walk depth 6, disabled skills filtered, name ≤100 / description ≤500 chars enforced). Emoji `📚`.

#### `skill_view` — toolset `skills`, read-only / parallel-safe

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `name` | string | yes | — | The skill name (use skills_list to see available skills); qualified `plugin:skill` form for plugin skills. |
| `file_path` | string | no | — | OPTIONAL: path to a linked file within the skill (references/, templates/, scripts/). Omit for SKILL.md itself. |

Returns the SKILL.md content plus a `linked_files` dict on first call; subsequent calls with `file_path` fetch linked files (traversal-guarded). Emoji `📚`.

#### `lsp_diagnostics` — toolset `lsp`, read-only (check-gated on LSP manager)

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `path` | string | yes | — | File path to check diagnostics for. |

Returns LSP errors/warnings for a file; `check()` is false when no LSP manager is registered for the file type. Emoji `🔬`.

#### `lsp_definition` — toolset `lsp`, read-only (check-gated)

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `path` | string | yes | — | File path |
| `line` | integer | yes | — | Line number (0-indexed) |
| `character` | integer | yes | — | Character offset (0-indexed) |

Go-to-definition for the symbol at a position; returns file locations. Emoji `🎯`.

#### `lsp_references` — toolset `lsp`, read-only (check-gated)

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `path` | string | yes | — | File path |
| `line` | integer | yes | — | Line number (0-indexed) |
| `character` | integer | yes | — | Character offset (0-indexed) |

All references to the symbol at a position. Emoji `🔗`.

#### `lsp_symbols` — toolset `lsp`, read-only (check-gated)

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `path` | string | yes | — | File path |

Lists document symbols (functions, classes, types) in a file. Emoji `📋`.

### Conditional tools

#### Browser tools (16) — toolset `web`, sequential; `check() = handle.is_connected()`

All 16 share one global `BrowserHandle`; they are registered unconditionally but **hidden until a browser session is connected** (`/browser connect` or first browser-tool use). `browser_navigate` refuses local/private network targets via the URL-safety bridge (same `is_safe_url` the web tools use). `browser_cdp` additionally requires `browser.allow_raw_cdp=true` (default false) and bypasses URL-safety gates.

| Tool | Params (required) | Behavior |
|---|---|---|
| `browser_navigate` 🌐 | `url` (string) | Navigate the agent's dedicated tab; waits for content settle; returns url/title/frame count |
| `browser_snapshot` 📸 | `viewport_only` (bool, default false), `since_last` (bool, default false) | Deep structural snapshot piercing shadow DOM and frames; delta mode for feeds |
| `browser_click` 👆 | `target` (object, required) | Click via cascading fallback refid→locator→text→geometry; reports which resolved |
| `browser_type` ⌨️ | `target` (required), `text` (string, required), `clear` (bool, default false), `submit` (bool, default false) | Type text into an input; optional clear and Enter-submit |
| `browser_scroll` 🖲️ | `direction` (`up`/`down`, required), `amount` (number, default 600), `target` (optional) | Scroll the page or a specific scrollable container |
| `browser_back` ◀️ | — (none) | Go back one history entry |
| `browser_press` 🎹 | `key` (string, required), `modifiers` (array of `ctrl`/`alt`/`shift`/`meta`/`cmd`) | Press a key with optional modifiers |
| `browser_get_images` 🖼️ | — (none) | List images on the page (src, alt, dimensions, visibility) |
| `browser_vision` 👁️ | `prompt` (string, optional) | Annotated Set-of-Mark screenshot with numbered markers |
| `browser_console` 🖥️ | — (none) | Read buffered console entries (level, text, source) |
| `browser_cdp` 🔧 | `method` (string, required), `params` (object) | Raw CDP passthrough (expert); needs `browser.allow_raw_cdp=true` |
| `browser_dialog` 💬 | `action` (`accept`/`dismiss`, required), `prompt_text` (string) | Accept/dismiss a JS dialog |
| `browser_hover` 🖱️ | `target` (required) | Hover an element (hover-only menus) |
| `browser_select_option` 📋 | `target` (required), `value` (string, required) | Select an option on a native `<select>` |
| `browser_drag` 🫳 | `source` (target, required), `target` (target, required) | Drag from source element to target element |
| `browser_click_coords` 🎯 | `x` (number, required), `y` (number, required), `marker` (string) | Click at viewport pixel coordinates; SoM marker picks resolve to rect center |

The shared `target` descriptor object: at least one of `refid` (element refid from the latest snapshot, e.g. `e12`), `locator` (structural CSS locator), `text` (visible text), `geometry` (`{x, y, w, h}` all required).

#### `vision_analyze` — toolset `web`, read-only

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `image_path` | string | yes | — | Path to the image file to analyze. |
| `question` | string | yes | — | What to look for / answer about the image. |

Reads an image (png/jpg/gif/webp; also data URLs), enforces a **15 MB** limit, and returns a `Multimodal` result with a base64 `image_url` part so the model's vision capability sees it natively. Emoji `👁️`.

#### `session_search` — toolset `session_search`, read-only / parallel-safe; registered by joey-cli when a session DB exists

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `query` | string | yes | — | Search query. Supports FTS5 syntax: quoted phrases, boolean (AND/OR/NOT), prefix wildcards (deploy*). |
| `limit` | integer | no | 5 | Max results (clamped 1–20) |
| `session_id` | string | no | — | With around_message_id, retrieve a window of messages around that message. |
| `around_message_id` | integer | no | — | Message ID to center the window on (scroll mode). |
| `window` | integer | no | 5 | Messages on each side of the anchor (clamped 1–20) |

FTS5-ranked search over past sessions, or scroll mode (context window around a message). Requires the FTS index (`check()` = session DB present).

#### `clarify` — toolset `clarify`; check() = interactive session **and** channel present

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `question` | string | yes | — | The question itself, and ONLY the question. Options go in 'choices'. |
| `choices` | array of string | no | — | Up to 4 distinct, mutually exclusive options (maxItems 4). Omit for open-ended. |

Sends a `ClarifyRequest` to the UI and awaits the user's response via a oneshot channel; errors immediately in non-interactive sessions.

#### NeuroCode tools — toolset `coding`; check() = backend wired

| Tool | Params | Notes |
|---|---|---|
| `neurocode_index` 🧠 | `path` (string, required); `force` (bool, default false) | Build/refresh the structural dependency graph (tree-sitter); returns ingestion summary |
| `neurocode_query` 🔍 | `query_type` (`dependencies`/`dependents`/`definition`/`references`, required); `symbol` (string, required); `limit` (integer, default 20, min 1) | Structural queries over the indexed graph |
| `neurocode_status` 📊 | none (`additionalProperties: false`) | Engine status: artifact/edge counts, schema version, last-index time |
| `neurocode_ingest` 📚 | `category` (`pattern`/`antipattern`/`rule`/`convention`, required); `source_path` (string, required); `provenance` (string, required); `version_tag` (string) | Ingest domain knowledge into the knowledge memory |

#### `neurocode_search` — toolset `coding`; registered **only** when `neurocode.rag.enabled` (absent from the registry otherwise, spec 021 FR-009 parity)

| Param | Type | Required | Default | Description |
|---|---|---|---|---|
| `query` | string | yes | — | Natural language and/or exact symbol names |
| `file_filter` | string | no | — | Glob restricting results to matching file paths |
| `limit` | integer | no | `neurocode.rag.top_k` | Max results (maximum 50) |
| `expand_lines` | integer | no | `neurocode.rag.context_window_lines` | ± context lines (clamped to maximum 200) |
| `relation_depth` | integer | no | — | Relationship expansion depth 0–2 |

Semantic search; whitespace-only queries are validation errors without a backend call. Backend errors degrade to keyword-only results rather than failing the turn.

### Parallel dispatch (PARALLEL_SAFE_TOOLS)

`joey-agent-core` (`agent.rs`) batches read-only tools for concurrent dispatch — the port-restricted `_PARALLEL_SAFE_TOOLS`:

```
read_file, search_files, session_search, skill_view, skills_list, web_extract, web_search
```

Maximal contiguous runs of these in a model batch run concurrently (each capped by a 300s parallel-tool timeout); runs of length <2 are demoted to sequential and merged with adjacent sequential segments. Everything else is a sequential barrier, spaced by the configured `tool_delay`.

## JSON-schema sanitizer

`src/sanitize.rs` (port of `tools/schema_sanitizer.py`) repairs tool parameter schemas before they reach strict backends. Applied recursively, then guaranteed at the top level:

- **Bare-string schemas** — a schema position holding `"object"` (or any known type name) becomes `{"type": ...}`; unknown strings become `{"type": "object", "properties": {}}`.
- **Missing `properties`** — object nodes get `properties: {}` injected; the top level is always `type: object` with `properties`.
- **`type` arrays** — with exactly one non-null type → plain `type` (+`nullable: true` if null was present); with ≥2 → `anyOf` of single-type schemas (+`nullable`); all-null/garbage → `"null"`/`"object"`.
- **Nullable `anyOf`/`oneOf` collapse** — a union with a `null` branch and exactly one non-null branch collapses to that branch, carrying over `title`/`description`/`default`/`examples` and setting `nullable: true`.
- **Top-level combinator strip** — `allOf`, `anyOf`, `oneOf`, `enum`, `not` removed from the outermost object only.
- **`$ref` siblings** — `default` beside a `$ref` is dropped (strict-validators reject it).
- **Stale `required` pruning** — `required` entries with no matching property are dropped; an empty list removes the key entirely.

Reactive strippers (public, used by backend error-recovery paths): `strip_pattern_and_format(tools)` removes `pattern`/`format` keywords, and `strip_slash_enum(tools)` removes slash-containing `enum` values, each returning the number of tools changed.

## Fuzzy patch matcher

`src/fuzzy.rs` (port of `tools/fuzzy_match.py`) backs `patch` (replace mode), `multi_edit`, and V4A update hunks. Nine strategies, tried in order; the first with any match wins:

1. `exact` — literal substring match.
2. `line_trimmed` — per-line `trim()` on both sides.
3. `whitespace_normalized` — runs of whitespace collapsed.
4. `indentation_flexible` — leading whitespace ignored per line.
5. `escape_normalized` — `\\n`/`\\t`/escaped quotes treated as their literal chars.
6. `trimmed_boundary` — pattern with leading/trailing blank lines trimmed.
7. `unicode_normalized` — smart quotes → `"`, em/en dash → `--`/`-`, ellipsis → `...`, nbsp → space.
8. `block_anchor` — matches multi-line blocks by first/last trimmed anchor lines (unicode-normalized).
9. `context_aware` — block-level match when ≥50% of lines have ≥0.80 `SequenceMatcher` ratio.

Every strategy returns **all** matches; more than one match without `replace_all` is an error ("Found N matches… Provide more context or use replace_all=True"). Post-match guards: **escape-drift detection** (old/new contain `\'` or `\"` but the matched file region does not → error explaining the serialization artifact), **conditional `\t`/`\r` unescape** of the new string (only when the matched region actually contains the raw char), **unicode preservation** (replacement keeps the file's original smart quotes when strategy 7 matched), and **re-indentation** (replacement inherits the matched region's indentation for non-exact strategies). All offsets are byte offsets computed to mirror CPython's char-offset arithmetic.

## Security layers

**Read guards** (`guards.rs`, applied in `read_file`): a device-path blocklist (`/dev/zero`, `/dev/random`, `/dev/urandom`, `/dev/full`, `/dev/stdin`, `/dev/tty`, `/dev/console`, `/dev/stdout`, `/dev/stderr`, `/dev/fd/0|1|2`, plus `/proc/*/fd/*` and leak-y `/proc` suffixes like `/environ`, `/cmdline`, `/maps`…), checked through symlink hops; a binary-extension guard (~90 extensions; `.pdf` deliberately excluded); a credential/internal-path block (`.env*` project files, joey-home `auth.json`/`auth.lock`/`.anthropic_oauth.json`/`webhook_subscriptions.json`/`auth/google_oauth.json`/`cache/bws_cache.json`, the `mcp-tokens/` directory, and `skills/.hub` caches as prompt-injection carriers); secret redaction of returned content; and similar-filename suggestions on misses.

**Write guards**: sensitive-prefix refusal for `/etc/`, `/boot/`, `/usr/lib/systemd/`, `/private/etc/`, `/private/var/` and exact docker sockets (`/var/run/docker.sock`, `/run/docker.sock`); Joey config-file refusal; refusal to write internal `read_file` display text (line-number prefixed content); a fail-closed pre-write syntax gate for `.json`/`.yaml`/`.toml`; and CRLF/BOM preservation when overwriting an existing file.

**Terminal**: a tier-1 env strip of ~20 secret keys (`GH_TOKEN`, `GITHUB_TOKEN`, `TELEGRAM_BOT_TOKEN`, `SLACK_*`, `GATEWAY_RELAY_*`, `HASS_TOKEN`, `EMAIL_PASSWORD`, `MODAL_*`, `DAYTONA_API_KEY`, …) from every spawned subprocess, plus removal of all `JOEY_PROVIDER_FORCE_*` and `AUXILIARY_*_API_KEY`/`AUXILIARY_*_BASE_URL` vars; ANSI stripping parallelized above 256KB; output secret redaction; the exit-code meaning table; a process-global **terminal governor** capping concurrent commands (limit resolution: `TERMINAL_MAX_CONCURRENT` env > `terminal.max_concurrent` config > auto = available cores clamped to 4–16, fallback 8) with per-agent FIFO queues and round-robin admission, queue-state events throttled to 50ms; and the `__JOEY_CWD_MARKER__` cwd-contract marker.

**Web**: secret-in-URL blocking (prefix heuristics + a 19-name credential query-param list: `access_token`, `api_key`, `apikey`, `auth_token`, `authorization`, `awsaccesskeyid`, `client_secret`, `credential`, `credentials`, `jwt`, `password`, `passwd`, `secret`, `session_id`, `signature`, `token`, `x_amz_security_token`, `x_amz_signature`, and the dash-separated AMZ variants); SSRF protection via `is_safe_url` — only http/https, DNS resolved with **every** answer checked and resolution failure failing closed, cloud-metadata targets (`metadata.google.internal`, `metadata.goog`, `169.254.0.0/16`, `100.100.100.200`, IPv6 `fd00:ec2::254`) **always blocked**, and private/loopback/link-local/reserved/multicast/CGNAT ranges blocked unless `JOEY_ALLOW_PRIVATE_URLS` / `security.allow_private_urls` (legacy `browser.allow_private_urls`); base64 inline images reduced to `[IMAGE: alt]` placeholders.

## Defaults & limits

| Limit | Value | Source |
|---|---|---|
| Default max tool result | 100_000 chars | `storage::DEFAULT_RESULT_SIZE_CHARS` |
| Per-turn aggregate budget | 200_000 chars | `storage::DEFAULT_TURN_BUDGET_CHARS` |
| Persisted-output preview | 1_500 chars | `storage::DEFAULT_PREVIEW_SIZE_CHARS` |
| `tool_output.max_bytes` / `max_lines` / `max_line_length` | 50_000 / 2_000 / 2_000 | `truncate.rs` |
| read_file defaults | offset 1, limit 500 (max 2000; limit clamped to `tool_output.max_lines`) | `truncate.rs` |
| search_files default limit | 50 (offset 0) | `truncate.rs` |
| Terminal timeout | default 180s (`terminal.timeout` / `TERMINAL_TIMEOUT`), foreground max 600s (`TERMINAL_MAX_FOREGROUND_TIMEOUT`) | `terminal_tool.rs` |
| Terminal concurrency | `TERMINAL_MAX_CONCURRENT` > `terminal.max_concurrent` > auto (cores clamped 4–16, fallback 8) | `terminal_tool.rs` / `terminal_governor.rs` |
| Governor queue-state throttle | 50 ms | `QUEUE_STATE_THROTTLE_MS` |
| Process ring buffers | 256KB stdout + 256KB stderr per session; 32 completed sessions retained; 1024-char notice tails | `process_tool.rs` |
| Memory char limits | 2200 (memory) / 1375 (user); `\n§\n` delimiter; 3 consolidation failures/turn | `memory_tool.rs` |
| Todo | 4000 chars/item; 256 items | `todo_tool.rs` |
| Skills | walk depth 6; name ≤100 chars; description ≤500 chars | `skills_tool.rs` |
| web_extract | default char_limit 15000; stored full text ≤ 2_000_000 chars | `web_tools.rs` |
| vision_analyze | 15 MB image limit | `vision_tools.rs` |
| Read dedup/loop | stub at ≥2 unchanged hits, hard block at ≥3; loop warning at 3 consecutive, block at 4 | `file_tools.rs` |
| Patch failure hint | `_hint` emitted at ≥3 consecutive failures per path | `context.rs` tracker |
| Session trackers | read_history 500; dedup 1000; read_timestamps 1000; pending completions 64 | `context.rs` |
| check() cache | 30s TTL; 60s last-good grace | `registry.rs` |

## Testing

- `tests/schema_snapshots.rs` — pins every built-in tool's model-visible surface (name, description, parameters) against upstream-derived literals.
- `tests/terminal_streaming.rs` — terminal async-streaming regressions; the result schema stays `{output, exit_code, error}`.
- `tests/terminal_governor.rs` — governor admission contracts: cap enforcement, FIFO fairness, cooperative interrupt.
- `tests/rayon_terminal_e2e.rs` — exercises the rayon post-processing path (ANSI strip + redaction) for real.
- `tests/process_reaper.rs` — the background reaper fills session ring buffers; dead sessions are reaped above the cap.

Plus extensive inline `#[cfg(test)]` unit tests in every module (fuzzy strategies over adversarial unicode tables, sanitizer repair cases, guard blocklists, tool envelopes). Run scoped: `cargo test -p joey-tools`.

## See also

- [../tools.md](../tools.md) — user-facing tool documentation
- [joey-agent-core.md](joey-agent-core.md) — the turn loop, parallel dispatch (`PARALLEL_SAFE_TOOLS`), and output-budget enforcement
- [joey-core.md](joey-core.md) — config resolution for `tool_output.*` / `terminal.*` / `security.*` keys, secret redaction
- [joey-browser.md](joey-browser.md) — the browser session/CDP layer behind the 16 browser tools
- [joey-neurocode.md](joey-neurocode.md), [joey-neurocode-rag.md](joey-neurocode-rag.md) — the NeuroCode engine and RAG backend behind the five neurocode tools
- [joey-cli.md](joey-cli.md) — conditional tool wiring (session DB, clarify channel, platforms)
- [skills.md](skills.md) — the SKILL.md format the skills tools walk
