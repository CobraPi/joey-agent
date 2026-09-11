# Contract: state block injection + guidance

## State block message
- Appended to the request message list clone at build_request time (after ProviderRequest::new), as Message::user with first line `[STATE BLOCK — deterministic, auto-maintained]`, sections TASKS / SCRATCHPAD pointer (path + entries + last time) / PROGRESS (turn n of max).
- Never persisted (no push_message), never in self.history, cleared with dedupe key change (new user text).
- Ordering guard: skip when the history tail ends in unresolved tool results.
- Retry-identical within a turn: dedupe key = last user text (neurocode pattern).
- Bound: state_block.max_chars (default 1200); deterministic truncation.

## Guidance constant (Joey-only)
- CONTEXT_ECONOMY_GUIDANCE in guidance.rs, injected via the gated pattern when scratchpad tool present + agent.context_economy_guidance (default true). Wording (upstream-verbatim strings untouched; this is a Joey-only addition, PORTING.md ledger):
  "Work economically with context: keep responses concise — your own output becomes future context. Record discoveries (paths, identifiers, decisions, exact values) to the scratchpad as you find them, before context pressure, so later cleanup is safe. Cite pointers (file paths, ids) instead of pasting content you can re-read. For broad noisy exploration, delegate to sub-agents and keep only their distilled conclusions. Prefer targeted paginated reads (offset/limit) over whole-file loads."
- PORTING.md ledger entry required (guidance + marker + placeholder reuse + config keys + tool + registry cache).

## Non-regression tests (V2/V3)
- Disabled parity: build_request messages identical (byte-level) with state_block.enabled=false vs golden.
- Default-on: block present in request (not history) under default config with todos present.
- Retry: two build_request calls same turn → identical block text.
- Ordering guard: tool-result tail → no block.
- Guidance: present/absent by config.
