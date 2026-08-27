# Contracts: Delegation Tools
Scope: model-facing tool schemas (the project's public interface for this feature). Types referenced: [data-model.md](../data-model.md); behavioral requirements: [spec.md](../spec.md).

## delegate_task (modified, additive)
Existing parameters unchanged (goal, role, context, tasks, model, toolsets, persist...). Additions:
- background: boolean, optional, default false — when true the call returns immediately with work handle(s); false preserves today's blocking behavior byte-for-byte (FR-002).
- budgets: object, optional — { max_turns?: positive int, max_tokens?: positive int, max_wall_clock_secs?: positive int }; invalid (zero/negative/non-numeric) values are rejected at parse with a clear error (FR-011). In batch mode, a top-level budgets applies to every task in the batch; per-task override is out of scope.
Result contract:
- background=false: unchanged single/batch report format.
- background=true: handle report — one block per accepted task: `[BACKGROUND] id=<child_id> goal=<goal> started` — returned within the SC-001 budget (<2s). Queuing note: tasks beyond concurrency limits are accepted and queued under the same limits (FR-013); the handle does not imply a permit is held.

## subagent_control (new tool, toolset: delegation)
Action-based, mirrors the background process tool's UX. Parameters: action (enum, required), id (string, for id-scoped actions), ids (array of string, for wait), message (string, for steer), last (positive int, optional, default 10, for log), timeout_secs (positive int, optional, default 60, for wait).
Actions and results:
- list → per-record line: id, goal (truncated), state (running|completed|failed|stopped:<reason>), elapsed, tokens (FR-005, FR-016, FR-019 session-lifetime records included).
- status → single record detail incl. budget consumption vs caps (FR-005, FR-012).
- log → last N tap events for that child, bounded, never the full transcript (FR-006).
- wait → blocks until all ids terminal or timeout; returns their results; timeout returns partial statuses (FR-007).
- steer → enqueues steering text on the child; ack includes delivery semantics (next action boundary); unknown/terminal id → graceful "already finished"/unknown-id error (FR-008, edge cases).
- stop → requests stop with reason orchestrator-requested; ack immediately; partial result arrives via notice (FR-009, FR-010).
Errors: every action returns a tool-level error string for unknown ids; never panics.

## TUI operator controls (live interface contract)
When a subagent pane has focus: `x` stops that subagent (operator-requested reason), `s` opens the text input overlay to steer it; delivery mirrors orchestrator actions and is visible to the orchestrator in the pane/log (FR-017). Keybindings follow the existing focus model (Ctrl+E/Ctrl+G cycle focus).
