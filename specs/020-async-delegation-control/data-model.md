# Data Model: Async Delegation & Subagent Control

All entities below are **in-memory, session lifetime**; this feature introduces **no on-disk format changes**.

## Entities

- **WorkHandle**
  - Fields: `child_id: String` (existing global child id), `goal: String`, `started_at: Instant`
  - Returned by background `delegate_task`.
  - Validation: `child_id` unique per manager (existing `NEXT_CHILD_ID` global counter guarantees this).

- **ChildHandle** *(internal, manager registry)*
  - Fields: `interrupt: Arc<AtomicBool>`, `steer: Arc<Mutex<String>>`, `task: TaskSpec`, `budgets: Option<Budgets>`, `usage: RunningUsage { iterations: u64, prompt_tokens, completion_tokens, total_tokens }`, `started_at: Instant`, `pending_stop: Option<StopReason>`
  - Lifecycle: created at spawn, removed at completion (result archived into the overview record).

- **TaskSpec (extension)**
  - Existing fields unchanged, plus: `background: bool` (default `false`), `budgets: Option<Budgets>` (default `None`).
  - Validation: budget values must be positive; `max_turns` falls back to `delegation.default_max_turns` when unset.

- **Budgets**
  - Fields: `max_turns: Option<u32>`, `max_tokens: Option<u64>`, `max_wall_clock_secs: Option<u64>`
  - Validation: any present value must be `> 0` (FR-011 — reject invalid immediately).

- **StopReason** *(enum)*
  - Variants: `OrchestratorRequested`, `OperatorRequested`, `BudgetExceeded`, `SessionEnd`
  - Matches FR-010 exactly.

- **DelegationResult (extension)**
  - Existing fields unchanged, plus: `stop_reason: Option<StopReason>` (`None` for natural completion/failure).

- **CompletionNotice** *(formatted string, pushed via the existing background-completion queue)*
  - Grammar: `[SUBAGENT <COMPLETE|FAILED|STOPPED>] id=<id> goal=<goal> outcome=<success|failure|stop_reason> tokens=<total> duration=<secs>s` + newline + distilled summary (≤500 tokens).
  - States are distinguishable per FR-016.

- **SteeringMessage**
  - Fields: `target: child_id`, `text: String`
  - Delivered via the existing pending_steer slot; concatenated when multiple are pending (existing behavior).

- **DelegationOverview record**
  - Fields: `child_id`, `goal`, `state: Running | Completed{result} | Failed{error} | Stopped{reason}`, `elapsed`, `tokens`
  - `Running` → terminal states is one-way; retention is session-lifetime (FR-019).

## State Transitions

```text
Spawned ──▶ Running ──(natural)──▶ Completed
                │
                ├──(natural error)──▶ Failed
                │
                ├──(orchestrator stop)──▶ Stopped{OrchestratorRequested}
                ├──(operator stop)──────▶ Stopped{OperatorRequested}
                ├──(budget breach)──────▶ Stopped{BudgetExceeded}
                └──(session wind-down)──▶ Stopped{SessionEnd}
```

- `Stopped`, `Completed`, and `Failed` are terminal.
- All terminal states emit a completion notice + an overview record update.
- Budget breach originates only from `Running` (detected by the watcher).

## Validation Rules (from requirements)

- Budget positivity: any present budget value must be `> 0` (FR-011).
- Unknown `child_id` in any control action → clear error message (edge case).
- `stop`/`steer` on a terminal child → graceful "already finished" response (edge case).
- Notice queue overflow drops the oldest entry (existing cap 64 — documented behavior, not new).

## Relationships

- Manager 1—N ChildHandle (live) and 1—N DelegationOverview record (session).
- ChildHandle 1—1 child Agent (via steer/interrupt slots).
- DelegationResult 1—0..1 StopReason.
- WorkHandle is the projection of ChildHandle returned to the orchestrator.
