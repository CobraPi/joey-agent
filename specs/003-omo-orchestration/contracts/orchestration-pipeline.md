# Contract: Orchestration Pipeline

**Feature**: 003-omo-orchestration

## Three-Layer Architecture

```
Planning Layer (READ-ONLY)
  Prometheus (planner) + Metis (gap analysis) + Momus (review) + Oracle (arch)
     ↓ writes .omo/plans/{name}.md
Execution Layer
  Atlas (conductor) — reads plan, delegates, verifies, accumulates wisdom
     ↓ delegates via joey-orchestration SubagentManager
Worker Layer
  Sisyphus-Junior (categories) + Oracle + Explore + Librarian + Multimodal-Looker
     ↓ results + learnings back to Atlas
```

## Planning Layer Flow

1. User describes work to Prometheus (via Tab selection or `@plan`).
2. Prometheus loads the `ulw-plan` skill.
3. Prometheus fires Explore/Librarian subagents (background, parallel) for
   codebase context.
4. Prometheus interviews the user (clarifying questions) until intent is clear.
5. Prometheus consults Metis (gap analysis) — mandatory before finalizing.
6. Optionally runs high-accuracy dual review: Momus + Oracle both must approve.
   - If either rejects → Prometheus fixes issues, resubmits (no retry limit).
7. Plan written to `.omo/plans/{name}.md`.

**Constraint**: Planning layer is READ-ONLY. No product code edits. Only
`.omo/` files are created/modified.

## Execution Layer Flow (Atlas)

1. User runs `/start-work [plan-name]`.
2. Atlas reads `.omo/plans/{name}.md`, parses the task list.
3. Boulder state created/updated in `.omo/boulder.json`.
4. For each task (respecting dependency order):
   a. Atlas delegates to a worker (Sisyphus-Junior via category, or
      Oracle/Explore/Librarian via subagent_type).
   b. Worker executes, returns result + learnings.
   c. Atlas verifies the result (reads files, runs diagnostics).
   d. Learnings extracted and accumulated in `.omo/notepads/{plan}/`.
   e. Learnings passed forward to the next worker.
5. Atlas produces a final report.
6. Boulder state marked Completed.

**Constraint**: Atlas MUST delegate all implementation. It never writes code
directly. It can read files, run commands, and verify — but not edit.

## Wisdom Accumulation

After each task completes, Atlas extracts learnings into 5 categories:

| File | Content |
|------|---------|
| learnings.md | Patterns, conventions, successful approaches discovered |
| decisions.md | Architectural choices made and their rationale |
| issues.md | Problems, blockers, gotchas encountered |
| verification.md | Test results, validation outcomes |
| problems.md | Unresolved issues, technical debt flagged |

These are **appended** (never rewritten) and passed forward to subsequent
subagents in the delegation context.

## Boulder State Lifecycle

```
                    /start-work {plan}
                         │
                         ▼
              ┌──── Create BoulderWork ────┐
              │     status: Active          │
              │     session_id: current     │
              └────────────┬───────────────┘
                           │
                    Atlas executes
                           │
              ┌────────────┴───────────────┐
              │   All tasks complete?       │
              └──┬──────────────────────┬───┘
               Yes                     No
                │                       │
                ▼                       ▼
         status: Completed      (remains Active)
         BoulderWork done       user can /start-work
                                again to resume
```

## Session Continuation

Every delegation returns a continuation session ID (`ses_...`). This allows
follow-up interactions with the same subagent without losing context:

- Task failed → `task(task_id="ses_...", prompt="Fix: {error}")`
- Follow-up question → `task(task_id="ses_...", prompt="Also: {question}")`
- Verification failed → `task(task_id="ses_...", prompt="Failed: {error}. Fix.")`

This maps to the existing `DelegationRequest` + `persist` flag in
`joey-orchestration`.

## Integration with joey-orchestration

The OMO pipeline is a semantic layer on top of the existing delegation engine:

| OMO Concept | joey-orchestration Mapping |
|-------------|---------------------------|
| Atlas delegates to worker | `SubagentManager::dispatch_batch` with 1 task |
| Parallel workers | `SubagentManager::dispatch_batch` with N tasks |
| Category routing | Resolves model+prompt, then `DelegationRequest::single` |
| Session continuation | `DelegationRequest` with `persist: true`, reuse session ID |
| Wisdom accumulation | Context string built from `.omo/notepads/` files, passed in `DelegationRequest.context` |
| Boulder state | `joey-omo::boulder::BoulderState` (file-based, separate from SessionDb) |

## Behavioral Contracts

- **BC-026**: Planning layer MUST NOT edit product code (only `.omo/` files).
- **BC-027**: Atlas MUST delegate all code writing — never edits directly.
- **BC-028**: Wisdom accumulated after task N MUST be passed to task N+1's
  delegation context.
- **BC-029**: Boulder state MUST persist across sessions (`.omo/boulder.json`).
- **BC-030**: `/start-work` with no plan name and exactly 1 active work MUST
  auto-resume without asking.
- **BC-031**: Plan parser MUST extract task rows matching
  `- [ ] N. <title>` (implementation) and `- [ ] F<num>. <title>` (final
  verification).
- **BC-032**: Dependency graph in the plan MUST be respected — tasks with
  unmet dependencies are not started until blockers complete.
