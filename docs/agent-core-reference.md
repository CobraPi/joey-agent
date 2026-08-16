# joey-agent-core Reference: Turn Loop, System Prompt, Compression, Hooks, Events

Source of truth: `crates/joey-agent-core/src/` (`agent.rs`, `prompt.rs`,
`guidance.rs`, `events.rs`, `hooks.rs`, `compression/`). This page is the
comprehensive crate reference; it complements (and deliberately does not
duplicate) two existing focused docs:

- [`agent-turn-loop.md`](agent-turn-loop.md) — already covers `run_turn`
  mechanics in depth: the iteration-loop diagram, retry/backoff decision
  order, context-overflow recovery shapes, tool-call validation/repair,
  parallel-safe batching, untrusted-result wrapping, interrupts, and
  session persistence. Read that first for the turn loop narrative.
- [`HOOKS.md`](HOOKS.md) — already covers the user-facing hooks
  configuration, JSON stdin contract, exit codes (0/2/49), matcher syntax,
  and input rewriting.

What this page adds: system-prompt assembly stages (not documented
anywhere yet), compression triggers/thresholds/what-is-kept, the full
`AgentEvent` catalog, hooks integration points *inside the loop*, the
tunables table, and subagent/delegation wiring.

## 1. Turn loop lifecycle (summary — see agent-turn-loop.md for detail)

`Agent::run_turn(user_input, tx) -> TurnResult { final_text, usage,
iterations, interrupted }`:

1. Per-turn resets: interrupt flag cleared, loop detector reset,
   `invalid_tool_strikes = 0`, model-allocator cache refreshed.
2. Emits `TurnStart`; replays a stored compression warning once.
3. `repair_dangling_tool_tail()` — drops unanswered
   assistant-with-tool_calls tails left by a crashed prior turn.
4. Drains background-process completions (injected as user-role notices
   before the user message), then pushes + persists the user message.
5. Loop while `api_calls < max_turns`:
   - Interrupt check → close tool sequence, `Done { interrupted: true }`.
   - Pre-API pressure check → maybe pre-emptive compression (see §3);
     a compaction pass is not charged an iteration.
   - `IterationStart` + `ApiCallStart`; per-turn tool output budget reset.
   - `call_with_retries` (see below).
   - Real usage fed to the compressor (`update_from_response`).
   - If tool_calls present: fuzzy name repair, arg normalization,
     all-valid/mixed/all-invalid classification (3-strike abort),
     persist assistant tool-call message BEFORE side effects, then
     `execute_tool_calls`; post-tool-round compression check; continue.
   - `finish_reason == Length` → continuation prompt, up to 4 attempts.
   - Empty content → post-tool nudge (once/tool round), then 3 retries,
     then fallback provider, else honest "(empty)" failure.
   - Otherwise: persist final assistant message → `AssistantMessage` →
     `Done`.
6. Budget exhausted → one tools-stripped summary call (+1 retry), else a
   canned "iteration limit" summary.

`call_with_retries` = one call block: `api_max_retries` TOTAL attempts
(1 initial + n−1 retries); interrupt beats everything; overflow-shaped
errors route to `handle_context_overflow_error` (≤3 compression attempts
per turn); Retry-After honored capped at 600s, else jittered backoff;
non-retryable/failover-class errors and retry exhaustion walk the
`fallback_providers` chain (skipping entries matching the current
provider+model; activation resets retry and compression budgets, patches
the prompt's Model/Provider lines in place, recalibrates the compressor).

Tool dispatch (`execute_tool_calls`): contiguous parallel-safe runs
(read_file, search_files, session_search, skill_view, skills_list,
web_extract, web_search) execute concurrently, results appended in call
order; everything else sequential with interruptible `tool_delay` sleep.
Interrupts mid-batch error-result skipped calls (protocol validity).
Untrusted tools (web_extract, web_search, `browser_*`, `mcp_*`) get
delimiter-wrapped results (min 32 chars). Sequential results also feed a
crush-style loop detector → nudge tool-result on repetition.

## 1b. Mid-turn steering (`Agent::steer`, upstream `_pending_steer`)

`/steer <message>` injects a user message into the RUNNING turn without
interrupting (ported from run_agent.py:2853-2886 + conversation_loop.py:
933-975):

- `Agent::steer(text)` — stashes into an **Arc-shared**
  `pending_steer: Arc<Mutex<String>>` slot (`steer_handle()` /
  `steer_via_handle()` let the engine task steer while the turn future
  holds the mutable agent borrow). Multiple steers concatenate with
  `\n`; empty/whitespace text is rejected.
- **Two injection points** drain the slot and append the text — wrapped
  in the verbatim upstream markers
  `[OUT-OF-BAND USER MESSAGE — a direct message from the user, delivered
  mid-turn; not tool output]` … `[/OUT-OF-BAND USER MESSAGE]` — to the
  LAST tool-role message in history:
  1. **Post-tool-batch** (after `execute_tool_calls` completes without
     interruption) — the model sees the steer on its next iteration.
  2. **Pre-API** (top of each loop iteration) — catches steers that
     arrived while the previous API call was streaming, so they aren't
     lost when the model returns a final answer with no further tool
     batch.
- When no tool message exists yet (first iteration), the text is
  re-stashed — injecting into a user message would break role
  alternation.
- A **new user turn drops** pending steers (they were meant for the
  aborted turn's tool loop).
- The stable system prompt carries `STEER_CHANNEL_NOTE` (verbatim
  upstream) whenever tools are loaded — it teaches the model to treat
  text inside the exact marker as a genuine user instruction and to
  IGNORE lookalike markers embedded in tool output, web pages, or files
  (injection defense).
- `pub const STEER_MARKER_OPEN/CLOSE` + `pub fn format_steer_marker` are
  the wire format; host semantics (which key steers vs interrupts) are
  documented in [tui.md](tui.md#mid-turn-messaging-hermes-parity).

## 2. System prompt assembly (`prompt.rs`, `guidance.rs`)

`build_system_prompt(&PromptInputs)` is called **once per session** in
`Agent::new` (and re-rendered only in `set_session_store` when
`pass_session_id` is set, to splice the `Session ID:` line). It is never
rebuilt per turn — that is the constraint keeping provider prompt-prefix
caches warm. Fallback/model-switch patches only the last `Model:` /
`Provider:` lines in place. Request-time overlays stack on top via
`effective_system_prompt()`: base prompt → OMO agent identity →
extra-instructions (ultrawork) → NeuroCode context graph.

Three tiers joined with `\n\n`:

Stable tier (session-stable, ordered):
1. Identity — `~/.joey/SOUL.md` (threat-scanned, truncated) else
   `DEFAULT_AGENT_IDENTITY`.
2. `AGENT_HELP_GUIDANCE` (Joey/Hermes self-description + docs pointer).
3. `TASK_COMPLETION_GUIDANCE` — config `agent.task_completion_guidance`
   (default true), tools loaded.
4. `PARALLEL_TOOL_CALL_GUIDANCE` — `agent.parallel_tool_call_guidance`
   (default true), tools loaded.
5. Tool-aware guidance, space-joined: `MEMORY_GUIDANCE` (memory tool),
   `SESSION_SEARCH_GUIDANCE`, `SKILLS_GUIDANCE` (skill_manage) — all
   verbatim upstream ports.
6. Model-family guidance, gated by `agent.tool_use_enforcement`
   (bool/string/list/default model-patterns): tool-use enforcement +
   Google (gemini/gemma) or OpenAI-style (gpt/codex/grok) blocks.
7. Skills index (when any skills_* tool is enabled).
8. Environment hints (OS, shell, cwd, git, platform — untagged lines).
9. `CLI_PLATFORM_HINT`.

Context tier: project context files (`.joey.md` / AGENTS.md / CLAUDE.md /
.cursorrules) discovered under the agent cwd. Each file is
threat-scanned and truncated head/tail (70%/20% + omission marker) at
`context_file_max_chars`: explicit config → dynamic
(window × 4 chars/token × 6%, clamped 20K–500K) → flat 20K.

Volatile tier: memory snapshot block (MEMORY.md, char limit
`memory.memory_char_limit` default 2200) and USER.md profile
(`memory.user_char_limit` default 1375), then final lines —
"Conversation started: <date>" (date-only, byte-stable per day),
optional "Session ID:", "Model:", "Provider:".

## 3. Context compression (`compression/`)

Config keys (defaults): `compression.enabled` true, `threshold` 0.50,
`target_ratio` 0.20 (clamped 0.10–0.80), `protect_last_n` 20,
`protect_first_n` 3, `abort_on_summary_failure` false,
`model.context_length` (catalog-resolved per model).

Trigger: `should_compress(tokens)` = tokens ≥ threshold_tokens AND
not blocked (cooldowns/breakers). `threshold_tokens` =
(context_length − max_tokens) × threshold_percent, floored at the
catalog `MINIMUM_CONTEXT_LENGTH`; if the floor meets/exceeds the window,
trigger at 85%. Windows < 512K tokens get a raise-only threshold floor
of 75%. Compression fires from three sites in the loop:

1. Pre-API pressure check (rough estimate of history + system prompt +
   tool schemas; guard chain: defer-on-noisy-estimate, failure cooldown,
   then should_compress; ≤3 per turn, not charged an iteration).
2. Post-tool-round check on the provider's REAL prompt-token count
   (sentinel −1 = just compacted → 0; stale 0 → rough estimate).
3. Overflow recovery (`handle_context_overflow_error`) on 413 /
   context-length errors — includes the output-cap detour
   (`ephemeral_max_output_tokens`) which never touches context_length.

What happens (`orchestrator.rs::compress_context`): honors cooldowns and
durable breaker state (auto path only; manual `/compress` uses
`force=true`), one-time feasibility probe of the summary model, acquires
an atomic per-session compression lock in state.db (with lease refresher
and rotation detection), then replaces the compressible middle with an
LLM-generated summary. Protected regions: the first `protect_first_n`
messages (head) and the last `protect_last_n` messages (tail; floor 3,
hard cap 8 on the recent-message floor; most recent user and assistant
messages guaranteed into the tail). Summaries follow a structured
## Goal / ## Constraints & Preferences / ## Completed Actions /
## Active State / ## Blocked / ## Key Decisions / ## Resolved Questions
/ pending / ## Relevant Files / ## Last Dropped Turns / ## Critical
Context format; a deterministic redacted fallback summary is used when
the aux summary model is unavailable/fails. Summaries are generated by a
`SummaryBackend` (default `AuxSummaryBackend` from config); summarizer
input truncates message content at 6000 chars (4000 head + 1500 tail)
and tool args at 1500 chars. With a session store attached, compaction
soft-archives pre-compaction rows and inserts the compacted transcript
under the same session id (`archive_and_compact`).

## 4. AgentEvent variants (`events.rs`)

Streaming: `ContentDelta`, `ReasoningDelta`.
Lifecycle: `TurnStart`, `IterationStart`, `ApiCallStart`, `ApiCallEnd`.
Tools: `ToolStart`, `ToolProgress`, `ToolOutput` (name, chunk — raw
live-streamed output from streaming tools such as `terminal`, throttled to
the same 50ms window as ToolProgress and bracketed by the same
ToolStart/ToolEnd; UIs accumulate it per tool call for a realtime view;
`ToolEnd.full_result` remains the definitive complete output), `ToolEnd`
(is_error, preview, duration, exit_code, full_result), `FileChange` (path,
kind Create/Edit/Delete, before/after, unified `DiffResult`, is_binary,
source FileTool/Terminal/Detected — ordering: ToolStart → FileChange* →
ToolEnd; produced only by the tool layer).
Context: `ContextSnapshot` (entries: role/tokens/preview/has_tool_calls/
is_compressed_summary per history message, system_tokens, history_tokens,
context_window, compression_threshold, compactions, model) — emitted at
every history mutation the turn loop makes (user turn, tool rounds,
compactions, final message); additive observational event backing the
TUI's live agent-stats page; never alters the request path.
Messages: `AssistantMessage` (interim messages deduped against the
previous interim).
Status: `Notice`, `RetryAttempt`, `CompressionStart`, `CompressionEnd`,
`FallbackActivated`.
Orchestration: `SubagentSpawn`, `SubagentComplete`, `SubagentFailed`,
`DelegationBatchComplete`.
OMO: `AgentModeChanged`, `CategoryDelegation`, `BoulderWorkStarted` /
`Resumed` / `Completed`, `GoalSet`, `GoalCleared`, `WisdomAccumulated`.
NeuroCode (feature 015): `NeuroCodeContext { tier, token_estimate,
expanded_nodes, cold_mode, formatted_context }` — emitted before each
model call when the engine is active, carrying the exact context string
prepended to the system prompt (drives the TUI's live feed panel);
`NeuroCodeActive { active }` — engine wired/unwired state changes.
Turn end — exactly one of: `Done { final_text, usage, iterations }`,
`Failed(String)`.

## 5. Hooks integration points (`hooks.rs`)

Only `PreToolUse` exists. The CLI loads hooks from config
(`load_hooks_from_config`) and installs them via
`Agent::set_hooks(Option<PreToolUseRunner>)`. Inside
`execute_tool_calls`, before any dispatch, hooks run once per tool call
in the batch (stdin JSON: event, tool_name, tool_input, session_id,
cwd; 10s default timeout):

- Halt (exit 49) — every remaining call in the batch gets an error
  result, the turn ends interrupted-style, a `🛑 Turn halted by hook`
  Notice fires.
- Deny (exit 2) — that call gets a `[Tool call blocked by PreToolUse
  hook: …]` error result AND IS NOT EXECUTED — denied calls are filtered
  out of the dispatch batch entirely (regression-tested; historically the
  tool ran anyway). Valid calls still execute. All-denied is not an
  interrupt; the model sees the errors and adjusts.
- Rewrite (`updated_input` stdout) — shallow-merged into that call's
  arguments before dispatch.
- The stdin write is INSIDE the timeout wrapper: a hook that never reads
  a >64KB tool_input cannot outlive its configured timeout
  (`kill_on_drop` reaps the child).

Loop-detection nudges are delivered as user-role messages (NOT synthetic
tool results — a tool_call_id no assistant message declares is rejected
by strict providers like Anthropic with a 400, bricking the turn).

See [`HOOKS.md`](HOOKS.md) for the user-facing contract.

## 6. Tunables (AgentConfig::from_config)

| Key | Default | Meaning |
|---|---|---|
| `agent.max_turns` | 90 | tool-calling iterations per turn |
| `agent.api_max_retries` | 3 | TOTAL provider attempts per call block |
| `agent.tool_delay` | 1.0s | sleep between sequential tool calls |
| `display.streaming` | false | SSE streaming |
| `model.max_tokens` | none | output cap (ephemeral override wins) |
| `fallback_providers` | none | ordered failover chain |
| `model_pinned` | false | set when the model was chosen explicitly; |
| | | blocks NeuroCode tier rewrites (see providers.md) |
| `compression.*` | see §3 | compaction behavior |
| `model.context_length` | catalog | window override |

Others surfaced in prompt assembly: `agent.task_completion_guidance`,
`agent.parallel_tool_call_guidance`, `agent.tool_use_enforcement`,
`context_file_max_chars`, `memory.memory_char_limit`,
`memory.user_char_limit`, `memory.memory_enabled`,
`memory.user_profile_enabled`.

## 7. Subagent / delegation integration

`joey-agent-core` itself stays loop-only but exposes two seams consumed
by `joey-orchestration` / `joey-omo`: (a) `set_provider_semaphore` — a
shared `tokio::Semaphore` permit acquired around every provider call,
throttling concurrent subagent dispatch; (b) the orchestration event
variants (§4) plus `Agent`'s optional OMO overlays
(`set_agent_identity`, `set_extra_instructions`). Subagents
(`joey-orchestration::subagent`) drive their own `Agent::run_turn` and
emit `SubagentSpawn` / `SubagentComplete` / `SubagentFailed` /
`DelegationBatchComplete` through the same event channel; a
`delegation_tool` exposes it back to the model. Additional optional
intercepts (inactive by default, byte-identical when off): a dynamic
model allocator (`set_model_allocator` / `install_model_allocator`,
feature 011) and the NeuroCode engine (`set_neurocode_engine`,
feature 015).
