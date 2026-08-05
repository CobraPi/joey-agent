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
  `--accept-hooks`, `--checkpoints`, `--tui/--cli/--dev`, `--no-restore-cwd`
  are not offered.
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
