# Contract: busy signal, outcomes, and degraded marking

## Busy refusal (FR-002)
Exact tool-result text: `[busy] delegation queue full (N waiting, cap M) — re-plan or defer` (N = current queue depth, M = cap). Returned immediately, no side effects, task not started, nothing enqueued. Sustained-overload signal: when busy refusals occur at a rate of 3 or more within 60 seconds, the busy text appends: ` [overload] sustained saturation detected — consider degraded mode`.

## Outcome kinds (additive)
The existing DelegationResult outcome vocabulary is unchanged; governance adds new outcome kinds reported additively — timeout, aborted_by_resource_limit, cache_hit — recorded in resource records and result text (see data-model.md). Busy refusal is a dispatch-level refusal (a ToolResult), not a DelegationResult outcome.

## Degraded marking (FR-013)
Any output produced under degraded mode carries `degraded=true` in its resource record and a visible ` [degraded]` marker in the result text presented to the model. Critical-priority work never carries the marker (it is never sampled).

## Non-regression obligations
- Busy refusal, timeout, budget exhaustion, cache hit, and degraded output each get contract tests pinning the exact strings and shapes above (under crates/joey-orchestration/tests/).
- With `delegation.resource_governance.enabled=false`, none of these outcomes can occur (parity, SC-007).
