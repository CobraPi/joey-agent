# Contract: governor execution events (additive AgentEvent variant)

Follows the documented additive-event pattern (events.rs:96-110). Consumers must treat the enum as open (`#[non_exhaustive]` verified/added; workspace-internal exhaustive matches updated).

## Event shape
- One new `AgentEvent` variant (queue-state change) with payload: `active: usize`, `queued: usize`.
- Emission: on admission and release transitions; producer-side throttled to the existing 50ms budget so bursts cannot flood channels.
- Delivery: existing unbounded event channel → CLI render task / TUI pump; TUI renders last-value-wins within its frame budget.

## Consumer obligations
- CLI: show a queued badge near the active-tool line only while `queued > 0`; print `active/queued` in `/status`.
- TUI: render a contention span in the status bar only while `queued > 0`; include counts in `/status` notice. No persistent indicator in either UI when `queued == 0` (spec clarification Q2).
