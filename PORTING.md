# Porting status: Hermes Agent → Joey Agent (Rust)

This document tracks what has been ported from the ~690K-line Python
`hermes-agent` into the Rust `joey-agent`, and what remains. The goal is a
faithful clone: same architecture, defaults, data formats, prompt text, and
wire behavior, rebranded `hermes → joey`.

**Fidelity review (2026-07-21):** every crate was audited line-by-line against
upstream (commit `7651764`, 2026-07-20) and rewritten where it deviated. The
ported surface below is now behavior-, format-, and text-faithful (modulo
branding); tests assert exact schemas, envelopes, grammars, and prompt text
(457 tests across the workspace). Anything not faithful is listed under
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

**Provider layer (`joey-providers`)** — port of `providers/`, `agent/transports/`,
`agent/anthropic_adapter.py`, `agent/error_classifier.py`:
- Profile registry matching upstream composition (openrouter/openai-api/
  anthropic/nous/deepseek/zai/gemini/xai/custom) with aliases, env-var chains,
  aux models; per-provider base-URL overrides (`<ID>_BASE_URL`).
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
- Subcommands matching upstream semantics: `model` (interactive picker),
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

**Verified end-to-end:** `cargo test --workspace` — 31 suites, 457 tests, 0
failures, 0 warnings. Live-verified command surfaces: one-shot exit codes,
config round-trips incl. secret masking, cron create/pause/resume/run/remove/
status, mcp add/list/test incl. security rejection, first-run guard, slash
prefix resolution, resume by id/title.

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

## Partial

- **Providers:** OpenAI-compatible + Anthropic wire modes only. Codex/
  Responses, Bedrock, native Gemini REST, Vertex, and Azure are not ported
  (`ApiMode::CodexResponses` exists but refuses). No credential pools, model
  catalog, request_overrides/service-tier plumbing, Z.AI adaptive long
  backoff, or per-provider timeout table.
- **Context compression** is not implemented: 413/context-overflow errors
  abort with a classified error instead of compress-and-retry; there is no
  auto-compaction. Thinking-only prefill continuation is likewise unported.
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
