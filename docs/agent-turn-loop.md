# The Agent Turn Loop

Source: `crates/joey-agent-core/src/agent.rs` (`Agent` struct + `Agent::run_turn`).
This is a faithful port of `run_agent.py` + `agent/conversation_loop.py` +
`agent/tool_executor.py`.

## What is a "turn"?

A **turn** starts when a user (or a scheduled cron job) sends one message,
and ends when the assistant produces a final answer with no further tool
calls (or the turn is aborted/interrupted). Internally a turn is a bounded
loop over "iterations" — each iteration is one call to the model, optionally
followed by a batch of tool executions.

```
run_turn(user_input)
 └─ push user message, repair any dangling tool tail from a prior crash
 └─ loop (up to config.max_turns iterations):
     ├─ pre-API pressure check → maybe pre-emptively compress context
     ├─ call_with_retries(with_tools=true)         [model call + retries + fallback]
     │    └─ on 413 / context-length error → handle_context_overflow_error
     ├─ if assistant returned tool_calls:
     │    ├─ repair hallucinated tool names (fuzzy match against valid_tool_names)
     │    ├─ normalize empty/whitespace arguments to "{}"
     │    ├─ classify: all-valid / mixed / all-invalid (3-strike abort)
     │    ├─ persist the assistant tool-call message BEFORE side effects
     │    └─ execute_tool_calls(...)                 [parallel-safe batching]
     └─ else: turn is done → Done event, return TurnResult
 └─ on max_turns exhaustion: ask for a tools-stripped final summary
```

## `AgentConfig`

Built from a loaded `Config` via `AgentConfig::from_config`:

| Field | Default | Meaning |
|---|---|---|
| `max_turns` | 90 | Tool-calling iteration budget per turn (`run_agent.py:434`) |
| `api_max_retries` | 3 | TOTAL provider attempts per call block (1 initial + n-1 retries) |
| `tool_delay` | 1.0s | Sleep between sequential (non-parallel-safe) tool calls |
| `reasoning` | provider default | Resolved reasoning-effort override, per-model |
| `enabled_tools` | resolved from `toolsets` config | The checked/loaded tool-name set |
| `stream` | `display.streaming` config | Whether responses are streamed via SSE |
| `pass_session_id` | false | Whether to embed a `Session ID:` line in the system prompt |

## `Agent` state

The `Agent` struct (agent.rs:223) owns:

- `client: ProviderClient` — the resolved provider connection.
- `system_prompt: String` — built once in `Agent::new`, never re-rendered
  (see [system-prompt.md](system-prompt.md) for why).
- `history: Vec<Message>` — the running conversation (excludes the system
  prompt, which is injected fresh into each request).
- `session_db` / `session_id` — optional SQLite persistence; every durable
  message (user, assistant, tool result) is appended as it's produced.
- `synthetic_indices` — indices of ephemeral recovery scaffolding messages
  that are **never persisted** and are dropped from a trailing failure
  position if a turn aborts (`run_agent.py:1757-1806`).
- `interrupt: Arc<AtomicBool>` — a cooperative interrupt flag; obtain a
  clone via `Agent::interrupt_handle()` and set it (e.g. from a Ctrl-C
  handler) to stop the turn at the next checkpoint.
- `fallback_chain` / `fallback_index` — the parsed `model.fallback_providers`
  (or root `fallback_providers`) config list.
- `invalid_tool_strikes` — consecutive turns where every tool call in a
  batch was invalid; hits 3 → the turn fails outright.
- `compressor: ContextCompressor` — the context-compaction engine (see
  [context-compression.md](context-compression.md)).

## Provider call block: retries, backoff, and fallback

`call_with_retries` (agent.rs:745) drives one "call block": it will retry
transient errors up to `api_max_retries` TOTAL attempts, honoring the
provider's `Retry-After` header for rate limits (capped at 600s —
`RETRY_AFTER_CAP`), and jittered exponential backoff otherwise
(`jittered_backoff` for generic transient errors, `jittered_backoff_api`
for rate limits without an explicit `Retry-After`).

Decision order on a provider error `e`:

1. **Interrupted?** — an interrupt always wins over any retry decision.
2. **`e.should_compress()`** — context-overflow-shaped errors (413 payload
   too large, context-length errors, output-cap errors) route into
   `handle_context_overflow_error` instead of the generic retry path.
3. **`!e.is_retryable()`** — a non-retryable error tries the fallback
   chain (`e.should_failover()`) before aborting the turn fatally.
4. **Retry budget exhausted** — once `retry_count >= max_retries`, the
   fallback chain is tried one more time before failing fatally.
5. Otherwise: sleep (interruptible) for the computed backoff, increment
   `retry_count`, and loop.

Activating a fallback provider resets `retry_count` and
`compression_attempts` to zero so the new provider gets a clean budget.

## Context-overflow recovery (`handle_context_overflow_error`)

This function (agent.rs:853) implements upstream's
`conversation_loop.py:3196-3842` recovery flow, capped at
`MAX_COMPRESSION_ATTEMPTS = 3` attempts per turn. It distinguishes several
error shapes:

- **`compression.enabled: false` and not an output-cap error** → fails
  fatally immediately with a message pointing at `/compress`, `/new`, or a
  larger-context model. (Output-cap errors are exempt from this guard
  because they aren't an input-overflow condition.)
- **413 payload-too-large** → compress the context, and if the resulting
  history actually got smaller (fewer messages, or ≥5% fewer estimated
  tokens), retry after a 2s cooldown; otherwise fail fatally.
- **"max_tokens too large" (output cap vs. available budget)** — the
  *input* fits but input+requested-output doesn't. This reduces the
  **output** cap only (`ephemeral_max_output_tokens`), never touches
  `context_length`, and retries.
- **Output-cap-shaped but unparseable budget** — compression genuinely
  cannot fix this; fails fast with an actionable message about
  `model.max_tokens`.
- **Genuine input-context-length error** — if the provider's error message
  reports an explicit lower context limit, the compressor's
  `context_length` is updated to that value (never guessed down without
  provider confirmation); then the context is compressed and the request
  retried.

## Tool-call validation and repair

When the assistant's response includes `tool_calls` (agent.rs:1309
onward), each call is checked against the session's `valid_tool_names`
snapshot:

1. **Fuzzy name repair**: an unknown tool name is fuzzy-matched against
   the valid set (`repair_tool_call`); a successful repair emits a
   `Notice` event ("🔧 Auto-repaired tool name...").
2. **Empty argument normalization**: whitespace-only argument strings
   become `"{}"`.
3. **Classification**:
   - *All valid* → strikes reset to 0, proceed to execution.
   - *Mixed* (some valid, some not) → strikes reset to 0; invalid calls are
     individually error-resulted, valid ones still execute.
   - *All invalid* → `invalid_tool_strikes` increments; at 3 consecutive
     strikes the turn fails outright ("Model generated invalid tool call").
     Below 3, every call in the batch gets an error result telling the
     model to retry with a valid name, and the loop continues (spending an
     iteration) so the model can self-correct.

The assistant's tool-call message is always persisted **before** any tool
side effects run, so a crash mid-execution leaves a resumable, well-formed
transcript (`repair_dangling_tool_tail` fixes this up on the next turn if
needed).

## Tool execution: parallel-safe batching (`execute_tool_calls`)

Source: agent.rs:1652, port of `tool_executor.py`.

Tool calls in one assistant turn are split into segments by
`plan_tool_segments` — consecutive runs of "parallel-safe" tools become one
concurrently-executed batch; everything else runs sequentially.

**Parallel-safe tools** (`PARALLEL_SAFE_TOOLS`, agent.rs:40) — read-only,
no shared mutable session state:

```
read_file, search_files, session_search, skill_view, skills_list,
web_extract, web_search
```

For a parallel segment: `ToolStart` events fire for every call up front,
all executions are spawned via `tokio::spawn`, then results are collected
and appended **in the model's original call order** (not completion
order) — this keeps the tool-result transcript deterministic regardless of
which call actually finished first.

For a sequential segment: each call runs to completion, its `ToolEnd`
event and persisted result are emitted, then (if more calls remain and
`tool_delay > 0`) the agent sleeps `tool_delay` seconds — interruptibly —
before the next call. An interrupt mid-batch skips the remaining calls
with a `[Tool execution skipped — ...]` placeholder result so every tool
call still gets a matching result (required by the OpenAI/Anthropic wire
protocols).

## Untrusted tool-result wrapping

Tools whose results carry attacker-controllable content — `web_extract`,
`web_search`, and anything with the `browser_` or `mcp_` name prefix — have
their output passed through `maybe_wrap_untrusted` before being appended to
history. This is a defense-in-depth boundary: it marks fetched content so a
prompt-injection payload embedded in a scraped web page or an MCP tool
result can't blend into the trusted instruction stream. Wrapping only
kicks in above `UNTRUSTED_WRAP_MIN_CHARS` (32 chars) to avoid noise on tiny
results. See [security.md](security.md) for the full untrusted-content
model.

## Recovery constants and prompts

| Constant | Value / purpose |
|---|---|
| `RETRY_AFTER_CAP` | 600s hard cap on any provider `Retry-After` wait |
| `MAX_ITERATIONS_SUMMARY_REQUEST` | Sent (tools stripped) when `max_turns` is exhausted, asking for a final summary without further tool calls |
| `POST_TOOL_EMPTY_NUDGE` | Sent if the assistant executes tools but returns an empty text response, nudging it to process results and continue |
| `LENGTH_CONTINUATION_PROMPT` | Sent when a response is truncated by the output-length limit, asking the model to continue exactly where it left off |

## Interrupts

`Agent::interrupt_handle()` returns an `Arc<AtomicBool>` shared with the
agent's internal checks. Setting it to `true` (e.g. from a Ctrl-C signal
handler wired up by the CLI) stops the turn at the next checkpoint:
between iterations, during backoff sleeps (`sleep_with_interrupt`), and
between sequential tool calls. An interrupted turn calls
`close_interrupted_tool_sequence` to make sure every outstanding tool call
still has a matching result message, then emits `Done { interrupted: true }`.

## Session persistence

When `Agent::set_session_store(db, session_id)` has been called, every
durable message is written to `~/.joey/state.db` as it's produced:

- The user message, immediately on `run_turn` entry.
- The assistant tool-call message, **before** tool execution starts.
- Every tool result, as each tool finishes.
- Interim assistant text during multi-iteration tool loops.
- The final assistant message.

Ephemeral recovery scaffolding (dangling-tail repairs, synthetic
placeholders) is tracked in `synthetic_indices` and is never written to
the DB — it exists only to keep the in-memory `history` well-formed for
the next model call.
