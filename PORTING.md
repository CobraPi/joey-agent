# Porting status: Hermes Agent → Joey Agent (Rust)

This document tracks what has been ported from the ~690K-line Python
`hermes-agent` into the Rust `joey-agent`, and what remains. The goal is a
faithful clone: same architecture, defaults, data formats, prompt text, and
wire behavior, rebranded `hermes → joey`.

**Fidelity review (2026-07-21):** every crate was audited line-by-line against
upstream (commit `7651764`, 2026-07-20) and rewritten where it deviated. The
ported surface below is now behavior-, format-, and text-faithful (modulo
branding); tests assert exact schemas, envelopes, grammars, and prompt text
(520 tests across the workspace). 2026-07-22: the `joey model` setup wizard,
Z.AI endpoint detection, the auth store, and the picker model catalog were
ported (this file's sections below reflect that). Anything not faithful is listed under
*Partial*, *Deferred*, or *Deliberate deviations* — if it isn't listed there,
it is intended to match upstream exactly.

## Complete and faithful (compiles, tested, runs end-to-end)

**Core foundation (`joey-core`)** — port of `hermes_constants.py`, `hermes_state.py`,
`hermes_time.py`, `hermes_logging.py`, `agent/redact.py`, `utils.py`, config layer:
- Home/profile resolution (`JOEY_HOME`, platform defaults, container/WSL/Termux
  detection, passwd-db real-home repair, subprocess HOME contract), first-run
  skeleton + `SOUL.md` seeding, named-profile guard.
- Layered config with upstream defaults (`max_turns` 90, model unset → setup,
  `display.show_reasoning` true, streaming false at the config layer):
  `DEFAULT_CONFIG` ← `config.yaml` ← `.env` (`.env` **overrides** shell vars) ←
  flags; `${VAR}` expansion; save writes only user-set keys (+`_config_version`);
  exact `.env` routing predicate and quoting writer; string-schema coercion
  guards; parse failures keep last-known-good with a corrupt backup; dotted
  paths address list indices; root-key normalization; `unset`.
- SQLite state store: upstream's exact schema (sessions 46 cols, messages 21,
  session_model_usage, gateway_routing, compression_locks, async_delegations,
  schema_version=22), standalone FTS5 + trigram twin with upstream triggers,
  FTS query sanitization + upstream snippet params, WAL-with-DELETE-fallback,
  busy retries, checkpoint cadence, escaped prefix resume. A hermes-created
  `state.db` opens and works unchanged; old joey DBs migrate in place.
- Timezone clock (cached, `reset_cache`), size-rotated redacting logs
  (`agent.log` 5MB×3 + `errors.log` WARNING+), full secret-redaction port
  (~40 vendor patterns + families, head6/tail4 masking, sentinel modes,
  `security.redact_secrets` kill-switch), reasoning-effort parsing with the
  full model-variant expansion, atomic writes (fsync, symlink-target, owner).
- Auth store (`~/.joey/auth.json`): upstream's store shape (`version` 1,
  `providers`, `active_provider`, `updated_at`), atomic 0600 writes with
  0700 parent, corrupt-file preservation (`auth.json.corrupt`) with
  empty-store recovery, bounded flock (15s), per-provider state get/merge,
  `deactivate_provider`. (The profile→global-root fallback rides on
  HERMES_HOME profile machinery; each joey profile keeps its own store.)

**Provider layer (`joey-providers`)** — port of `providers/`, `agent/transports/`,
`agent/anthropic_adapter.py`, `agent/error_classifier.py`:
- Profile registry matching upstream composition (openrouter/openai-api/
  anthropic/nous/deepseek/zai/gemini/xai/custom) with aliases, env-var chains,
  aux models, picker metadata (display name, `tui_desc`, signup URL, curated
  fallback models); per-provider base-URL overrides (`<ID>_BASE_URL`,
  full auth.py table incl. `GLM_BASE_URL`).
- Z.AI endpoint detection (auth.py `ZAI_ENDPOINTS` / `detect_zai_endpoint` /
  `_resolve_zai_base_url`): the four official endpoints (Global, China,
  Coding Plan Global/China) probed in order with per-endpoint candidate
  models (1-token `chat/completions` ping), result cached in auth.json keyed
  on `sha256(api_key)[:16]` (`set_active=false`), `GLM_BASE_URL` always wins,
  empty-key probe suppression; wired into client construction so a bare
  `GLM_API_KEY` lands on the right billing endpoint automatically.
- OpenAI Chat Completions wire: request build (no tool_choice, max_tokens /
  `max_completion_tokens` tables, developer-role swap for gpt-5/codex),
  SSE streaming with upstream's tool-call assembly quirks (index-reuse fix,
  integer finish reasons, first-non-null reasoning), refusal/cache-stats
  parsing, empty-stream and truncated-call detection.
- Anthropic Messages wire: model-name normalization (dots→hyphens), adaptive
  extended thinking (`{"type":"adaptive"}` + `output_config.effort`; legacy
  budget_tokens only for legacy models; none for haiku), signed thinking-block
  capture + replay, tool_result merging for parallel calls, same-role merges,
  orphan stripping, empty-content placeholders, tool/id sanitization, prompt
  caching (`cache_control` breakpoints, default-on for Claude), beta headers,
  `tool_choice: {"type":"auto"}`, SSE `error` events, per-model max_tokens.
- Per-provider reasoning wire shapes (OpenRouter verbosity-for-Claude rule,
  deepseek/zai `thinking`, nous `reasoning` dict, gemini shim
  `thinking_config`), error taxonomy with upstream's pattern buckets and
  retry/failover/compress flags, jittered backoff (2/60 API, 5/120 general,
  Retry-After capped 600s), streaming read-stall timeout.

**Tool system (`joey-tools`)** — port of `tools/`, `toolsets.py`:
- `Tool` trait + registry: TTL-cached `check` gating, dispatch with upstream's
  error envelopes (`[TOOL_ERROR]` sanitizer, capital-U `Unknown tool`),
  Python-`json.dumps`-compatible serialization (byte-identical envelopes),
  per-result persistence (`<persisted-output>` spill files) + per-turn budget,
  config-driven truncation limits with upstream marker texts.
- The full 9-strategy fuzzy matcher (incl. `trimmed_boundary`), all-matches
  ambiguity semantics, upstream re-indentation, a hand-ported difflib
  `SequenceMatcher`, post-match guards, verbatim error strings with
  did-you-mean snippets.
- Built-in tools with upstream-verbatim schemas, descriptions, JSON result
  envelopes, and guards: `read_file` (unpadded gutter, 100K budget, device/
  binary/credential blocks, dedup + loop blocks, ENOENT suggestions),
  `write_file` (sensitive-path refusals, fail-closed JSON/YAML/TOML gate,
  CRLF/BOM preservation), `patch` (mode enum incl. real V4A parse/apply,
  unified diff + post-write verification), `search_files` (rg-backed with
  upstream flags, mtime-sorted files mode, densified envelope, offset/
  output_mode/context), `terminal` (bash-only, interleaved stderr, sanitized
  env, timeout semantics + exit-code meanings + ANSI strip), `todo` (read
  mode, JSON summary envelope, `[>]`/`[~]` markers), `memory` (operations
  batch, locking, drift detection, exact-one remove, inventory responses),
  `web_search`/`web_extract` (Tavily payloads, storage footer + read_file
  continuation, image placeholders, secret-URL blocks), full DNS-resolving
  fail-closed SSRF guard, `skills_list`/`skill_view` (JSON envelopes,
  file_path, external dirs, disabled filtering).
- Toolsets with upstream memberships/descriptions (incl. `search`, `safe`,
  `debugging` with includes, `coding`, platform auto-bundles), schema
  sanitizer incl. reactive strippers.
- Filesystem checkpointing (`vcs.rs`) — port of `tools/checkpoint_manager.py`,
  **2026-07-24 rewritten (004-git-checkpoint-perf)**: replaced the original
  per-session shadow-git-repo design (new bare repo + eager full-tree initial
  commit on every session start) with a single shared bare git store at
  `~/.joey/checkpoints/store` (JOEY_HOME-aware), matching upstream's v2
  shared-store architecture. Per-project state (`refs/joey/<hash16>`, a
  per-project git index, and `store/projects/<hash16>.json` metadata) is
  keyed by `sha256(canonicalized_abs_path)[:16]`, so git's content-addressable
  object store deduplicates blobs/trees across every project and session.
  `CheckpointManager::new()` is now fully lazy — cheap path/hash resolution
  and a `git`-on-PATH probe only, no filesystem mutation — with store/ref/
  first-snapshot creation deferred to the first `checkpoint()` call, so the
  interactive prompt is never blocked on a startup-path scan/add/commit
  (FR-001/FR-002). Default excludes (build output, dependency dirs, VCS
  metadata, caches, venvs, media/archives, secrets, logs) are applied via
  the shared store's `info/exclude` (FR-003). Every git subprocess call is
  isolated from user global/system git config (`GIT_CONFIG_GLOBAL`/
  `GIT_CONFIG_SYSTEM=/dev/null`, FR-004) and bounded by a hand-rolled 5-second
  poll-loop timeout — no new dependency (`git2`) was introduced; still shells
  out to the `git` binary (FR-005, research.md R1/R5). Retention is enforced
  opportunistically and throttled (`.last_prune` marker, ≥1h between passes)
  at the tail of `checkpoint()`: 50 snapshots/project, 2GB total store cap
  (oldest-checkpoint-across-projects-first eviction), a 90-day stale-project
  window, and orphan pruning for deleted working directories (FR-007). Old
  per-session shadow-repo directories from the pre-rewrite design are
  discarded outright during the same pruning pass — **not migrated**, per
  the feature's explicit clarification that old per-session data is ephemeral
  (FR-009). `list()`/`revert()` externally-observable semantics (checkpoint
  numbering, added/modified/deleted file restoration) are unchanged from the
  pre-rewrite behavior; only the storage substrate moved. See
  `specs/004-git-checkpoint-perf/` for the full spec/plan/tasks trail.

**Agent loop (`joey-agent-core`)** — port of `run_agent.py`,
`agent/conversation_loop.py`, `agent/system_prompt.py`, `agent/prompt_builder.py`:
- The system prompt is upstream's text verbatim (branded): SOUL.md identity,
  help/task-completion/parallel/memory/skills guidance, model-family blocks,
  untagged environment hints, cli platform hint, project context files
  (`.joey.md`/`JOEY.md` → `AGENTS.md` → `CLAUDE.md` → `.cursorrules`) with
  threat scan + 70/20 truncation, `## Skills (mandatory)` + categorized
  `<available_skills>`, `═`-boxed MEMORY/USER PROFILE blocks with usage
  gauges, `Conversation started`/`Session ID`/`Model`/`Provider` tail;
  three-tier assembly, session-stable snapshot.
- Turn loop: max_turns 90 with upstream's summary-request finalization;
  tool calls execute regardless of finish_reason; finish=length continuation
  (4×); dangling-tail repair; unknown-tool repair chain + 3-strike abort;
  post-tool empty nudge + empty retries; retry counts = total attempts;
  `fallback_providers` failover with prompt identity rewrite; read-only tools
  parallel + `tool_delay` sequential spacing; `<untrusted_tool_result>`
  wrapping; interrupt handle with upstream's cancellation texts; incremental
  session persistence (assistant-before-tools, tool rows, error rows);
  usage accounting with the 4-chars/token estimator fallback.
- **Context compression** (port of `agent/context_compressor.py`,
  `agent/conversation_compression.py`, `agent/context_engine.py`,
  `agent/context_breakdown.py`, `agent/model_metadata.py` context catalog):
  the full `ContextCompressor` engine — threshold math (0.50 with the 0.75
  raise-only floor under 512K windows), usage-driven `should_compress` with
  anti-thrash and 600s failure cooldowns (persisted), window selection
  (protect_first_n 3 / protect_last_n 20, tool-group alignment, user/assistant
  anchors), aux-model summarization with upstream's verbatim prompt/prefix/
  end-marker text (byte-diffed against upstream), deterministic fallback
  summary + `abort_on_summary_failure` freeze path, tool-result pruning and
  media stripping, DB-backed compression locks with lease refresh, in-place
  `archive_and_compact` session rewrite (archived rows stay searchable),
  cached-system-prompt refresh, the model context-length catalog + provider-
  error context probe, and loop integration at all three upstream points:
  preflight pressure check (`📦 Pre-API compression:`), post-response usage
  tracking, and 413/context-overflow compress-and-retry (3-attempt cap, exact
  disabled-compaction guard messages). CLI: `/compress` (`/compact`) with
  force/focus semantics and upstream feedback strings; `/usage` shows the
  context block; `/status` shows context usage.

**Cron (`joey-cron`)** — port of `cron/`:
- Hermes-compatible `jobs.json` (`{"jobs", "updated_at"}` envelope, nested
  `repeat`, full field set + unknown-field round-trip, tolerant repairing
  load), croniter-compatible matcher (DOW 0/7=Sunday, Vixie DOM/DOW OR,
  seconds-last 6-field, names, DST-aware, configured timezone), lenient ISO +
  duration grammar with upstream display normalization, one-shot repeat=1 +
  delete-on-completion, advance-before-run at-most-once dispatch with claims,
  concurrent job execution with in-flight guard, flock'd load-modify-save +
  tick lock, grace/fast-forward rules, pause/resume/trigger with name-or-id
  resolution, heartbeat files, upstream output documents (incl. FAILED) with
  retention pruning and 0700/0600 permissions, cron prompt-contract hint.

**MCP client (`joey-mcp`)** — port of the client side of `tools/mcp_tool.py`:
- Stdio JSON-RPC with the SDK's exact handshake (`2025-11-25`, clientInfo
  `mcp/0.1.0`, ids from 0), upstream name sanitization (`[^A-Za-z0-9_]`→`_`),
  paginated `tools/list` with capability gating, the exact result envelopes
  (`{"result"}`/`{"error"}`/structuredContent, credential-sanitized),
  `mcp_servers` config loading with `${VAR}`/`${env:VAR}` interpolation,
  safe-mode gate, and the full exfiltration-shape security filter, filtered
  safe-env spawning + command resolution (incl. the managed-node tree),
  timeouts (300s/60s), initial-connect retry with backoff, stderr to
  `logs/mcp-stderr.log`, graceful shutdown, schema normalization.

**Gateway core (`joey-gateway`)** — port of the `gateway/` spine:
- Upstream session-key grammar exactly (DM fallback chain, thread-suppresses-
  per-user-isolation, profile namespace parameter, WhatsApp canonicalization
  incl. lid-mapping walk), full `SessionSource` (20 fields, byte-compatible
  to_dict/from_dict incl. scope_id/guild_id reconciliation), `Platform` enum,
  full `MessageEvent` + command helpers, `SendResult` + error-kind classifier,
  `PlatformAdapter` mirroring `BasePlatformAdapter` (capability flags, default
  methods, fence-preserving message splitter).

**CLI (`joey-cli`)** — port of `hermes_cli/`, `cli.py`:
- Upstream parser shape: `-z/--oneshot`, the `chat` subcommand (`-q`, `-m`,
  `-t`, `--provider`, `-Q`, `-r`, `-c`, `--max-turns`, `--pass-session-id`,
  `--yolo`, `--safe-mode`), top-level `-m/-r/-c/-t/--provider/-s/--max-turns/
  --usage-file`, pre-parse `-p/--profile` re-pointing `JOEY_HOME`.
- One-shot with upstream's exit codes and stderr texts, `--usage-file` JSON
  report, platform-toolset resolution, provider auto-detect,
  `JOEY_INFERENCE_MODEL` honored only here.
- REPL: Ctrl-C interrupt (second press within 2s force-exits), the full
  upstream slash-command registry (73 names; `/q` = `/queue`, unique-prefix
  expansion, upstream unknown/ambiguous texts; ~22 implemented, the rest
  answer honestly that they're not available yet), persistent
  `~/.joey/.joey_history`, `❯` prompt, exit outro with resume hints, banner
  with model/context/cwd/session/tools/tips, dim Reasoning box + tool-progress
  modes, interactive streaming overlay.
- The full `joey model` setup wizard (`select_provider_and_model` +
  `model_setup_flows`): current-model/active-provider header (custom
  base-url matching, credential auto-detection, stale `OPENAI_BASE_URL`
  custom detection), canonical provider rows with upstream `tui_desc` labels
  + "← currently active" marker + `model_catalog.excluded_providers`
  filtering, saved-custom-provider rows, `--refresh` cache clearing.
  Flows: the generic API-key flow (first-time masked key entry saved to
  `.env`, [K]eep/[R]eplace/[C]lear recovery, signup URLs, base-URL override
  persistence, model resolution models.dev → curated (≥8) → live `/models`
  probe, provider/base_url/api_mode persistence + `deactivate_provider` +
  stale-`OPENAI_BASE_URL` cleanup), the **Z.AI endpoint picker** (four
  official endpoints + custom proxy, defaulted to the active endpoint,
  `GLM_BASE_URL` persistence), OpenRouter (curated∩live tools-filtered
  catalog with free/default badges, live $/Mtok pricing columns), Anthropic
  (credential reuse/reauth menu, masked API-key entry clearing the OAuth
  slot), custom endpoints (URL+key prompts, local `/v1` hint, probe with
  `/v1`-toggle fallback swap, explicit API-mode picker with URL detection,
  probe-driven model pick, context length, display name, `custom_providers`
  persistence + dedup + remove flow). Model selection puts the current model
  first with a marker, aligned In/Out/Cache pricing columns, custom-name
  entry, skip row, and the expensive-model guard (models.dev pricing,
  $20/$100 per-Mtok thresholds, confirm prompt).
- Model catalog: curated per-provider lists (zai's 8-model GLM list,
  anthropic, openai-api, deepseek, gemini, nous, xai incl. the disk-cache-
  derived xAI list with promote-top + curated extras), the models.dev
  registry port (in-mem → fresh-disk → network → stale-disk cache hierarchy,
  1h TTL, agentic filter: `tool_call` minus noise patterns minus hidden
  Google models), the generic `/models` probe with `/v1` toggling and
  Anthropic-mode headers, OpenRouter pricing fetch.
- Subcommands matching upstream semantics:
  `config show|edit|get|set|unset|path|env-path` (bare = show, masked
  echoes, exit codes), `doctor` (sectioned report, `--fix`), `cron` (bare =
  list, add/rm/delete aliases, create flags, `run <job>` = trigger-now,
  `status`, card list; `tick --loop` runs the standalone scheduler),
  `mcp add|remove|list|test` against `mcp_servers` config, `skills list`,
  `tools --summary|list|enable|disable` on `platform_toolsets`, `version`
  (= `-V`), first-run setup guard. SIGPIPE-safe; piped-stdin batch mode.

**Skills library** — all 73 upstream skills (19 categories, 453 files) are
bundled and rebranded (env vars, paths, CLI names; upstream attribution URLs
preserved; install instructions adapted to the Rust binary), plus the
port-only `software-development/rust-review` skill.

**Verified end-to-end:** `cargo test --workspace` — 31 suites, 520 tests, 0
failures, 0 warnings. Live-verified command surfaces: one-shot exit codes,
config round-trips incl. secret masking, cron create/pause/resume/run/remove/
status, mcp add/list/test incl. security rejection, first-run guard, slash
prefix resolution, resume by id/title, `joey model` TTY guard + `--refresh`,
and the Z.AI endpoint probe (live: bogus key probes all four endpoints,
falls back to the default URL, caches nothing).

## Bug-sweep / panic-hardening pass (feature 006, 2026-07-28)

Audited all `.unwrap()` / `.expect()` / `panic!()` / `unreachable!()` sites
across the 7 core crates for external-input exposure. Classification:

- **external-input**: file/buffer that touches untrusted data (tool results,
  MCP JSON-RPC, web content, config files, provider SSE) — must not panic.
- **safe**: provably-infallible (static regex compilation, post-condition
  get/get_mut, json! object_mut) — retained with an inline `// SAFETY:` comment.
- **internal**: internal Mutex `.lock().expect()` — poisoning only occurs on a
  prior panic-while-locked; retained with SAFETY comments per the constitution.

Result: **0 unhardened external-input sites** across all 7 crates.
`scripts/audit-external-input-unwraps.sh` (FR-010) confirms PASS (exit 0).

Per-crate summary (external → 0, safe+comment count):

| Crate | Sites hardened | Technique |
|---|---|---|
| joey-mcp | 8 | SAFETY comments on static regexes; mutex→unwrap_or_else+warn! on tool provenance |
| joey-gateway | 0 | Already clean |
| joey-cron | 7 | SAFETY comments on regex captures + guarded Options |
| joey-core | 12 | SAFETY comments on mutex locks, static regexes, post-insert get_mut |
| joey-providers | 9 | SAFETY comments on json! object_mut, guarded auth/slot access |
| joey-tools | 7 | SAFETY comments on static regexes, unreachable! after is_array check |
| joey-agent-core | 28 | SAFETY comments on LEDGER mutex, static regexes, last()/last_mut() guards |

No public surface changed: SCHEMA_VERSION=22, no CLI flags, no config keys,
no trait signatures modified. All changes are additive SAFETY comments and
the audit script improvements (cfg(test) filtering, standalone test-file
detection).

## Terminal async performance & streaming (feature 009, 2026-07-30)

Replaced the terminal tool's blocking `spawn_blocking(read_to_end)` output
capture with async chunked reads, and fixed the inert background-process
`notify_on_complete` path. This is an **internal implementation change** —
the observable contract is preserved:

- **Result schema unchanged**: the `terminal` tool still returns
  `{output, exit_code, error, exit_code_meaning}` with identical exit-code
  semantics (0 success, non-zero command code, negative signal, 124 timeout),
  timeout policy (180s default / 600s max), and CWD-marker / ANSI-strip /
  redaction post-processing. The merged-stdout/stderr single `os_pipe`
  contract is retained (the reader FD is wrapped in `tokio::io::AsyncFd` for
  native async readiness — no `Stdio::piped()` switch that would alter byte
  ordering). New dependency `tempfile` (justified in `research.md` R5).
- **`Tool` trait unchanged**; `ToolContext` gained two additive optional
  fields — `progress_sender: Option<UnboundedSender<String>>` and
  `interrupt_flag: Option<Arc<AtomicBool>>` — both defaulting to `None`
  (existing callers unaffected). Regression tests in `context.rs`.
- **New behavior (additive, upstream-faithful surface)**: live `ToolProgress`
  deltas during a command (≤1s latency), a "running… Ns" heartbeat for silent
  commands (≥2s), cooperative Ctrl-C cancellation mid-command, and a
  background reaper that drains child pipes into the existing `RingBuffer`
  and fires a one-shot completion notice. None of these alter any on-disk
  format, config key, CLI flag, or session-handle grammar.
- **Deliberate divergence (unchanged)**: the upstream-equivalent streaming
  reuses Joey's existing `AgentEvent::ToolProgress` variant (already consumed
  by both CLI and TUI) rather than adding a new event type — `ToolProgress`
  already carries the tool name + progress text, matching upstream's
  `tool_progress` callback surface.

Feature spec/tracking: `specs/009-terminal-async-perf/`.

## Live terminal output streaming + maximized TUI viewer (2026-08-16)

Additive follow-up to feature 009: realtime in-GUI terminal output. While a
`terminal` tool call runs, its output now live-streams into the TUI
transcript (last-10-line tail per command block) and a maximized full-
screen viewer shows the complete stream. Upstream Hermes streams terminal
output through the same `tool_progress` callback; Joey originally did too,
but the TUI could only show a one-line summary overwrite — raw output was
invisible until `ToolEnd`. The fix adds a dedicated surface while keeping
the upstream-visible one intact:

- **`ToolContext`**: new additive `output_sender:
  Option<UnboundedSender<String>>` (+ `with_output_sender` / `emit_output`,
  no-op when unset — existing callers unaffected). The terminal tool's
  `flush_chunk` now emits each throttled chunk on BOTH channels (progress
  for upstream-parity consumers, output for live-view consumers).
- **`AgentEvent::ToolOutput { name, chunk }`**: new additive variant,
  forwarded by a per-dispatch task in `ctx_for_tool` alongside the
  existing ToolProgress forwarder. The one-shot CLI renderer ignores it
  (verbose mode already shows the chunks via ToolProgress); the TUI
  accumulates it.
- **TUI**: `TranscriptItem::Tool` carries a bounded live-output accumulator
  (128 KB tail ring, line-boundary eviction); terminal items render a live
  tail while Running; Ctrl+O / clicking a terminal block opens the
  maximized viewer (takeover of the main screen below a transcript strip,
  same pattern as the expanded reasoning panel / NeuroCode explorer —
  explicit viewer wins precedence). Auto-follow tail pinning with
  freeze-on-scroll (↑/PgUp/wheel-up), hjkl/g/G in transcript focus,
  auto-retarget to each NEW terminal call while open, replay of finished
  calls' full output, Esc/Ctrl+O restores. Terminal ToolProgress events
  are ignored in the TUI (they'd duplicate the output stream).
- **Untouched**: terminal result schema, timeouts, CWD markers, redaction,
  background reaper, ToolProgress consumers (CLI verbose, gateway).
  Tests: `context.rs` (7), `terminal_streaming.rs` (2),
  `agent.rs` (2 incl. a real-bash end-to-end ordering test),
  `state.rs` (10), `app.rs` key tests (4), `widgets.rs` render tests (3).

## Animated header gradient bar (2026-08-16, TUI-only)

Joey-native TUI feature (no upstream equivalent to port): the header's
gradient underline is now an agent-active indicator. `anim::HeaderFlow`
drives a slow traveling brightness wave across the bar while a turn runs
(raised-cosine bump, ~8s traversal, breathing base lift; gradient colors
fixed — only brightness moves), with an asymmetric eased busy envelope
(~1s engage / ~0.8s settle, clamped to exactly-static when idle) and
phase continuity across busy↔idle transitions. `draw_header` takes the
animator as `Option<&HeaderFlow>` (None = the old static render,
backward-compatible for non-Tui callers); `Tui::tick_animations[_with_dt]`
latches `app.is_busy()` into it alongside the other animators, riding the
shared activity speed. Contract tests: idle == static byte-identical,
busy animates across frames, adjacent-cell color deltas stay graded.
Purely additive; no state-schema, event, or config changes.

## Agent stats page — live context-window stream (2026-08-16)

Joey-native TUI feature (no upstream equivalent): clicking the header's
right section (or Ctrl+A) maximizes an agent-stats page with a realtime
stream of the full context window. Backing surface: new additive
`AgentEvent::ContextSnapshot` (plus `ContextEntry` projection type) emitted
by `Agent::emit_context_snapshot` at every history mutation the turn loop
makes — user turn appended, each tool round flushed, pre-API and
post-tool compactions, final assistant message. The payload carries
per-message role/token/preview entries, system+history rough token
estimates (`estimate_tokens`), and the compressor's context window /
threshold / compression count. Purely observational: nothing about the
request path, wire format, or persistence changes; the one-shot CLI
renderer ignores it.

TUI side: `App` stores the latest snapshot (entries + aggregates + a
bounded 240-sample per-API-call usage series + turn count);
`draw_stats_page` renders a dashboard (context-usage bar with
green/amber/red thresholds, system-vs-history breakdown, session token
totals, per-call usage sparkline) above a one-line-per-message context
stream with the reasoning-panel scroll semantics (auto-follow tail,
freeze-on-scroll-up, re-pin at bottom; ↑↓/PgUp·PgDn/Home/End + hjkl/g/G in
transcript focus; wheel-over-page scrolls it). The header's right section
records a hit-test rect (`last_header_right_rect`) — the click target —
and the page takes main-screen precedence over the output viewer /
NeuroCode explorer / reasoning panels. Tests: agent-core end-to-end
(turn drives ≥3 snapshots with correct roles/growth), TUI state (5),
key/mouse (5), render (3).

## Crush-style tool output & diff formatting (2026-08-16, TUI-only)

Ported Crush's tool-result display conventions (`internal/ui/chat/tools.go`,
`unified_diff.go`, `ui/diffview/diffview.go`) onto Joey's transcript:

- **Envelope unwrapping** (`state::display_result_content`): tool bodies and
  the maximized viewer show the payload, never the JSON envelope —
  `{"output":…,"exit_code":…}` → its `output` string, `{"error":…}` → the
  message. Non-JSON results pass through unchanged.
- **JSON pretty-printing** (`state::pretty_json_if_parses` /
  `format_tool_result_for_display`): results that are themselves JSON
  (MCP/list outputs, tool args) render with 2-space indent — no literal
  `\n` escape runs. `serde_json` added to joey-tui (already a workspace dep).
- **Line-numbered code gutters**: terminal live tails (absolute numbers
  across the tail window), finished collapsed bodies, expanded tool
  results/args, and the maximized viewer all render `N │ content` rows with
  a dimmed separator; blank lines are preserved as numbered rows (fixed a
  pre-existing `wrap()` bug that dropped them via textwrap collapse).
- **Dual-gutter diffs** (`parse_diff_lines` + FileDiff rendering): unified
  diffs carry old/new line numbers parsed from hunk headers — context shows
  both, deletions old-only (new blank), insertions new-only (old blank),
  hunk headers render as `… …` dividers with colored +/- markers, exactly
  crush's diffview semantics.
- **Viewer generalization**: the maximized viewer now handles ANY tool call
  (terminal or generic), header adapting (`$ cmd (exit N)` vs
  `tool summary`); clicking any tool block opens it; live-follow retargets
  to each new tool call.

Tests: formatting helpers (4), gutter/envelope render contracts (8),
visual end-to-end rows (2); two legacy indent tests updated to the gutter
contract. No event/schema/config changes — display-layer only.


## Rayon parallelization of CPU-bound hot paths (2026-08-16)

Added `rayon 1.12` as a workspace dependency (already in the lockfile
transitively via sysinfo — zero binary-size cost) and moved the
CPU-intensive, non-async-appropriate work onto the rayon pool:

- **Terminal tool** (`joey-tools`): the post-command pipeline (head/tail
  truncation → ANSI strip → secret redaction → file-mutation detection)
  now runs inside `spawn_blocking`, with the internals parallelized —
  `strip_ansi_par` (line-boundary chunking, 256 KB threshold) and
  per-file stat+hash fan-out for mutation snapshots/detection.
- **Redaction** (`joey-core`): `redact_secrets_par` — line-boundary
  chunked rayon map over the ~30-pattern regex cascade (512 KB
  threshold); every pattern class is line-local so chunking is safe.
- **Diff engine** (`joey-tools::file_tracker`): `generate_diff` rebuilt
  with parallel line hashing (rayon::join), common prefix/suffix
  trimming (a one-line edit in a 50 K-line file collapses the quadratic
  LCS core to a tiny region instead of a 2.5×10⁹-cell table), hash-based
  equality in the LCS hot loop, and a wholesale-rewrite guard that caps
  memory on pathological inputs. `drain_pending_diffs` fans the per-file
  read+decode+diff work across cores (order-preserving collect).
  Byte-identical to the original algorithm — pinned by a reference-oracle
  test across 8 edit shapes plus fast/bounded tests for 50 K-line edits
  and 20 K-line rewrites.
- **NeuroCode ingestion** (`joey-neurocode`): `ingest_project` split into
  a parallel phase-1 (per-file read + tree-sitter parse via par_iter,
  order-preserving) and the unchanged sequential phase-2 graph upserts.
  Measured on a frozen copy of this repo's tree: identical output
  (2744 artifacts / 3842 edges), 2.64 s → 1.69 s (~1.56× faster).
- **Agent turn loop** (`joey-agent-core`): parallel tool-batch
  post-processing (untrusted-content wrapping + preview extraction +
  exit-code parsing fan out per result on the rayon pool; event emission
  and history pushes stay sequential for byte-identical ordering), and
  `estimate_messages_tokens_rough` parallelizes per-message shadow-JSON
  serialization above a 24-message threshold.

All parallel/sequential equivalence points are contract-tested
(strip_ansi, redaction, generate_diff vs a reference oracle). Rayon work
never runs on tokio's async workers: call sites wrap the pool in
`spawn_blocking` where they're on the async runtime.

## Deliberate deviations (not oversights)

- **Anthropic OAuth "Claude Code" impersonation is NOT ported.** Upstream,
  when using an OAuth/subscription token, injects a "You are Claude Code"
  system prefix, rewrites its own branding to "Claude Code", renames tools to
  `mcp__*`, and spoofs the `claude-code` user-agent + beta headers so
  Anthropic's subscription billing accepts third-party traffic. That
  circumvents Anthropic's terms, so joey-agent omits the entire identity
  layer (honest OAuth token *detection* and Bearer-vs-`x-api-key` selection
  are ported). Consequence: Anthropic subscription OAuth tokens will likely
  be rejected by Anthropic; use an API key. See
  `crates/joey-providers/src/anthropic.rs` for the policy comment.
- `gemini` runs through Google's OpenAI-compatible shim (upstream's native
  Gemini REST adapter is unported); `xai` refuses with a clear error rather
  than silently degrading (upstream uses the unported codex_responses wire);
  `nous` uses plain API-key auth (device-code OAuth unported).
- `joey home` is a port extension (labeled in help); the standalone scheduler
  lives under `joey cron tick --loop` (upstream runs it inside the gateway);
  `-q` at top level was removed in favor of upstream's `-z` (use `joey chat
  -q` for the chat-path form).
- The SOUL.md identity line reads "based on Hermes Agent by Nous Research"
  rather than claiming Nous authorship; the prompt threat-scanner keeps the
  `HERMES` env-var token alongside `JOEY` so migrated homes stay protected.
- **Local default model deviation:** upstream ships `model.default ""` +
  `model.provider "auto"` (unset → first-run setup). This install bakes
  `glm-5.2` / `zai` into `DEFAULT_CONFIG_YAML` as the out-of-box default (a
  deliberate local preference, asserted in `config::tests`). Setup-wizard
  behavior is unaffected: with no API keys configured the first-run guard
  still triggers.
- **Setup-wizard scope:** the wizard implements upstream's numbered-list
  fallback UI (the curses radiolist/searchable menus are unported); the
  Gemini free-tier probe (needs the unported native Gemini adapter), the
  "Configure auxiliary models..." submenu, secret-source suffixes
  (Bitwarden/1Password plugins), and the remote catalog manifest are
  unported — curated in-repo snapshots (upstream's own fallbacks) are used.
  The Anthropic flow's "Claude Pro/Max subscription (OAuth login)" option
  explains the standing impersonation decision and directs to API keys.

## Partial

- **Providers:** OpenAI-compatible + Anthropic wire modes, plus the complete
  GitHub Copilot provider path: OAuth device login, GitHub-to-Copilot token
  exchange/refresh, live model catalog, endpoint discovery, Chat Completions,
  Anthropic Messages, and OpenAI Responses routing by model. Generic
  Codex/Responses for non-Copilot providers, Bedrock, native Gemini REST,
  Vertex, and Azure are not ported (`ApiMode::CodexResponses` still refuses
  outside Copilot). No credential pools,
  request_overrides/service-tier plumbing, Z.AI adaptive long backoff, or
  per-provider timeout table. The model catalog covers the picker surface
  (curated lists + models.dev registry); the wider `model_metadata.py`
  capability/vision lookups remain unported.
- **Compression edges:** the codex app-server compaction path, pixel
  re-encoding of oversized images (`try_shrink_image_parts_in_messages`),
  the legacy session-rotation branch (`compression.in_place: false` — the
  port always compacts in place, upstream's shipped default), live context-
  length probes (OpenRouter/models.dev/Anthropic endpoints; the offline
  catalog + provider-error probe are ported), plugin context engines
  (`context.engine` — the trait is ported), and `/compress here|--preview|
  --aggressive` (honest notices) are unported. Thinking-only prefill
  continuation is likewise unported.
- **Tools:** `session_search`, `delegate_task`, `clarify`, `process`
  (background procs), `cronjob` (agent-callable) remain stubs; `terminal`
  `background`/`pty` params return honest not-supported errors; document
  extraction (.docx/.xlsx), lint/LSP result fields, the memory threat-scan/
  approval gate, skill usage counters, and non-Tavily web backends are
  unported. MCP tools are not yet injected into the tool registry.
- **Cron:** delivery/notification, the executions ledger (`cron runs`/
  `history`), per-job model/provider/toolset runner wiring, inactivity
  timeout, per-run session persistence, and `cron edit` are unported (job
  fields are stored and round-trip).
- **MCP:** HTTP/StreamableHTTP/SSE transports, OAuth server auth, sampling/
  elicitation, keepalive/reconnect/circuit breaker, resources/prompts
  utility tools are unported (config keys parse; `url` servers refuse).
- **CLI:** `-s/--skills` is accepted but does not preload skills;
  `config check|migrate`, `doctor --ack`, `mcp serve/catalog/login/reauth`,
  `skills` beyond `list`, `tools post-setup`, and the version update check
  answer with honest not-available messages. `--image`, `-w/--worktree`,
  `--accept-hooks`, `--checkpoints`, `--no-restore-cwd` are not offered.
  `--tui/--cli` exist but with joey-native semantics (upstream's trio also
  includes `--dev`): the TUI is the DEFAULT interactive interface;
  `--cli` selects the line REPL (`--cli` beats `--tui`; `JOEY_TUI=0|false`
  env-opts-out, any other value or unset → TUI; non-terminal stdio falls
  back to the line REPL). `--dev` is not offered.
- **Skills self-improvement** (curator, `skill_manage` authoring/patching)
  is not ported; skills are discovered, indexed into the prompt, and viewable.

## Spec-Kit Development IDE (feature 010, 2026-08-03)

`joey-speckit-ui` is a **Joey-original crate** (no upstream Hermes
equivalent). It extends the visual UI from `specs/001` from a read-only
viewer into a full authoring and execution surface for Spec-Kit artifacts.

**Status: Complete (all phases implemented and tested)**

- **Artifact authoring (US1):** discovery, GET/PATCH for every artifact kind
  (spec, plan, tasks, checklists, research, data-model, contracts, quickstart,
  constitution) with whole/section scope, conflict-checked writes (FR-020),
  structural validation (FR-007), and rendered outlines (FR-006).
- **Workflow controls (US2):** step catalog with derived readiness states
  (FR-022), run configuration with staged/direct change modes (FR-010),
  out-of-process agent execution via `joey` CLI subprocess (FR-011),
  interaction streaming over WS (FR-012/013/014), project-level overrides
  (FR-034).
- **Change review (US3):** Git-backed staging area with temp worktree for
  staged mode (FR-016), hunk-level accept/reject (FR-016/SC-016), recovery
  with safe checkpoints (FR-017/033), re-run linking (FR-019).
- **Workspace (US4):** resizable pane layout (FR-002), workspace preferences
  (FR-026), cross-artifact search (FR-025), keyboard navigation + ARIA (FR-027).
- **Readiness (US5):** dependency-graph-based stale propagation (FR-021),
  traceability (FR-023/032), streamed JSONL history (FR-018/031).

**Deliberate deviation:** the backend drives the agent **out-of-process**
(spawns `joey` CLI as a subprocess, streams stdout/stderr/interactions over
WS) rather than linking `joey-agent-core` in-process. This is Constitution
Principle VI (depend only on the CLI contract) and matches the existing
`specs/001` `/speckit-implement` wrapper pattern. An in-process reimplementation
is explicitly rejected.

**Dependencies added:** `gix 0.66` (pure-Rust git read/object side, ~50s
compile-time delta), `which 7`, `dirs 6`. Frontend: `diff 5.2`, `split.js 1.6`.

**New on-disk format:** JSONL history at
`~/.joey/speckit-ui/history/<feature-id>.jsonl`, `schema_version: 1`
mandatory. Round-trip + partial-line-tolerance + migration-stub tests in
`tests/history_jsonl_roundtrip.rs`.

## Deferred (matches upstream's own "defer for a first port" guidance)

- The 20 messaging platform adapters (Telegram, Discord, Slack, WhatsApp,
  Matrix, …) — the `PlatformAdapter` trait + session spine are faithful;
  concrete adapters are additive.
- The FastAPI dashboard / web server and the Electron desktop app.
- The TUI-gateway JSON-RPC protocol, ACP editor adapter, relay/connector.
- Kanban multi-agent coordination, projects, blueprints, memory providers
  (Honcho/mem0/…), computer-use, TTS/STT/voice, image/video generation,
  browser automation.
- The 6 terminal backends beyond `local` (docker/ssh/singularity/modal/daytona).
- Research tooling: batch runner, trajectory compressor, mini-swe runner.

## Branding conversion (complete)

`~/.hermes` → `~/.joey` · `HERMES_*` → `JOEY_*` · `hermes` command → `joey` ·
`hermes-*` toolsets → `joey-*` · package `hermes-agent` → `joey-agent`. MIT
license and upstream attribution retained throughout. The `mcp__` wire prefix
and `§` memory delimiter are kept identical for interoperability. Upstream
attribution URLs (github.com/NousResearch/hermes-agent,
hermes-agent.nousresearch.com) are intentionally left un-rebranded.

## Dynamic LLM Model Selector (feature 011, 2026-08-04)

**Status**: Complete (Rust-native implementation; multi-provider generalization).

Upstream's `specs/003-dynamic-llm-selector` is GitHub-Copilot-specific. Joey
generalizes the candidate pool to *any provider exposing a live catalog*
(Copilot, OpenRouter, ZAI, Anthropic, …) while keeping Copilot as the canonical
source. The implementation lives in a new dedicated crate `joey-llm-selector`
(Constitution I/VI).

**Deliberate deviation**: upstream's `auto` cost-scorer (`agent/model_metadata.py`,
`model_cost_guard.py`) was deliberately NOT ported — it is tightly coupled to
the GitHub-Copilot-specific Python runtime. Joey builds a clean-room capability
+ cost scorer (`ColdStartScorer`) that is multi-provider and has no external
dependency. This is documented in `specs/011-dynamic-llm-selector/research.md §1`.

**On-disk format**: `~/.joey/llm-selector/allocations.json` (schema_version 1,
versioned public format, atomic write via `atomic_json_write`). Machine-global
across profiles via `process_joey_home()` (FR-014).

**Implemented**: cold-start scorer, catalog consolidation (Copilot + models.dev),
per-module allocation at 3 intercepts (main turn, compression, subagent pending),
detached tokio diagnoser with heuristic performance estimator, learning loop
with budget bounds, pin/unpin, `/llm-selector` slash + CLI command, auto-disable
on empty catalog, degraded fallback chain, cross-profile map sharing.

**Deferred**: subagent intercept (T028 — threading allocator through
orchestration layer); real LLM-based diagnoser call (heuristic estimator used
for now — future enhancement via `joey-providers` chat client).

## Spec Studio — Visual IDE (feature 012, 2026-08-05)

**Status**: Complete (Rust-native; Joey-original, no upstream equivalent).

Extends `joey-speckit-ui` (the Joey-original crate from `specs/001`/`010`)
with the **Meaning Layer**: a lossless concrete-syntax-tree parser built on
the already-present `pulldown-cmark`, a derived semantic graph, and a
byte-anchor patch engine for round-trip-safe visual editing. Like `specs/010`,
this is a Joey-original crate — there is no upstream Hermes parity surface to
track. The CST + meaning + patch modules are strictly additive behind the
existing `parser/`/`writer.rs`/`editor.rs` contract (Constitution VII).

**No new on-disk canonical format**: the CST is an in-memory derivation, never
persisted. The one new on-disk file is the Overlay-layer UI-state JSON at
`~/.joey/speckit-ui/ui-state/<repo-hash>-<branch>.json` (`schema_version: 1`,
write-tree-isolation tested in `tests/ui_state_roundtrip.rs`), which extends
the `specs/010` `~/.joey/speckit-ui/` convention rather than forking it.

**New runtime dependency**: CodeMirror 6 (frontend only, scoped to the two
non-structured editing depths in FR-015). Measured bundle delta: +172.14 KB
gzipped (15% over the pre-build estimate); zero Rust compile-time impact.
Full cost table in `specs/012-spec-studio-visual-ide/research.md` §2.

## Windows Platform Support (feature 014, 2026-08-05)

**Status: Complete for the terminal tool; build + tests green on Windows.**

The agent now compiles and runs on Windows (x86_64-pc-windows-msvc).
`cargo build --workspace` and `cargo test --workspace` succeed on both
Windows and Unix with no regressions. The terminal tool executes commands
via bash (Git Bash, preferred) or PowerShell (fallback when bash is absent),
with streaming output, correct exit codes, CWD tracking, timeout, and
cooperative interrupt on both platforms.

**What changed**: a single file, `crates/joey-tools/src/tools/terminal_tool.rs`.
The unguarded Unix-only streaming path (`std::os::unix::io::AsRawFd`,
`tokio::io::unix::AsyncFd`, `libc::read`/`dup`/`close`) was extracted behind
`#[cfg(unix)]` gates. A cross-platform `ChunkSource` trait +
`OutputChunkStream` boundary was introduced, with `UnixFdReader` (Unix,
byte-for-byte identical to feature 009) and `WindowsPipeReader` (Windows,
merges child stdout+stderr via `tokio::select!`) as concrete impls. Shell
discovery was generalized from `find_bash()` into `Shell` enum +
`resolve_shell()` with bash-first → `pwsh` → `powershell` fallback (cached
per-process). A PowerShell-dialect wrapper script (`$LASTEXITCODE`, `$PWD`,
`-NoProfile`) mirrors the bash path's exit-code + CWD-marker contract.

**Zero new dependencies.** The feature rearranges existing workspace deps
(`tokio`, `which`, `tempfile`) behind cfg gates.

**Deliberate limitation**: PTY support remains a stub on all platforms
(`portable-pty` is declared but unused; `pty=true` returns "not supported").
CWD tracking via Git Bash's `$PWD` returns MSYS-style paths (`/d/...`) that
`Path::is_dir()` rejects on Windows — the bash CWD-tracking test is gated
`#[cfg(unix)]`; PowerShell fallback uses native Windows paths and works.
A future improvement could translate MSYS paths to native Windows paths.

**Audit note**: the rest of the codebase (joey-core, joey-mcp, joey-cron,
joey-cli, joey-tools sibling files) was already correctly cfg-guarded before
this feature — the terminal tool was the sole compile blocker.

## NeuroCode — Enterprise Java & Pega Rule System Coding Agent (feature 015, 2026-08-13)

**Status**: Deliberate-deviation subsystem (Joey-original, no upstream
equivalent).

`joey-neurocode` is a new library crate (`crates/joey-neurocode/`, Constitution
I) that gives the agent dependency-graph-aware context, complexity-tier
routing that composes with `specs/011`'s `ModelAllocator`, and a build/verify
feedback loop for enterprise Java and Pega Platform codebases. It consumes
`joey-llm-selector`'s trait upward and is consumed by `joey-agent-core` via a
narrow `NeuroCodeEngine` trait (Constitution VI) and by `joey-cli` for the
`/neurocode` command.

**Deliberate deviation — no Qdrant; SQLite + FTS5 instead.** The source plan
proposed Qdrant (a separate vector database server). This is rejected for this
workspace: Qdrant adds a second storage engine, a runtime server/dependency,
and deployment complexity. The workspace already bundles SQLite with FTS5
(`SCHEMA_VERSION = 22`; `joey-core::state` probes/uses FTS5), so the structural
knowledge graph is stored in a per-project SQLite DB
(`~/.joey/neurocode/<project-hash>/graph.db`). FTS5 with BM25 ranking and
symbol-aware tokenization handles FR-007's retrieval, and the graph edges
(implements/injects/references) are exact-match typed traversals — not
nearest-neighbor searches — for which a vector store adds no value (Constitution
VIII, lean deps). Embedding-model retrieval is deferred to a future trait
extension (research.md §2, §6).

**Deliberate deviation — tree-sitter for AST parsing.** Upstream's source
plan uses a Python-based parsing approach. Joey adds `tree-sitter = "0.26"`
plus one grammar crate per supported language — every programming language
with a grammar under the tree-sitter org
(https://tree-sitter.github.io/tree-sitter/): Java, Python, JS/TS/TSX, Go,
Rust, Ruby, PHP, C#, C, C++, Scala, Haskell, Julia, OCaml, Bash, Verilog,
Agda (`crates/joey-neurocode/src/parse/registry.rs` is the authoritative
list; long-tail languages fall back to the heuristic extractor) — each
~150-300KB compiled, no transitive runtime deps, C source bundled via `cc`
— to satisfy FR-006's mandate for deterministic, syntax-aware parsing
(type/method/field boundaries, annotations, imports, injection points)
that does not rely on the LLM to guess structure. Regex heuristics fail on
generics/annotations/nested classes; a hand-written Rust parser is rejected
as enormous and less correct than the maintained grammars (research.md §3).
Markup/data grammars on the tree-sitter supported list (CSS, HTML, JSON,
JSDoc, Regex, embedded-template) are deliberately not compiled in: they
produce no type/method/import structure for the dependency graph.

**On-disk format**: per-project SQLite DB at
`~/.joey/neurocode/<project-hash>/graph.db` (machine-global across profiles via
`process_joey_home()`), schema v2 (tables: `code_artifacts` with the additive
`signature` column — declaration headers for methods/fields, migrated in place
on open; `graph_edges` including the `MemberOf` edge kind for member→type
membership, distinct from `Injects`; `code_artifacts_fts`, `patterns`,
`anti_patterns`, `domain_knowledge`, `domain_knowledge_fts`, `schema_meta`).
Round-trip + acyclic-DAG + disabled-state regression tests in
`crates/joey-neurocode/tests/`.

**Disabled state is byte-identical to today**: with `neurocode.enabled = false`
the engine's `classify()`/`assemble_context()` are never called, no messages are
injected, and the system prompt bytes are unchanged (FR-020, SC-008 — asserted in
`tests/regression_disabled.rs`).

Full design trail and every dependency decision against the constitution:
`specs/015-neurocode-enterprise-java/research.md`.

**Follow-up (2026-08-16) — context-quality overhaul ("useful context for the
LLM")**. The assembly pipeline was rebuilt around what the model actually
receives:

- **Request-text discovery** (`context::discovery`): backtick-quoted spans,
  CamelCase/snake identifiers, dotted/`::` references, and file-path mentions
  are extracted from the free-text request. Identifiers seed target lookup
  (`CodingRequest.active_symbols`) and the classifier's scope-fanout signal;
  file mentions become `active_file`. Previously the intercept passed empty
  symbols and FTS-matched every ≥3-char word with AND semantics — mostly empty
  or noisy results.
- **Symbol-match ranking** (`best_symbol_match` / `resolve_query_node`): FTS
  also indexes `declared_dependencies` text, so dependents' FQCNs crowd the
  true node out of a small limit. Both lookups now fetch generously and
  re-rank in Rust: type-level exact → qualified suffix → exact member → first
  type-level → first hit.
- **Ranked, budget-capped best-first expansion**: a `BinaryHeap` frontier
  ordered by `ExpansionReason::rank` (inherits > implements > members >
  injects > exchanges > references) replaces arbitrary `Vec::pop()` BFS — the
  implemented interface always survives a tight budget ahead of dependents.
  Members render inside their type's roster, not as separate artifacts. A
  defensive pop cap (12× render budget) bounds hub-node traversal.
- **Rendering with actionable detail**: file paths (model can `read_file`
  directly), member rosters with captured declaration signatures
  (`public User findById(Long id)`), fan-in blast-radius warnings (≥5
  dependents), and an index-staleness note when a target file's mtime
  postdates `indexed_at` (paths resolve against the project root).
- **Schema v2** (`NEUROCODE_SCHEMA_VERSION = 2`): additive `signature` column
  (migrated in place; rows keep NULL until re-indexed) + `MemberOf` edge kind.
- **Signatures captured by every extractor**: Java/Python/JS-TS/Go/Rust
  tree-sitter grammars emit declaration headers; the heuristic extractor
  stores the trimmed source line.
- **Spring ≥4.3 implicit constructor injection**: a single-constructor class
  has its object-typed params recorded as declared dependencies (no
  `@Autowired` needed — the dominant modern style).
- **Tier routing follows classification**: `DefaultEngine` caches the last
  classified tier; `resolve_tier_model` now returns the tier model for the
  tier that actually served the request (previously always the ambiguous
  default — the core premise of tier routing was broken).
- **Per-turn assembly dedupe**: the agent-core intercept keys on the user
  text; retries and tool-loop iterations reuse the stashed context instead of
  re-running assembly (which also re-bumped anti-pattern hit counts). New
  user turns clear the key.
- **Query surface**: `definition` (exact declaration lookup with span +
  signature) and `references` (alias of dependents) query types implemented —
  previously advertised by the tool schema but rejected by the engine.
- **Tier budgets raised**: economical depth 2 / 2 primaries / 8 expanded;
  frontier depth 3 / 3 primaries / 24 expanded (was 1/1/5 and 2/3/20).
- **Conservative token estimation** (`context::tokens`): ~3.5 chars/token
  blended with a word-count floor (was `len()/4`, undercounting symbol-dense
  text).

Regression tests: `crates/joey-neurocode/tests/context_enrichment.rs` (9
cases: discovery, rendering, ranking, tier routing, staleness, hub warnings,
determinism).

**Follow-up (2026-08-15) — realtime assembly progress feed.** Assembly is now
streamable: `ContextAssembler::assemble_with_progress(request, tier, progress)`
invokes a callback with short stage descriptions ("locating artifacts" →
"expanded graph: N nodes pulled in" → "surfacing known anti-patterns" →
"surfacing domain knowledge", plus a "cold mode" notice), and the plain
`assemble` delegates to it with a no-op callback (byte-identical result —
asserted by `streaming_assembly_is_identical_and_reports_stages`). The
`NeuroCodeEngine` trait gained a default `assemble_context_with_progress`
(source-compatible; existing impls unchanged), overridden by `DefaultEngine`
to forward through `with_graph`. The agent-core intercept emits a new
`AgentEvent::NeuroCodeProgress { stage }` per stage live during assembly
(before the final `NeuroCodeContext` blob), and the TUI context panel renders
the current stage with an animated spinner plus a "↻ updated Ns ago" refresh
stamp (`state.neurocode_stage` / `neurocode_stage_at` / `neurocode_updated_at`;
cleared on deactivate). The line renderer consumes the new event silently
(same treatment as `NeuroCodeContext`). Regression tests:
`crates/joey-neurocode/tests/context_assembly.rs` (streaming parity + stage
coverage), `joey-agent-core/src/agent.rs` (`active_engine_streams_progress_events`
— streaming engine double verifies every stage forwards as a live event),
`joey-tui/src/state.rs` (`live_stage_streams_into_panel` + refresh stamp).

## Copilot reverse-proxy integration (2026-08-14)

**Status**: Deliberate-deviation extension (Joey-original, no upstream
equivalent). Uncommitted working-tree feature completed and verified 2026-08-14.

`COPILOT_API_BASE_URL` pointing at a host **off** `githubcopilot.com` (e.g. a
local AI Usage HUD reverse proxy on `127.0.0.1:8317` that owns upstream
Copilot auth, token refresh, and usage capture) activates a "custom
Copilot-compatible endpoint" mode (`joey-providers::copilot::custom_endpoint`):

- **No GitHub token exchange**: `CopilotAuth::with_endpoint` pins the endpoint
  and `credentials()` returns the raw GitHub credential (env var / `gh auth
  token`) + the pinned base URL; `build_client` constructs the pinned auth
  when the copilot profile's base URL is off-host. The proxy accepts the raw
  credential and owns upstream auth.
- **Routing magnet**: `resolve_profile` resolves EVERY `auto`-provider request
  to the `copilot` profile (vendor prefixes, bare family names, and foreign
  base_url hosts alike) so no request escapes the proxy — the proxy serves
  every model family. Explicit non-`auto` provider settings still win.
  `llm_selector::resolve_provider_name` mirrors the magnet so the Feature 011
  candidate pool targets the proxy's `/models` catalog.
- **Catalog via proxy**: `fetch_model_catalog` fetches `/models` from the
  pinned endpoint with the raw credential (60 s in-process cache — consulted
  per client build and by the OMO model set); `AvailableModelSet::
  from_connected_with_catalog` seeds every proxy catalog model id into the OMO
  model set (used by the REPL, TUI roster, and agent switching).
- **On-disk formats unchanged**; no schema/version bumps. Graceful
  degradation: unset var → identical to native behavior; unreachable proxy →
  catalog fetch fails, active model + static fallbacks still used.

Tests: `copilot.rs` (custom-endpoint detection, pinned-credential
no-exchange, catalog filtering), `profile.rs` (magnet covers all auto paths,
natively-routed otherwise — env-var tests share `TEST_ENV_LOCK`), `llm_
selector.rs`, `joey-omo/src/models.rs` (degradation). Verified end-to-end
against a live proxy: `-z` one-shot through `127.0.0.1:8317` logged
`JoeyAgent/1.0` requests with exact model passthrough (`gpt-5.4 → gpt-5.4`).

## `ai-usage-hud` first-class provider (2026-08-14)

**Status**: Deliberate-deviation extension (Joey-original). Promotes the AI
Usage HUD reverse proxy (~/Development/ai-usage-hud, `127.0.0.1:8317`) from
an env-var-only mode to a named provider, on top of the custom-endpoint
machinery above.

- **Profile**: `ai-usage-hud` (aliases `usage-hud`, `ai-usage`) registered in
  `joey-providers::profile` — Copilot wire semantics, base URL
  `http://127.0.0.1:8317`, env override `AI_USAGE_HUD_BASE_URL`, same GitHub
  credential resolution (`COPILOT_GITHUB_TOKEN` / `GH_TOKEN` / `GITHUB_TOKEN`
  / `gh auth token`).
- **Single source of truth for copilot-family dispatch**:
  `profile::is_copilot_wire(name)` replaces every hardcoded
  `name == "copilot"` check in `build_client`, `ProviderClient` (auth attach,
  chat/responses/messages header paths), `wire_model_name`, `doctor`,
  `llm_selector`, and `model_catalog` — new Copilot-wire providers can't
  silently drift from the registry.
- **Env-var magnet**: `AI_USAGE_HUD_BASE_URL` set (off githubcopilot.com)
  magnetizes `auto` resolution to the `ai-usage-hud` profile (the existing
  `COPILOT_API_BASE_URL` magnet keeps precedence for the copilot profile).
  `copilot::hud_endpoint()` is the shared resolver; `custom_endpoint()`
  falls through to it.
- **Setup wizard**: `flow_ai_usage_hud` — proxy health check
  (`copilot::hud_health_check` probing `/api/health`, fail-fast with the
  deploy remediation), GitHub credential prompt (device flow or manual
  token), catalog + model selection through the proxy, persists
  `model.provider=ai-usage-hud` + the proxy base URL. Listed in
  `CANONICAL_ORDER` for the `joey model` picker.
- **OMO**: `AvailableModelSet::from_connected_with_catalog` always seeds from
  the proxy catalog when the profile is `ai-usage-hud`; BC-010 billing
  aliases (`github-copilot`, `copilot`, `usage-hud`, `ai-usage`) registered
  for requiresProvider gating.
- **On-disk formats unchanged**; no schema bumps. Explicit provider settings
  (`--provider zai`) still win over the magnet.

Tests: `profile.rs` (registration, aliases, is_copilot_wire, explicit + magnet
+ real-host-guard resolution), `client.rs` (pinned proxy client, no
exchange, env override), `copilot.rs` (hud_endpoint, precedence, health
check), `llm_selector.rs`, `joey-omo/src/models.rs` (catalog seeding +
billing aliases). Verified live: `joey --model gpt-5.4 -z` served
`gpt-5.4 → gpt-5.4` via `/responses` with usage recorded in the proxy's DB
(neurocode tier override temporarily disabled for the clean-path check).

## UX parity & robustness passes (2026-08-15)

Four user-facing features plus a workspace-wide audit, all
regression-tested; workspace 0 warnings, ~1,440 tests green.

1. **TUI input history recall** — plain ↑/↓ in the TUI walks the shared
   `~/.joey/.joey_history` (same reedline-format file as the CLI;
   recall semantics ported from readline/reedline incl. draft
   save/restore). Transcript scrolling moved to Shift+Up / Ctrl+T / PgUp.

2. **Smart completions (port of `hermes_cli/commands.py::
   SlashCommandCompleter` + `SlashCommandAutoSuggest`)** — shared engine
   in `joey-tools::completion`: slash names/aliases, pipe-hint
   subcommands (`SUBCOMMANDS` parity), @-context refs (`@diff/@staged/
   @file:/@folder:/@git:/@url:` + fuzzy project file search with the
   upstream scoring tiers), path completions, size labels. CLI: reedline
   description menu (fixed a pre-existing `only_buffer_difference`
   misconfig that left the menu empty) + fish-style ghost-text Hinter
   (slash/subcommand remainder, history fallback). TUI: auto-popup +
   subcommand stage + @/path completion popup, background-refreshed file
   cache.

3. **NeuroCode live context panel** — new `AgentEvent::NeuroCodeContext/
   NeuroCodeActive` emitted from the turn-loop intercept; TUI renders a
   bottom-right live feed (tier/tokens/nodes/COLD + full context text,
   Alt+↑/↓ scroll) and a `⚡NEUROCODE` status badge.

4. **TUI engine-actor decoupling** — `joey-cli/src/engine.rs`: the Agent
   lives on a dedicated engine task; UI ↔ engine over EngineCommand/
   EngineEvent channels; UI loop is one `select!` (events / input /
   frames) and never awaits compute. Ctrl-C escalation: 1st press
   interrupt, 2nd within 2s = force-kill (abandon the task, rebuild the
   agent from the session DB, respawn). Heavy jobs (`/neurocode index`)
   run on the engine's blocking pool under the same regime.

5. **ai-usage-hud wire fixes** — Responses-wire input items now carry
   `type:"message"` (typeless items are silently dropped by the Responses
   API and the HUD proxy — every prompt previously got the same generic
   greeting); new `AgentConfig.model_pinned` (`--model`, `/model`,
   agent picker, delegation) blocks NeuroCode tier rewrites of explicit
   model choices.

6. **Workspace robustness audit** — 19 real bugs found and fixed across
   all crates, highlights: inverted checkpoint-retention pruners (deleted
   the NEWEST snapshots; rewritten via commit-tree chain rebuild),
   PreToolUse-denied tools executed anyway, loop-nudge phantom
   tool_call_id (Anthropic 400), engine busy-deadlock, completion-engine
   pipe deadlock >64KB, concurrent history-file corruption (now
   lock-guarded + atomic), presigned-S3 signature redaction gap, MCP
   wire-prefix collisions (deterministic disambiguation registry),
   unbounded MCP frame reads (32 MiB cap), OMO blind-first fallback-chain
   resolution, non-atomic boulder.json writes, multibyte cursor-position
   slice panics in the completer/hinter, hook-stdin timeout escape.

## Mid-turn messaging: /steer, /queue, interrupt-with-message (2026-08-15)

Hermes parity for user input while a turn is running:

- **`Agent::steer`** (joey-agent-core, port of run_agent.py:2853-2886):
  Arc-shared pending-steer slot (`steer_handle`/`steer_via_handle` so hosts
  can steer from another task mid-borrow); concatenating; drained at TWO
  injection points (conversation_loop.py:933-975 pre-API and the post-tool-
  batch `apply_pending_steer_to_tool_results`), appended to the LAST tool
  result wrapped in the verbatim upstream `[OUT-OF-BAND USER MESSAGE]`
  markers; re-stashed when no tool message exists yet; DROPPED when a new
  user turn starts (interrupted turns never see stale steers).
- **STEER_CHANNEL_NOTE** (guidance.rs, verbatim) appended to the stable
  system prompt when tools are loaded — teaches the model to trust only
  the exact marker.
- **Engine/TUI semantics** (busy_input_mode=interrupt, the upstream
  default): plain Enter mid-turn = interrupt + the message runs as the
  next turn; `/steer` = EngineCommand::Steer → agent steer slot (no
  interrupt); `/queue` = queue for the next turn (never interrupts).
  Read-only slash commands (/status, /help, /copy, /model, /version) still
  answer inline while busy.
- The line REPL dispatches between turns, so its `/steer` degrades to a
  queued message with a hint (reedline input is blocking; concurrent input
  reading is not portable).

Tests: agent-core steer_tests (5: injection helper, restash, concat/empty,
  new-turn drop, marker format), engine steer-command routing, live TUI
  E2E (steer no-interrupt, plain-message interrupt+next-turn, queue).

## Expandable tool/terminal/diff blocks in the TUI (2026-08-15)

- Terminal-tool expanded view now shows the FULL result (tail-anchored
  200-line window + "… N earlier lines hidden" affordance) instead of just
  the one-line preview; generic tools keep args + full result.
- FileDiff items gained an `expanded` toggle (collapsed = last 50 lines,
  expanded = whole diff).
- Keyboard expansion: Space / x in transcript focus resolves the item at
  the viewport center via the mouse hit-test machinery (single source of
  truth for click/key parity), falling back to the first expandable
  visible item. Mouse clicks keep working via hit-testing.

## Natural-language /neurocode ingest (2026-08-15)

`/neurocode ingest` now accepts two forms: the strict
`<category> <path> [--version] [--provenance]` (unchanged, direct engine
call) and free text — anything whose first token isn't a category+path.
The NL form composes a workflow prompt (`ingest_agent_prompt`: teaches the
neurocode_ingest tool contract, file-location via read_file/search_files,
pasted-knowledge → write `.neurocode/sources/<slug>.md` then ingest with
provenance `user-provided`, honest failure over guessing) and runs it as a
full agent turn: REPL via run_turn_interactive, TUI via engine Submit
(strict form keeps the HeavyJob path). `neurocode_slash_outcome` returns
NeurocodeOutcome::{Text, AgentIngest}; the plain-text wrapper (engine
heavy jobs, tests) degrades to usage guidance. Tests: 5 routing + 2 tool
integration (registry registration + backend ingest roundtrip).
