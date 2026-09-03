# joey-agent-core — the turn loop, system prompt, and context compression

`joey-agent-core` is the agent runtime at the heart of Joey: it wires the
provider layer ([joey-providers.md](joey-providers.md)) to the tool system
([joey-tools.md](joey-tools.md)) and drives the conversation. It builds a
session-stable system prompt, calls the model, validates/repairs and
dispatches tool calls, retries or fails over on provider errors, compresses
context when the window fills, and loops until the assistant stops asking
for tools. It is a Rust port of upstream Hermes `run_agent.py` +
`agent/conversation_loop.py` + `agent/system_prompt.py` /
`agent/prompt_builder.py`, with guidance text ported verbatim.

> See also: [../agent-turn-loop.md](../agent-turn-loop.md), [../agent-core-reference.md](../agent-core-reference.md)

## Overview

The crate is 11,891 lines across 23 `.rs` files (measured; the single
largest is `src/agent.rs` at 6,901 lines). Its central design rule:
**`Agent::new` builds the system prompt ONCE per session** — this is
intentional, to keep provider prompt-prefix caches warm. The prompt is not
re-rendered per turn. It is rebuilt only in two places:

- `set_session_store` (wiring the SQLite session, honoring
  `pass_session_id`), and
- `rebuild_system_prompt` (explicit, e.g. after a `/model` switch via
  `switch_model`).

Everything else in the crate exists to keep that prompt stable while the
turn loop mutates only the message history.

## Module map

| File | Lines | Role |
|---|---|---|
| `src/agent.rs` | 6,901 | `Agent`, `AgentConfig`, `TurnResult`, `Transport`, the turn loop, retries/fallback, tool dispatch, RAG/NeuroCode wiring |
| `src/prompt.rs` | 1,238 | `PromptInputs`, `build_system_prompt`, SOUL.md / context-file loading, memory blocks, skills index |
| `src/verification.rs` | 916 | evidence ledger (standalone — not invoked by the turn loop) |
| `src/guardrails.rs` | 634 | `ToolGuardrailController` (standalone — not invoked by the turn loop) |
| `src/hooks.rs` | 598 | `PreToolUse` hooks: config loading, runner, exit-code contract |
| `src/events.rs` | 498 | `AgentEvent` enum + context-view types |
| `src/guidance.rs` | 306 | verbatim model-visible guidance constants |
| `src/loop_detection.rs` | 233 | crush-style `LoopDetector` (SHA-256 sliding window) |
| `src/threat_scan.rs` | 215 | context-file injection scan (NFKC, BOM, invisible unicode) |
| `src/image_model.rs` | 209 | feature 016 image-model resolution order |
| `src/lib.rs` | 32 | module decls + re-exports |
| `src/compression/mod.rs` | 138 | compression subsystem root + re-exports |
| `src/compression/compressor.rs` | 3,683 | `ContextCompressor` engine, summary prefix, thresholds, cooldowns |
| `src/compression/orchestrator.rs` | 742 | cross-session lock, `archive_and_compact` persistence, breakdown status |
| `src/compression/catalog.rs` | 462 | `DEFAULT_CONTEXT_LENGTHS` table, probe tiers, error parsers |
| `src/compression/estimator.rs` | 241 | rough token estimators (~4 chars/token) |
| `src/compression/anchors.rs` | 233 | summary anchors: real-user-message detection, compressed-transcript invariants |
| `src/compression/summary.rs` | 358 | `SummaryBackend`, aux-model resolution, failure classification |
| `src/compression/engine.rs` | 167 | memory-context sanitization (`MEMORY_CONTEXT_MAX_CHARS = 6_000`) |
| `src/compression/breakdown.rs` | 149 | context breakdown (what got compacted) |
| `src/compression/feedback.rs` | 90 | manual compression feedback |
| `src/compression/loop_tests.rs` | 646 | loop-level compression integration tests |
| `tests/rag_config_key_parity.rs` | 111 | cross-crate `neurocode.rag.*` key-parity test |
| `Cargo.toml` | — | crate manifest |

Dependency direction: `joey-agent-core` sits on `joey-core`, `joey-providers`,
and `joey-tools`; `joey-cli` and the orchestration crates sit on it.

## Public API

Construction and the turn:

- `Agent::new(config: AgentConfig, registry: ToolRegistry, ctx: ToolContext) -> Result<Self, ProviderError>` — builds the
  provider client, snapshots the valid tool set, parses the fallback chain,
  and builds the system prompt exactly once.
- `Agent::run_turn(&mut self, user_input, tx) -> TurnResult` — one user
  turn: zero or more model calls interleaved with tool batches, ending in a
  final text, an interrupt, or a fatal error. `tx` is an
  `mpsc::UnboundedSender<AgentEvent>`.

`AgentConfig` (all fields):

| Field | Type | Default (`from_config`) |
|---|---|---|
| `model` | `String` | `cfg.model()` |
| `provider` | `String` | `model.provider` = `"auto"` |
| `base_url` | `String` | `model.base_url` = `"https://openrouter.ai/api/v1"` |
| `api_key` | `Option<String>` | resolved key |
| `max_turns` | `usize` | `agent.max_turns` = `90` |
| `api_max_retries` | `usize` | `agent.api_max_retries` = `3` (TOTAL attempts per call block) |
| `tool_delay` | `f64` | `agent.tool_delay` = `1.0` (seconds between sequential tool calls) |
| `reasoning` | `Option<ReasoningEffort>` | resolved from config + model |
| `enabled_tools` | `Vec<String>` | `resolve_toolsets(cfg.toolsets)` |
| `max_tokens` | `Option<u32>` | `model.max_tokens` |
| `stream` | `bool` | `display.streaming` = `false` |
| `pass_session_id` | `bool` | `false` (`--pass-session-id` opts in) |
| `model_pinned` | `bool` | `false`; when the user chose the model explicitly, dynamic routing must not rewrite it |

`TurnResult` fields: `final_text: String`, `usage: Usage`,
`iterations: usize`, `interrupted: bool`, `fatal: bool`,
`fatal_provider_error: bool` — `fatal` distinguishes provider-fatal from
behavioral-fatal (3× invalid tool calls) so delegation consumers don't
mistake an empty fatal turn for success.

`Transport` trait (test seam; `ProviderClient` implements it):

```rust
pub trait Transport: Send + Sync {
    async fn complete(&self, req: &ProviderRequest) -> Result<NormalizedResponse, ProviderError>;
    async fn stream(&self, req: &ProviderRequest, tx: mpsc::UnboundedSender<StreamEvent>)
        -> Result<NormalizedResponse, ProviderError>;
}
```

`ContextCompressor::new` takes, in order: `model`, `threshold_percent`,
`protect_first_n`, `protect_last_n`, `summary_target_ratio`, `quiet_mode`,
`summary_model_override: Option<&str>`, `base_url`, `api_key`,
`config_context_length: Option<i64>`, `provider`, `api_mode`,
`abort_on_summary_failure`, `max_tokens`. `compress()` then rewrites a
message list in place (see Context compression below).

`PromptInputs<'a>`: `ctx: &ToolContext`, `model: &str`, `provider: &str`,
`enabled_tools: &[String]`, `pass_session_id: bool`,
`session_id: Option<&'a str>` — fed to the free function
`build_system_prompt(&PromptInputs) -> String`.

Other `Agent` methods (grouped): images (`attach_image`,
`pending_image_count`), NeuroCode/allocator (`set_neurocode_engine`,
`set_neurocode_engine_opt`, `neurocode_engine`, `set_model_allocator`,
`install_model_allocator`), compression access (`compressor`,
`compressor_mut`, `compression_enabled`), provider/client (`client`,
`switch_model`, `model`, `provider_name`, `effective_main_turn_model`,
`set_provider_semaphore`), history/session (`history`, `set_history`,
`set_session_store`, `session_db`), control (`interrupt_handle`, `steer`,
`steer_handle`, `steer_via_handle`), prompt (`system_prompt`,
`effective_system_prompt`, `rebuild_system_prompt`), tools
(`set_enabled_tools`, `enabled_tools`, `set_hooks`), loop detector
(`reset_loop_detector`), RAG (`rag_refresh_state`, `set_rag_refresh_worker`,
`set_rag_prefetch_source`, `rag_prefetch_context`), overlays
(`set_extra_instructions`, `extra_instructions`, `set_agent_identity`,
`agent_identity`). Free function `format_steer_marker(steer_text)` wraps a
mid-turn steer for appending to a tool result.

## The turn loop, step by step

1. **Per-turn reset.** The loop detector is reset; the model allocator (if
   wired) refreshes its allocation map and updates the compressor's context
   length to the allocated model's window.
2. **`TurnStart`.** An `AgentEvent::TurnStart { max_iterations }` is sent.
3. **Replay stored compression warning.** A warning persisted by a prior
   turn's compaction is emitted once (`compression_warning_replayed`).
4. **`repair_dangling_tool_tail`.** A crashed/interrupted prior turn can
   leave an unanswered assistant-with-tool_calls tail; it is repaired
   before the user message is appended.
5. **Background-process completions.** Completions queued by the reaper
   are drained and injected as `[Background process N completed: …]` user
   notices BEFORE the user message.
6. **User message + images.** The user input (with any pending images
   taken from the image queue) is appended; a `ContextSnapshot` baseline
   is emitted.
7. **The `while api_calls < max_turns` cycle.** Each iteration:
   - **Interrupt check** — if interrupted, close tool sequences, run
     RAG/NeuroCode auto-refresh, emit `Done`, return
     `TurnResult { interrupted: true, … }`.
   - **Pre-API steer drain** — a steer that arrived during the previous
     streaming call is injected before the next request.
   - **Pre-API pressure check** — rough-estimate request tokens; if the
     guard chain passes (compression enabled, history > 1,
     `compression_attempts < 3`, rough-estimate defer off, no failure
     cooldown, `should_compress` true), compact now, reset empty-response
     retry state, and `continue` without charging an iteration.
   - **`IterationStart` / `ApiCallStart`** events; the per-turn tool
     output budget resets.
   - **`call_with_retries`** — TOTAL attempts = `api_max_retries`
     (1 initial + n−1 retries). Retry-After is honored for rate limits,
     capped at 600 s (`RETRY_AFTER_CAP`); otherwise rate limits use
     jittered backoff (2/60 s) and other transient errors jittered
     backoff (5/120 s). A silent model-substitution guard warns when the
     served model family differs from the requested one.
   - **413 / context overflow** → `handle_context_overflow_error`: the
     compress-and-retry flow with a 3-attempt cap, the
     output-cap detour (parse available output tokens, set
     `ephemeral_max_output_tokens`, retry with a lower `max_tokens`,
     context length unchanged), the provider-limit context probe, and
     `update_model` if the provider reveals a smaller window. When
     retries are exhausted the `fallback_providers` chain activates
     (resetting retry and compression counters); if none remain, the
     error is `Fatal`.
   - **Tool calls** are executed whenever `tool_calls` is non-empty,
     REGARDLESS of `finish_reason`.
   - **Fuzzy repair** (`repair_tool_call`): VolcEngine XML-attribute
     leak trim (cut at first `"`/`'`/`<`/`>`), lowercase fast-path,
     separator normalization (`-`/space → `_`), camelCase → snake_case,
     `_tool`/`-tool`/`tool` suffix stripping applied twice, and finally a
     difflib-style fuzzy match with cutoff ≥ 0.7. Empty/whitespace args
     become `{}`.
   - **Invalid names**: a mixed batch (some valid) error-results the
     invalid calls and executes the rest; an all-invalid batch sends
     error tool-results for agent self-correction — 3 consecutive strikes
     is a behavioral fatal (`fatal_provider_error: false`).
   - **The assistant message is pushed BEFORE tools run** (and flushed
     before tool side effects), so every tool call has a matching result.
   - **PreToolUse hooks** (`hooks.rs`) can `allow`, `deny` (exit code 2 →
     tool error), `halt` the turn (exit code 49), or `rewrite` the tool
     arguments (JSON `updated_input` shallow-merge).
   - **Dispatch** — `plan_tool_segments` splits the batch into maximal
     contiguous runs of parallel-safe read-only tools (`read_file`,
     `search_files`, `session_search`, `skill_view`, `skills_list`,
     `web_extract`, `web_search`) which run concurrently with a 300 s
     per-tool timeout (`PARALLEL_TOOL_TIMEOUT_SECS`) and rayon
     post-processing (untrusted wrapping + previews); runs shorter than 2
     are demoted to sequential. Everything else runs sequentially with
     `tool_delay` spacing.
   - **Loop detection** — each result is recorded; a SHA-256 signature
     (tool name + args + result) repeating more than 5 times within a
     window of 10 triggers the nudge message.
   - **Untrusted wrapping** — results from `web_extract`/`web_search`
     (names) and `browser_`/`mcp_` (prefixes) of ≥ 32 chars are wrapped:
     `<untrusted_tool_result source="…">…</untrusted_tool_result>`.
   - **Post-round compression** — `should_compress` on the provider's
     REAL prompt count (with rough-estimate fallback) decides a
     compaction pass.
   - **`finish_reason = Length`** — up to 4 continuations: the partial
     content is accumulated and `LENGTH_CONTINUATION_PROMPT` is appended
     as a user message; after 4 the joined partial text is returned.
   - **Empty response** — post-tool nudge (`POST_TOOL_EMPTY_NUDGE`) once
     per tool round, then 3 plain retries, then fallback provider, then a
     synthetic `"(empty)"` assistant sentinel ends the turn.
   - **Budget exhausted** (`api_calls == max_turns`) — one summary call
     with the tool list stripped, using `MAX_ITERATIONS_SUMMARY_REQUEST`;
     one retry if empty; else the fallback sentence `"I reached the
     iteration limit and couldn't generate a summary."`.

## System prompt assembly

Built once per session from three tiers, each internally joined with
`\n\n`, then the non-empty tiers joined with `\n\n`.

**Stable tier** (exact order):

1. Identity — `~/.joey/SOUL.md` (threat-scanned, truncated) or
   `DEFAULT_AGENT_IDENTITY`.
2. `AGENT_HELP_GUIDANCE` — pointer to the docs + `joey-agent` skill.
3. `TASK_COMPLETION_GUIDANCE` — gated by
   `agent.task_completion_guidance` (default true) AND tools loaded.
4. `PARALLEL_TOOL_CALL_GUIDANCE` — gated by
   `agent.parallel_tool_call_guidance` (default true) AND tools loaded.
5. Tool-aware block, joined with a single space: `MEMORY_GUIDANCE` (if
   `memory` enabled), `SESSION_SEARCH_GUIDANCE` (`session_search`),
   `SKILLS_GUIDANCE` (`skill_manage`), `SUBAGENT_CONTROL_GUIDANCE`
   (`subagent_control`); then `STEER_CHANNEL_NOTE` whenever tools are
   loaded.
6. Model-family guidance — gated by `agent.tool_use_enforcement`
   (default: match `TOOL_USE_ENFORCEMENT_MODELS` =
   `["gpt","codex","gemini","gemma","grok","glm","qwen","deepseek"]`):
   `TOOL_USE_ENFORCEMENT_GUIDANCE`, plus
   `GOOGLE_MODEL_OPERATIONAL_GUIDANCE` for gemini/gemma models, or
   `OPENAI_MODEL_EXECUTION_GUIDANCE` for gpt/codex/grok models.
7. Skills index (`SKILLS_INDEX_PREAMBLE` + list + `SKILLS_INDEX_FOOTER`)
   when any skills tool is loaded.
8. Environment hints — untagged `Host:`/`User home directory:`/
   `Current working directory:` lines (+ WSL/Windows hints when they
   apply).
9. `CLI_PLATFORM_HINT` — the port's only surface is the CLI.

**Context tier**: `PROJECT_CONTEXT_HEADER` followed by the discovered
context files. Search order: `.joey.md`/`JOEY.md` (nearest, walked to the
git root) → `AGENTS.md` (cwd) → `CLAUDE.md` (cwd) → `.cursorrules` +
`.cursor/rules/*.mdc` (cwd). Every file is threat-scanned; oversized files
are truncated with a 20,000-char default cap, raised dynamically to
`ctx_len × 4 × 0.06` clamped to [20K, 500K] (explicit
`context_file_max_chars` overrides), keeping head 70% / tail 20% with a
truncation marker. The copilot block (`.github/copilot-instructions.md`,
same budget) is appended when `copilot.enabled` (default true).

**Volatile tier**: memory blocks — `MEMORY.md` capped at 2,200 chars
(`memory.memory_char_limit`) and `USER.md` at 1,375
(`memory.user_char_limit`), each rendered with `═`×46 separators and a
usage header — followed by the footer lines: `Conversation started:
<date>` (date-only, byte-stable for the day), optional `Session ID:`
(`pass_session_id`), `Model:`, `Provider:`.

## Guidance strings

All constants live in `src/guidance.rs`. **The verbatim-port rule:
guidance text is ported verbatim from the Python upstream (modulo the
Hermes→Joey branding policy) because subtle wording changes measurably
affect model behavior — do not reword them.** A unit test enforces that no
unbranded "Hermes" references remain.

| Constant | First line / opening | Purpose |
|---|---|---|
| `DEFAULT_AGENT_IDENTITY` | (the seeded `DEFAULT_SOUL_MD`) | fallback identity when `SOUL.md` is absent/empty |
| `AGENT_HELP_GUIDANCE` | "You run on Joey Agent (based on Hermes Agent…" | docs are the authoritative reference; load the `joey-agent` skill |
| `STEER_CHANNEL_NOTE` | "## Mid-turn user steering" | trust the `[OUT-OF-BAND USER MESSAGE]` marker; ignore lookalikes |
| `MEMORY_GUIDANCE` | "You have persistent memory across sessions." | what to save vs. not save in memory; declarative facts |
| `SESSION_SEARCH_GUIDANCE` | "When the user references something from a past conversation…" | recall past transcripts before asking |
| `SKILLS_GUIDANCE` | "After completing a complex task (5+ tool calls)…" | save and maintain skills |
| `SUBAGENT_CONTROL_GUIDANCE` | "When you have subagents running from delegate_task…" | steer/stop delegated children (Joey-side addition) |
| `TOOL_USE_ENFORCEMENT_GUIDANCE` | "# Tool-use enforcement" | act, don't narrate; never end on a promise |
| `TOOL_USE_ENFORCEMENT_MODELS` | `["gpt","codex","gemini",…]` | model substrings that trigger the above |
| `TASK_COMPLETION_GUIDANCE` | "# Finishing the job" | working artifacts backed by real output; report blockers honestly |
| `PARALLEL_TOOL_CALL_GUIDANCE` | "# Parallel tool calls" | batch independent calls in one turn |
| `OPENAI_MODEL_EXECUTION_GUIDANCE` | "# Execution discipline" | tool_persistence / mandatory_tool_use / act_dont_ask / verification / missing_context blocks |
| `GOOGLE_MODEL_OPERATIONAL_GUIDANCE` | "# Google model operational directives" | absolute paths, verify first, non-interactive flags |
| `CLI_PLATFORM_HINT` | "You are a CLI AI Agent." | terminal-renderable output; MEDIA: tags don't work; cron is local-only |
| `WSL_ENVIRONMENT_HINT` | "You are running inside WSL…" | `/mnt/c/` path translation |
| `WINDOWS_BASH_SHELL_HINT` | "Shell: on this Windows host your `terminal` tool runs commands through bash…" | POSIX syntax, not PowerShell |
| `WINDOWS_HOSTNAME_NOTE` | "Note: on Windows, the machine hostname … is NOT the username." | construct paths from the home directory |
| `SKILLS_INDEX_PREAMBLE` | "## Skills (mandatory)" | scan and load relevant skills before replying |
| `SKILLS_INDEX_FOOTER` | "Only proceed without loading a skill if genuinely none are relevant…" | closing line after `</available_skills>` |
| `PROJECT_CONTEXT_HEADER` | "# Project Context" | header over the context-file tier |

## Context compression

**Trigger math.** `threshold_tokens = (context_length − max_tokens) ×
threshold_percent` (default `compression.threshold` = `0.50`), floored at
`MINIMUM_CONTEXT_LENGTH = 64_000`; if `max_tokens` leaves no window, the
full `context_length` is used. When the floor meets/exceeds the effective
window the threshold becomes `0.85 × window` (`MIN_CTX_TRIGGER_RATIO`),
capped at `window − 1`. Small windows (`< SMALL_CTX_WINDOW_LIMIT =
512_000`) raise the effective percent to at least
`SMALL_CTX_THRESHOLD_PERCENT = 0.75` (raise-only).

**Gates.** `should_compress` fires only when tokens ≥ threshold AND
anti-thrash guards pass: `ineffective_compression_count >= 2` or
`fallback_compression_streak >= 2` blocks. A summary failure records a
cooldown (`SUMMARY_FAILURE_COOLDOWN_SECONDS = 600.0`; the transient ladder
is 60 → 300 → 900 s for timeouts, 30 s for JSON/stream-closed, 60 s
otherwise) that also blocks. Per-turn, at most 3 compression attempts.

**Algorithm** (`ContextCompressor::compress`):

1. Prune old tool results (cheap, no LLM call), respecting
   `protect_last_n` (default 20) and the tail budget.
2. Determine boundaries: protect the head — system prompt at index 0 plus
   `protect_first_n` (default 3), which decays to 0 after the first
   compaction — and align boundaries to avoid splitting tool-call groups.
3. Protect the tail by token budget: `tail_token_budget = threshold_tokens
   × summary_target_ratio` (default `compression.target_ratio` = `0.20`,
   clamped to 0.10–0.80).
4. Summarize the middle with a structured LLM prompt whose template
   headings include `## Historical Task Snapshot`, `## Goal`,
   `## Constraints & Preferences`, `## Completed Actions`,
   `## Active State`, `## Historical In-Progress State`, `## Blocked`,
   `## Key Decisions`, `## Resolved Questions`,
   `## Historical Pending User Asks`, `## Relevant Files`,
   `## Historical Remaining Work`, `## Last Dropped Turns`, and
   `## Critical Context`; on later compactions the previous summary is
   iteratively updated rather than regenerated. Summaries are prefixed
   with `SUMMARY_PREFIX` ("[CONTEXT COMPACTION — REFERENCE ONLY] …").
   When the summarizer is unavailable, a deterministic local fallback
   (secrets redacted) is produced instead.

The aux summary backend (`AuxSummaryBackend`) resolves its model from
`auxiliary.compression.*` ("auto" = the main runtime) with an effective
timeout of `max(configured (default 30 s), 300 s floor)`. Failure classes
(`classify_summary_failure`): `ModelNotFound` (404/503/no-channel →
immediate main-model fallback, no cooldown), `Timeout` (408/429/502/504 →
escalating cooldown ladder), `JsonDecode` (30 s cooldown),
`StreamClosed` (30 s + abort so the session is preserved),
`AccessOrQuota` (abort, session preserved), other (60 s).

Persistence: the orchestrator takes a cross-session compression lock
(TTL `COMPRESSION_LOCK_TTL_SECONDS = 300.0`) and rewrites the transcript
in place under the SAME session id via `SessionDb::archive_and_compact`.
Unknown models resolve their context window from `DEFAULT_CONTEXT_LENGTHS`
(`compression/catalog.rs`, ported from `model_metadata.py`), falling back
to `DEFAULT_FALLBACK_CONTEXT = 256_000`; context probe tiers are
`[256_000, 128_000, 64_000, 32_000, 16_000, 8_000]`.

## Loop detection, hooks, threat scan, events

- **Loop detection** (`loop_detection.rs`): crush-style `LoopDetector` —
  SHA-256 signatures of (tool name, arguments, result) in a sliding window
  of 10; more than 5 repeats of the latest signature returns true and the
  nudge message is injected. Same tool with different args or different
  results does NOT count.
- **Hooks** (`hooks.rs`): `load_hooks_from_config` reads the `hooks` key
  from `~/.joey/config.yaml` (name/event/matcher regex/command).
  `PreToolUseRunner` aggregates: exit 0 allow, exit `2` deny (tool
  error), exit `49` halt the turn; stdout JSON `updated_input` rewrites
  arguments (last-write-wins).
- **Threat scan** (`threat_scan.rs`): context-file injection scan over
  `CONTEXT_PATTERNS` (exfil via curl/wget, secret reads, `unset
  CLAUDE|CODEX|JOEY|HERMES|AGENT|OPENAI|ANTHROPIC` env patterns, …),
  capped at `MAX_SCAN_CHARS = 65_536`. Invisible unicode is flagged on
  the RAW content (before NFKC folding), one finding per codepoint; the
  content is then NFKC-normalized so full-width homoglyphs can't evade
  the regexes. A single leading UTF-8 BOM is silently stripped
  (Windows-editor artifact); on findings, the file is replaced with a
  `[BLOCKED: …]` placeholder.
- **Events** (`events.rs`): `AgentEvent` covers streaming deltas
  (`ContentDelta`, `ReasoningDelta`), turn lifecycle (`TurnStart`,
  `IterationStart`, `ApiCallStart`, `ApiCallEnd`), tool execution
  (`ToolStart`, `ToolProgress`, `ToolOutput`, `ToolEnd`), the live
  context view (`ContextSnapshot`), `TerminalQueueState`, `FileChange`,
  `AssistantMessage`, `Notice`, `RetryAttempt`, `CompressionStart`/
  `CompressionEnd`, `FallbackActivated`, the subagent/delegation family,
  `AgentModeChanged`, and the NeuroCode family (`NeuroCodeContext`,
  `NeuroCodeProgress`, `NeuroCodeGraph`, `NeuroCodeActive`,
  `NeuroCodeReindexed`).
- **Image model** (`image_model.rs`, feature 016): pure resolution order —
  `providers.<id>.image_model` → `model.image_model` → provider default
  multimodal → primary model if `looks_vision_capable` → `Unavailable`
  with an actionable message.

## Standalone modules note

`guardrails.rs` (`ToolGuardrailController`, with
`FILE_MUTATING_TOOLS`/`NO_EFFECT_TOOLS`/`IDEMPOTENT_TOOLS`/
`MUTATING_TOOLS` classification and before/after-call decisions) and
`verification.rs` (the evidence ledger:
`VerificationKind`/`VerificationStatus`/`VerificationEvent`/
`EvidenceStatus`) are exported library modules but are **NOT invoked by
the turn loop** — they exist for embedding consumers. Don't wire them into
`run_turn` expecting upstream parity.

## NeuroCode & RAG wiring

`rag_keys` consts pin the dotted config paths this crate reads (they are
public precisely so `tests/rag_config_key_parity.rs` can pin them against
the canonical `RAG_CONFIG_KEYS` table in `joey-neurocode-rag`):
`neurocode.rag.enabled` (default false), `neurocode.rag.prefetch.enabled`
(false), `neurocode.rag.backend` (`"auto"`), `neurocode.rag.base_url`
(loopback default `http://localhost:11434`), `neurocode.rag.model`
(`nomic-embed-text-v1.5`), `neurocode.rag.local.model_dir` (empty =
profile-scoped default).

`RagRefreshPhase` is `Idle` or `Refreshing { files_done, files_total }`.
`RagRefreshWorker::run_refresh` is blocking, spawned fire-and-forget (one
refresh in flight); `RagPrefetchSource::prefetch_block(user_prompt)`
supplies per-turn pre-fetch context, and is called ONLY when the gate is
armed: `enabled` + `prefetch.enabled` + a hard-verified-local backend
(`local_onnx`; `auto` with `model.onnx` + `tokenizer.json` artifacts
present; or an HTTP backend with a loopback `base_url`). With the gate
closed, behavior is byte-identical to pre-feature. A `NeuroCodeEngine`
(`set_neurocode_engine`) can additionally prepend one-shot context per
request.

## Configuration

| Key | Default | Effect |
|---|---|---|
| `agent.max_turns` | `90` | tool-calling iteration budget |
| `agent.api_max_retries` | `3` | TOTAL provider attempts per call block |
| `agent.tool_delay` | `1.0` | seconds between sequential tool calls |
| `agent.task_completion_guidance` | `true` | gate for `TASK_COMPLETION_GUIDANCE` |
| `agent.parallel_tool_call_guidance` | `true` | gate for `PARALLEL_TOOL_CALL_GUIDANCE` |
| `agent.tool_use_enforcement` | model-list match | `true`/`false`/list or model substrings |
| `display.streaming` | `false` | stream responses |
| `model.provider` / `model.base_url` | `"auto"` / OpenRouter | provider selection |
| `model.max_tokens` / `model.context_length` | unset / `0` | output cap; explicit window override |
| `model.image_model`, `providers.<id>.image_model` | unset | image-model routing (feature 016) |
| `toolsets` | — | enabled tool groups |
| `fallback_providers` (or `model.fallback_providers`) | `[]` | provider/model/base_url/api_key chain |
| `compression.enabled` | `true` | auto-compaction master switch |
| `compression.threshold` | `0.50` | trigger percent |
| `compression.target_ratio` | `0.20` | summary target (clamped 0.10–0.80) |
| `compression.protect_first_n` / `protect_last_n` | `3` / `20` | protected head/tail messages |
| `compression.abort_on_summary_failure` | `false` | abort vs. deterministic fallback |
| `auxiliary.compression.provider` / `.model` / `.base_url` / `.api_key` | `"auto"` / `""` / `""` / `""` | aux summary backend |
| `auxiliary.compression.timeout` | `30` (floored at 300) | summary call timeout |
| `auxiliary.compression.context_length` | unset | aux model window |
| `context_file_max_chars` | dynamic | explicit context-file cap override |
| `copilot.enabled` | `true` | copilot context block + skills |
| `memory.memory_enabled` / `memory.user_profile_enabled` | `true` / `true` | memory blocks in prompt |
| `memory.memory_char_limit` / `memory.user_char_limit` | `2200` / `1375` | memory block caps |
| `hooks` | — | PreToolUse hook list |
| `neurocode.rag.*` | see above | RAG gating |

## Testing

- `agent.rs` inline tests: tool-name repair rules, retries/fallback
  budget, 413/overflow recovery, untrusted wrapping, max-turns summary
  stripping, steer injection, invalid-tool strikes.
- `compression/loop_tests.rs`: end-to-end compaction through the real
  turn loop with a scripted summary backend.
- `compressor.rs` unit tests: threshold math, floors, cooldown
  record/expiry, gating, anchors.
- `prompt.rs` tests: dynamic cap bounds, truncation marker, frontmatter
  stripping, memory rendering.
- `threat_scan.rs` tests: invisible unicode + BOM tolerance.
- `hooks.rs` tests: allow/deny/halt/rewrite aggregation.
- `guidance.rs` tests: no unbranded Hermes leakage; identity matches the
  seeded soul.
- `tests/rag_config_key_parity.rs`: cross-crate pin of every
  `neurocode.rag.*` key and default against `RAG_CONFIG_KEYS`.

## See also

- [../agent-turn-loop.md](../agent-turn-loop.md) — narrative walkthrough
  of the turn loop.
- [../agent-core-reference.md](../agent-core-reference.md) — deeper
  API reference.
- [../architecture.md](../architecture.md) — how the crates fit together.
- [../security.md](../security.md) — the sanitization/threat-scan model.
- [../state-and-config.md](../state-and-config.md) — config layers and
  session persistence.
- Neighbor crates: [joey-providers.md](joey-providers.md),
  [joey-tools.md](joey-tools.md), [joey-core.md](joey-core.md),
  [joey-cli.md](joey-cli.md), [joey-orchestration.md](joey-orchestration.md),
  [joey-omo.md](joey-omo.md), [joey-neurocode.md](joey-neurocode.md),
  [joey-neurocode-rag.md](joey-neurocode-rag.md).
