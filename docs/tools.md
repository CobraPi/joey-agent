# joey-tools — Tools, Toolsets, and the Tool Trait

`crates/joey-tools` is the tool system for joey-agent (Rust port of upstream
Hermes `tools/` + `toolsets.py`). It provides the `Tool` trait, the
`ToolRegistry`, the toolset resolver, JSON-schema sanitization, output
truncation + result persistence, the fuzzy matcher behind `patch`, the V4A
patch parser, SSRF/file-safety guards, checkpoint VCS, the LSP client, and the
self-contained built-in tools.

---

## 1. The `Tool` trait (src/registry.rs)

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;                    // wire name, e.g. "read_file"
    fn toolset(&self) -> &str;                 // owning toolset, e.g. "file"
    fn description(&self) -> &str;             // schema description
    fn parameters(&self) -> Value;             // JSON Schema of params object
    fn emoji(&self) -> &str { "" }             // progress display (default "⚡")
    fn max_result_chars(&self) -> Option<usize>; // per-tool persistence threshold
    fn check(&self, _ctx: &ToolContext) -> bool { true } // availability gate
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult;
}
```

`ToolResult` is an enum: `Text(String)`, `Multimodal(Vec<Value>)` (text +
image parts; rendered with `[image]` placeholders), or `Error(String)`
(serialized as `{"error": "..."}` in Python JSON style via `pyjson::dumps`).

### ToolRegistry behavior
- **Explicit registration** — no reflection/auto-discovery. `register_all`
  registers self-contained builtins; higher crates register tools that need
  broader context (`session_search`, `clarify`, `delegate_task`, `cronjob`,
  NeuroCode).
- **check() TTL cache** — check results cached ~30s, with a 60s last-good
  grace window so flaky availability probes can't strip tools from the model.
  Panics in check count as failure. `invalidate_check_cache()` clears it.
- **Dispatch envelope** — unknown tool returns `{"error": "Unknown tool: X"}`.
  Tool panics become `[TOOL_ERROR] Tool execution failed: Panic: ...` after
  `sanitize_tool_error` strips framing tokens (`</tool_call>`, code fences,
  CDATA, role tags) and caps length at 2000 chars.
- **Output persistence** — after execution, oversized results (over the
  per-tool threshold, default from `storage::DEFAULT_RESULT_CHARS`, resolved
  via pinned config → registered value → default) are persisted to
  `~/.joey/storage/` and replaced with a head preview + file path. A per-turn
  aggregate budget (`TurnBudget`) spills persistable output once the turn
  total exceeds the budget.
- **Read/search loop counter** — any tool other than `read_file`/
  `search_files` resets the consecutive read/search loop counters
  (`note_other_tool`).

### ToolContext (src/context.rs)
Carries cwd (session-persistent; `resolve_path` resolves relative/`~/` paths),
Config, session_id, interactive flag, yolo flag, progress sender, interrupt
flag, background-completion queue, `SessionState` (dedup caches, patch-failure
tracker, consecutive-search counters), and the per-turn `TurnBudget`.

---

## 2. Complete built-in tool list

Registered unconditionally by `builtins::register_all`:

| Tool | Toolset | Purpose | Key parameters | Emoji |
|---|---|---|---|---|
| `read_file` | file | Read a text file with `LINE_NUM\|CONTENT` output, pagination, truncation at ~100K chars with `next_offset` continuation; auto-extracts .ipynb/.docx/.xlsx | `path*`, `offset` (1-idx, default 1), `limit` (default 500, max 2000) | 📖 |
| `write_file` | file | Overwrite/create a file (creates parent dirs); fail-closed JSON/YAML/TOML syntax gate; refuses to write read_file display text back | `path*`, `content*`, `cross_profile` | ✍️ |
| `patch` | file | Targeted find-and-replace with 9-strategy fuzzy matching, or V4A multi-file patch mode; returns unified diff; syntax checks post-edit | `mode*` (replace/patch), `path`, `old_string`, `new_string`, `replace_all`, `patch` (V4A), `cross_profile` | 🔧 |
| `multi_edit` | file | Multiple find-and-replace edits to one file, atomically (all validated before any applied); only first edit may have empty `old_string` (file creation) | `file_path*`, `edits*` (array of {old_string, new_string, replace_all}) | 📝 |
| `search_files` | file | Ripgrep-backed content search (regex) or file search by glob (also replaces ls; sorted by mtime) | `pattern*`, `target` (content/files; aliases grep/find), `path`, `file_glob`, `limit` (50), `offset`, `output_mode` (content/files_only/count), `context` | 🔎 |
| `terminal` | terminal | Run shell commands; persistent cwd/env; merged stderr; ANSI stripping; secret redaction; session-persistent cwd; foreground default 180s (config `terminal.timeout` / env `TERMINAL_TIMEOUT`), hard foreground max 600s (env `TERMINAL_MAX_FOREGROUND_TIMEOUT`); background sessions via `background=true` (+ `notify_on_complete`, `watch_patterns` rate-limited to 1 per 15s, auto-disabled after 3 dropped-match windows); PTY mode via `pty=true`; per-command `workdir`. Bash on Unix (PowerShell fallback on Windows) | `command*`, `background`, `timeout`, `workdir`, `pty`, `notify_on_complete`, `watch_patterns` | 💻 |
| `process` | terminal | Manage background processes started via `terminal(background=true)` | `action*` (list/poll/log/wait/kill/write/submit/close), `session_id`, `data`, `timeout`, `limit`, `offset` | ⚡ |
| `todo` | todo | Session task list; read with no params; `merge=false` replaces list, `merge=true` updates by id; only one `in_progress` item | `todos` (array of {id, content, status}), `merge` | 📋 |
| `memory` | memory | Persistent cross-session memory (injected into every future turn); atomic batch operations against a char budget; targets: `user` (profile) / `memory` (notes) | `target*` (memory/user), `action` (add/replace/remove), `content`, `old_text`, `operations` (batch array) | 🧠 |
| `web_search` | web | Tavily-backed web search (up to 5 results by default with title/url/description; backend operators like `site:` supported) | `query*`, `limit` (default 5, max 100) | 🔍 |
| `web_extract` | web | Extract clean page content as markdown (no LLM summarization); handles PDF URLs; per-page char budget (default 15000) with head+tail window and persisted full text; `[IMAGE: alt]` placeholders; max 5 URLs | `urls*` (max 5), `char_limit` (15000) | 📄 |
| `skills_list` | skills | List available skills (name + description) | `category` | 📚 |
| `skill_view` | skills | Load a skill's SKILL.md or its linked files (references/templates/scripts) | `name*` (or `plugin:skill`), `file_path` | 📚 |
| `lsp_diagnostics` | lsp | LSP errors/warnings for a file | `path*` | 🔬 |
| `lsp_definition` | lsp | Go-to-definition at position | `path*`, `line*` (0-idx), `character*` (0-idx) | 🎯 |
| `lsp_references` | lsp | All references to symbol at position | `path*`, `line*`, `character*` | 🔗 |
| `lsp_symbols` | lsp | Document symbols (functions/classes/types) | `path*` | 📋 |

Registered conditionally by helper functions in `builtins.rs` (registered but
`check()`-gated, hidden from the model when unavailable):

| Tool | Toolset | Purpose | Availability gate | Registrar |
|---|---|---|---|---|
| `session_search` | session_search | FTS5 search over past session messages (snippets, timestamps); scroll mode via `session_id` + `around_message_id` | session DB handle present | `register_session_tools` |
| `clarify` | clarify | Ask the user a structured multiple-choice (max 4) or open-ended question | interactive session + clarify channel present | `register_clarify_tool` |
| `neurocode_index` | coding | Build/refresh tree-sitter structural dependency graph for a project | NeuroCode backend active | `register_neurocode_tools` |
| `neurocode_query` | coding | Query the graph: dependencies/dependents/definition/references for a symbol or FQCN | backend active | same |
| `neurocode_status` | coding | Engine status: indexed artifacts, edges, schema version, last-index time | backend active | same |
| `neurocode_ingest` | coding | Ingest domain knowledge (pattern/antipattern/rule/convention, optional version tag) into knowledge memory | backend active | same |

The four LSP tools are `check()`-gated on an LSP manager being registered and
configured servers existing (`lsp.rs` `LspManager::from_joey_config`; see
`docs/LSP.md`). Web tools are gated on a Tavily API key.

Registered by higher crates (not in joey-tools): `delegate_task`
(delegation), `cronjob` (joey-cron), plus platform tools upstream ships that
this port names in toolsets but does not implement (`image_generate`,
`execute_code`, `computer_use`, `ha_*`, `kanban_*`, `read_terminal`,
`close_terminal`, `text_to_speech` — the resolver filters unregistered names
at definition time, matching upstream).

Browser automation is implemented (feature 016, joey-browser crate): the
16 `browser_*` tools (12 declared + hover/select_option/drag/click_coords)
plus `vision_analyze` — hidden until a browser session connects (`/browser
connect` or first browser-tool use). See [browser.md](browser.md).

\* = required parameter.

---

## 3. Toolset hierarchy and recursive includes (src/toolsets.rs)

Ported verbatim from upstream `toolsets.py` (rebranded `hermes-*` → `joey-*`),
including names of unimplemented tools.

| Toolset | Tools | Includes |
|---|---|---|
| `web` | web_search, web_extract | — |
| `search` | web_search | — |
| `terminal` | terminal, process | — |
| `skills` | skills_list, skill_view, skill_manage | — |
| `cronjob` | cronjob | — |
| `file` | read_file, write_file, patch, search_files | — |
| `todo` | todo | — |
| `memory` | memory | — |
| `session_search` | session_search | — |
| `clarify` | clarify | — |
| `delegation` | delegate_task | — |
| `debugging` | terminal, process | web, file |
| `safe` | — (include-only) | web, vision, image_gen |
| `vision` | vision_analyze | — |
| `image_gen` | image_generate | — |
| `coding` | 36 tools (32 declared + 4 additive browser verbs): web_search, web_extract, terminal, process, read_terminal, close_terminal, read_file, write_file, patch, search_files, vision_analyze, skills_list, skill_view, skill_manage, browser_navigate/snapshot/click/type/scroll/back/press/get_images/vision/console/cdp/dialog, todo, memory, session_search, clarify, execute_code, delegate_task | — |
| `joey-cli` | CORE_TOOLS (full default set + cronjob) | — |
| `joey-cron` | CORE_TOOLS | — |

- **CORE_TOOLS** is the shared upstream `_HERMES_CORE_TOOLS` membership list
  (verbatim): web, terminal+process+GUI-terminal readers, file tools,
  vision/image-gen, skills, browser automation (16 tools, feature 016), TTS, todo, memory,
  session_search, clarify, LSP tools, execute_code, delegate_task, cronjob,
  Home Assistant (4), kanban (12), computer_use.
- **Platform toolsets**: `register_platform("telegram")` makes
  `joey-telegram` auto-resolve to CORE_TOOLS.
- **Resolution**: `resolve(name)` recursively expands includes with cycle
  protection (visited set), returning a flat sorted, deduplicated list.
  `all`/`*` = union of everything. `resolve_multiple` merges several.
  Unresolvable names yield an empty list (filtered by the registry later).

---

## 4. Read-only/parallel-safe vs sequential (agent-core dispatch)

Dispatch classification lives in `joey-agent-core/src/agent.rs`
(`PARALLEL_SAFE_TOOLS`, port of upstream `_PARALLEL_SAFE_TOOLS`, restricted to
tools this port ships):

**Parallel-safe (read-only, run concurrently within a batch):**
`read_file`, `search_files`, `session_search`, `skill_view`, `skills_list`,
`web_extract`, `web_search`

**Everything else** (write_file, patch, multi_edit, terminal, process, todo,
memory, clarify, LSP tools, neurocode_*) runs sequentially with `tool_delay`
spacing (config `agent.tool_delay`, default 1.0s, port of run_agent.py:435).

---

## 5. Key semantics

### read_file
- Output `LINE_NUM|CONTENT`; `wc -l` newline-count semantics for total_lines.
- Guards in order: device-file block (`is_blocked_device` — /dev nodes that
  would block or emit infinite output), binary-extension pre-check (directs to
  vision_analyze), internal-path/credential read block
  (`get_read_block_error` — e.g. `~/.joey` secrets), then read.
- **Dedup loop guard**: repeat reads of an unchanged (same mtime) path+offset
  return `{"status": "unchanged"}`; after 2 such hits further reads are
  BLOCKED with a "STOP calling read_file" error.
- BOM stripping; invalid-UTF8 → binary-file envelope; not-found → fuzzy
  filename suggestions; >100K chars truncates on a line boundary and returns
  `next_offset`.

### write_file
- Full replace; creates parents; records to FileTracker for session change
  detection and checkpoint VCS.
- Drops-arg diagnostics: missing `content` (path but no content) returns a
  specific "dropped-arg bug under context pressure" error.
- Sensitive-path guard (`check_sensitive_path`); refuses read_file display
  text as content (`is_internal_file_tool_content`); fail-closed pre-write
  syntax gate for JSON/YAML/TOML; staleness warning (`_warning`) if the file
  changed on disk since last read.

### patch (replace mode + V4A patch mode)
- **replace**: `old_string` must be unique unless `replace_all`; empty
  `new_string` deletes.
- **patch**: V4A format (`*** Begin Patch` / `*** Update File:` / `@@` hunks
  / `*** End Patch`), multi-file; header paths get extra `..` traversal
  rejection.
- Both modes: sensitive-path checks on all touched paths, staleness warnings,
  unified-diff result, post-edit syntax checks (only NEW errors surfaced).

### search_files
- `target=content` (alias grep): regex with output_mode content/files_only/
  count, context lines, file_glob filter. `target=files` (alias find): glob
  match, mtime-sorted — this replaces `ls`.
- **Repeat-search loop guard**: 4th identical consecutive search is BLOCKED.
- Read-blocked paths filtered from results (`_omitted` count); secrets
  redacted from matched content (`redact_secrets`); newline-regex warning.

### process (src/tools/process_tool.rs)
- Actions: `list` (reaps dead sessions first, shows session_id/command/
  runtime), `poll` (drain new stdout/stderr, surface recorded exit code),
  `log` (paginated with offset/limit, default 200 lines), `wait` (block up to
  timeout), `kill`, `write` (stdin, no Enter), `submit` (write + Enter),
  `close` (close stdin).

---

## 6. LSP subsystem (src/lsp.rs)

`LspManager` spawns and manages language servers configured via joey config
(`lsp.rs` `LspServerConfig`; `from_joey_config`). Provides `diagnostics`,
`definition`, `references`, `document_symbols`, `rename` (not exposed as a
tool), and `shutdown`. The four tools (toolset `lsp`) are thin wrappers:
1-indexed positions returned to the model; empty results return
"No definitions/references/symbols found" messages. All are check-gated on
server availability and hidden otherwise. See `docs/LSP.md`.

---

## 7. Checkpoint VCS (src/vcs.rs)

`CheckpointManager`: shared-store git checkpointing of the working tree.
- Single shared store at `~/.joey/checkpoints/store`; per-project ref
  `refs/joey/<sha256-16>`, index, metadata; git object dedup across
  projects/sessions/worktrees.
- Fully lazy init: first `checkpoint()` (first mutating tool call or
  `/checkpoint`) creates store + first snapshot.
- Default excludes (build output, deps, VCS metadata, caches, virtualenvs,
  media, secrets, logs) via store `info/exclude`.
- Every git subprocess isolated from global/system git config, 5s timeout.
- Retention: ≤50 snapshots/project, 2GB total store cap, 90-day
  stale-project window — pruning always keeps the **newest** N (per
  project) / newest-tail (size cap), dropping the OLDEST via a
  chain-rebuild (`git commit-tree` reconstruction of the kept chain +
  `reflog expire` + `gc --prune=now`). The size-cap pruner drops the
  globally-oldest checkpoint one at a time, never a project's last
  snapshot, and runs gc inside the loop so progress is observable.
  (Historical bug: both pruners were inverted, deleting the newest
  snapshots — fixed with real-git regression tests.)
- API: `checkpoint(msg)`, `list()`, `revert(number)`, `cleanup()`,
  `is_enabled()` (requires `git` on PATH), `repo_path()`.

Related: `FileTracker` (src/file_tracker.rs) records reads/writes/deletes/
external mutations, generates per-file diffs (`generate_diff`), pending diffs,
and a `ChangeSummary` for session change detection and `/diff`-style display;
`drain_pending_diffs` feeds turn summaries. `generate_diff` is
rayon-parallelized internally (parallel line hashing, common prefix/suffix
trimming so the quadratic LCS core covers only the changed region, and a
wholesale-rewrite guard that bounds memory) while producing byte-identical
output to the sequential algorithm — pinned by a reference-oracle test.
`drain_pending_diffs` fans per-file read+decode+diff across cores
(order-preserving).

---

## 8. Smart-completion engine (src/completion.rs)

Shared by the CLI (reedline completer/hinter) and the TUI (popups), ported
from upstream `hermes_cli/commands.py::SlashCommandCompleter`:

- `pipe_subcommands` — extracts first-argument subcommands from a command's
  pipe-encoded args_hint (`[on|off|status]`).
- `extract_path_word` / `extract_context_word` — detect the word under the
  cursor as a path-like token (./ ../ ~/ / or containing `/`, URLs
  excluded) or an `@` token.
- `path_completions` — directory listings, prefix-filtered
  (case-insensitive), dirs first with trailing `/`, compact size labels
  (`9B`/`4K`/`1.2M`).
- `STATIC_CONTEXT_REFS` — `@diff`, `@staged`, `@file:`, `@folder:`,
  `@git:`, `@url:`; `@file:`/`@folder:` delegate to filtered listings
  (files-only / dirs-only).
- `CompletionEngine` — Arc-backed, cloneable project-file cache (rg first,
  fd fallback, 2s subprocess timeout with a dedicated stdout-drain thread —
  listings >64KB pipe buffer can't deadlock it), 5s TTL, 5000-entry cap.
  `project_files_blocking` (CLI Tab budget) vs `project_files_stale_ok`
  (TUI: returns stale data instantly, refreshes on a background thread).
- `fuzzy_file_completions` + `score_path` — upstream's exact scoring tiers
  (exact=100, prefix=80, substring=60, path=40, boundary-initials=35/25).

---

## 9. Sanitizer, guards, and threat-scan integration

- **Schema sanitizer** (src/sanitize.rs, port of schema_sanitizer.py): fixes
  constructs strict backends reject — bare-string schemas, missing
  `properties`, `type` arrays, nullable `anyOf`/`oneOf` unions (with title/
  description/default/examples carry-over), top-level combinators, `$ref`
  siblings, stale `required` entries. Reactive strippers
  `strip_pattern_and_format` and `strip_slash_enum` for backend-recovery
  paths. Applied automatically in `ToolRegistry::definitions`.
- **Input sanitizer** (src/sanitize_input.rs): `sanitize_tool_input` /
  `validate_required_params` — input-shape validation helpers producing
  upstream-shaped error results.
- **Error sanitizer** (registry.rs): `sanitize_tool_error` strips role tags,
  fences, CDATA; 2000-char cap.
- **URL safety / SSRF** (src/url_safety.rs): blocks cloud-metadata hosts/IPs
  always (169.254.0.0/16 incl. 169.254.169.254, 100.100.100.200 Alibaba,
  fd00:ec2::254 AWS v6, metadata.google.internal/goog); blocks private/
  loopback/link-local/reserved/multicast/unspecified/CGNAT (100.64/10)
  unless `security.allow_private_urls` / `JOEY_ALLOW_PRIVATE_URLS`; DNS
  resolved with EVERY answer checked, resolution failure fails closed;
  sensitive-query-param detector (e.g. tokens in URLs). Used by web tools.
- **File-safety guards** (src/guards.rs): device-file blocks, binary/image
  extension detection, internal/credential path read blocks
  (`get_read_block_error`), sensitive-write checks
  (`check_sensitive_path`), read_file-content-echo refusal, ANSI stripping,
  path-traversal detection.
- **Secret redaction**: terminal output and search matches run through
  `joey_core::redact::redact_secrets`.
- **Untrusted-content wrapping** (enforced in joey-agent-core dispatch, not
  in joey-tools): results of `web_search`, `web_extract`, `browser_*`,
  `mcp_*` tools carry attacker-controllable content and are wrapped/marked
  (`_UNTRUSTED_TOOL_NAMES` / `_UNTRUSTED_TOOL_PREFIXES`, min 32 chars).
- **Safe commands** (src/safe_commands.rs): `is_safe_read_only_command`,
  `is_dangerous_command`, `contains_command_chaining` — command
  classification used by higher crates (e.g. terminal gating/yolo paths).
- **Fuzzy matcher** (src/fuzzy.rs): the 9-strategy chain behind patch
  (exact → line_trimmed → whitespace_normalized → indentation_flexible →
  escape_normalized → trimmed_boundary → unicode_normalized → block_anchor →
  context_aware), each returning ALL matches (>1 without replace_all is an
  error), with post-match guards, replacement re-indentation, closest-lines
  hints (`format_no_match_hint`), and a Python-SequenceMatcher port
  (`difflib.rs`) for ratios.
