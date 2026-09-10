# Contract: tool-result hygiene + boundary cleanup

## Hygiene (mid-turn condensation)
- Site: pre-API pressure check (agent.rs:2853-2898 region), new branch ordered BEFORE full-compression decision, ratio ∈ [midturn_threshold, threshold), enabled gate, dedup-first, oldest-first outside protected tail, in-place content rewrite to pass-2 one-line summary or PRUNED_TOOL_PLACEHOLDER ("[Old tool output cleared to save context space]", compressor.rs:173, verbatim reuse).
- Budget: consumes one shared turn-local compression_attempts slot (MAX_COMPRESSION_ATTEMPTS=3); shares failure cooldown; hygiene and full compression never both in one turn.
- Store fidelity: session store rows untouched (verbatim originals preserved for re-fetch via session_search FTS).
- Distinguishability: no summary message/marker emitted (FR-007).

## Boundary cleanup
- Site: run_turn exit paths (neurocode_auto_reindex site pattern), gated: enabled && todos-all-complete-or-empty (todo_tool::current) && ratio ≥ boundary_threshold && session cooldown clear && turn-local budget > 0.
- Action: existing compressor compress() once; counts as one attempt in its (fresh, post-turn) budget slot.
- Disabled parity: exit paths untouched when off.
- Backstop: existing 0.50 pressure trigger + overflow path remain fully intact and fire as today.

## Observability
- tracing::info! one-liners: hygiene swept N, boundary fired, state block rendered/skipped, scratchpad write.
