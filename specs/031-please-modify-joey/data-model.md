# Data Model: Goal-Directed Task Execution (Feature 031)

**Date**: 2026-09-15

This feature introduces no persistent data. All entities below are conceptual/session-transient by spec clarification Q1; the only new machine-read surface is one config key (see [contracts/config-key.md](contracts/config-key.md)).

## Entities

### Task Plan

- **Represents**: the agent's commitment for one task — goal, ordered steps, definition of done.
- **Nature**: session-transient visible text (assistant messages in the conversation); no struct, no storage.
- **Validation rules**: must exist before work on non-trivial tasks (FR-001); proportionate to task size (FR-002); re-stated at step transitions to survive context compression (clarification Q1).
- **Lifecycle**: created at task start (stated to user) → executed step-by-step → revised explicitly (Plan Revision) or completed → ceases at task end (no persistence).

### Plan Step

- **Represents**: one discrete, verifiable unit of a Task Plan.
- **Nature**: a line/entry within the plan text; status is communicated in prose at transitions.
- **Validation rules**: individually verifiable (FR-001); served by every action during execution (FR-003).
- **Lifecycle / states**: pending → in progress → done (with verification outcome stated, FR-007) | blocked (with evidence, FR-006). Terminal per task.

### Plan Revision

- **Represents**: an explicit change to a Task Plan.
- **Nature**: a visible assistant message stating what changed, why, and the resulting plan.
- **Validation rules**: the only sanctioned deviation (FR-005); user mid-task instruction wins and enters as a revision (edge case E4).
- **Lifecycle**: emitted at the moment of change; supersedes the prior plan text going forward.

### Guidance Constant: GOAL_DIRECTED_GUIDANCE (implementation entity)

- **Represents**: the stable-tier system-prompt section carrying the goal-directed loop.
- **Nature**: `pub const` string in `crates/joey-agent-core/src/guidance.rs`.
- **Validation rules**: exact wording pinned by a contract test (matches-contract style, guidance.rs:329-342 precedent); presence gated by `agent.goal_directed_guidance` (default true).
- **Lifecycle**: introduced by this feature; never removed at runtime.

## Relationships

- Task Plan 1..* Plan Step (ordered composition, position = execution order)
- Task Plan 1..* Plan Revision (each supersedes prior text; at most one current plan at any time)
- GOAL_DIRECTED_GUIDANCE (const) --governs--> Task Plan / Plan Step / Plan Revision behavior at runtime (no runtime link; textual governance only)

## State Transitions (consolidated)

```text
Task:      no-plan --FR-001(non-trivial)--> planned --execute--> executing --step done|blocked--> executing|revising --all done--> reporting --> closed
           no-plan --FR-002(trivial)--> executing (direct) --> reporting --> closed
PlanStep:  pending -> in_progress -> done(verified) | blocked(evidence) ; blocked --revision--> pending(in revised plan)
```

No on-disk state machines; all transitions are agent behavior specified by guidance and verified via transcript audit (baseline bundle rubric).
