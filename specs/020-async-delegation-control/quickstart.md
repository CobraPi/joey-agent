# Quickstart: Async Delegation & Subagent Control
Validation guide — proves the feature end-to-end. Prerequisites: workspace builds (cargo build --workspace) and tests pass; a working provider config (or existing mock-provider test setup); TUI available for scenarios 7-8. Refer to [contracts/delegation-tools.md](contracts/delegation-tools.md) for exact schemas and [data-model.md](data-model.md) for record shapes.

## Scenario 1 — Blocking parity (regression, run FIRST)
Invoke delegate_task exactly as today (no new params) single + batch. Expected: identical behavior/result format; existing delegation tests still green (SC-005).

## Scenario 2 — Background spawn (SC-001)
delegate_task background=true with a task known to take ≥30s. Expected: handle block returns immediately (<2s); immediately run an unrelated read-only tool and observe it completes before the subagent finishes.

## Scenario 3 — Completion notice (FR-003/FR-004)
Continue other work after Scenario 2; at next turn boundary (or engine idle wake) expect one distilled notice: id, goal, outcome, ≤500-token summary, tokens, duration. Verify: no raw transcript content in orchestrator context.

## Scenario 4 — Steering (FR-008)
Start a long task with wrong framing; subagent_control steer{id, message} with corrected instructions. Expected: ack; child's subsequent activity reflects correction; steer on a finished child returns "already finished".

## Scenario 5 — Selective stop (FR-009/FR-010/SC-003)
Start ≥3 background tasks; stop one via subagent_control. Expected: only that child stops (partial result + stop reason in notice); siblings complete normally.

## Scenario 6 — Budget enforcement (FR-011/SC-004)
Start a task with max_tokens set deliberately low. Expected: breach detected, child performs no further actions after detection, notice carries budget-exceeded outcome; invalid budgets (0, negative) rejected at call time.

## Scenario 7 — Operator control (FR-017)
In the TUI with ≥2 subagent panes live: focus one pane, press x → only that subagent stops; focus another, press s, type a steering message → it changes course; orchestrator-visible in log.

## Scenario 8 — Session wind-down (FR-015)
Exit the session while background tasks run. Expected: children wound down within the configured timeout, final statuses recorded, nothing running after exit.

## Scenario 9 — Reserved capacity (FR-018/SC-007)
Saturate all child permits with running subagents; then call subagent_control list/status. Expected: control actions complete <5s (orchestrator reservation honored).

## Concurrency limits (FR-013)
Dispatch more background tasks than delegation.max_concurrent_children. Expected: excess tasks queue (visible via list as running/queued under same limits), none rejected.
