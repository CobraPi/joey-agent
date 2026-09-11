# Context Economy (feature 028)

Five default-on mechanisms that keep the model's working context small and
lossless — everything needed stays reachable. Each mechanism is individually
disableable via config, and when any mechanism is disabled the system behaves
exactly as before the feature (byte-parity tests pin this).

## Mechanisms

1. **Session scratchpad** (tool `scratchpad`, toolset `todo`) —
   append/read/clear/stats. Findings recorded outside the conversation
   survive cleanup. Secrets are redacted before persist; entries are
   bounded. Storage lives under
   `~/.joey/scratchpads/<sanitized-session>-<hash>/scratchpad.md`; it
   persists after session end and is discoverable via session search.
2. **Deterministic state block** — TASKS / SCRATCHPAD pointer / PROGRESS
   rendered by fixed rules each turn; appended to the request only (never
   persisted to history). First line is
   `[STATE BLOCK — deterministic, auto-maintained]`. Bounded by
   `state_block.max_chars`; absent when there is nothing to show; skipped
   when the history tail is an unresolved tool result.
3. **Mid-turn tool-result hygiene** — when pressure is in
   `[compression.midturn_threshold, compression.threshold)`: identical old
   tool results are deduped, older-than-protected-tail results are condensed
   in place to one-line summaries (compressor pass-2 wording /
   `[Old tool output cleared to save context space]`). The recent tail
   stays verbatim; the session store keeps verbatim originals. Shares the
   turn-local compression attempt budget with full compression, so both
   never fire in one turn.
4. **Boundary-aligned cleanup** — at turn end, when the todo list is all
   complete or empty and pressure ≥ `compression.boundary_threshold` (but
   below the emergency threshold): runs the existing compressor once. The
   existing 0.50 pressure trigger remains the backstop; failure cooldowns
   are respected.
5. **Economy guidance + retrieval verification nudge** — standing
   concise/pointer/delegation/targeted-read guidance in the system prompt
   (present when the scratchpad tool is enabled); the verify-on-stop nudge
   gains a one-line retrieval reminder when the turn used rag prefetch or
   neurocode cold-mode. Existing nudge caps preserved.

## Configuration (10 keys, all additive; defaults per `DEFAULT_CONFIG_YAML`)

| key | default | clamp / notes |
|---|---|---|
| `scratchpad.enabled` | `true` | |
| `scratchpad.max_entry_chars` | `8000` | clamp 1000..=64000 |
| `state_block.enabled` | `true` | |
| `state_block.max_chars` | `1200` | clamp 200..=8000 |
| `compression.midturn_tool_hygiene` | `true` | |
| `compression.midturn_threshold` | `0.35` | clamp 0.10..=0.45; kept < `compression.threshold` |
| `compression.boundary_trigger` | `true` | |
| `compression.boundary_threshold` | `0.35` | clamp 0.10..=0.45 |
| `agent.context_economy_guidance` | `true` | |
| `agent.retrieval_verification_nudge` | `true` | |

Note: float thresholds are ratios of the model context length, converted to
token counts at runtime.

## Parity guarantees (FR-013/SC-004)

When any switch is off, affected surfaces are byte-identical to pre-feature:

- Registry wire output (`joey-tools` `tests/parity.rs`)
- System prompt text
- Verify-nudge text
- Request message list and history contents (inline `agent.rs` tests)

Default config activates every mechanism (`joey-agent-core` `tests/parity.rs`).
The golden tool list is pinned; the scratchpad appends additively to
`CORE_TOOLS`.

## Observability

`tracing::info!` one-liners: state block rendered/skipped, hygiene swept N,
boundary cleanup fired, scratchpad write.
