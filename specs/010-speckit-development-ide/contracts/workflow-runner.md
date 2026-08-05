# Contract: Out-of-Process Workflow Runner

**Feature**: `010-speckit-development-ide` | **Implements**: FR-011, FR-012,
FR-013, FR-014, FR-033

This contract defines how `joey-speckit-ui` drives the **native Joey Agent
out-of-process**. It is the single interface the backend depends on instead
of linking `joey-agent-core` (Constitution VI; spec FR-011; Clarification
"Joey adaptation" Q1).

## Interface

```text
trait WorkflowRunner {
    /// Spawn the joey CLI / skill wrapper for `step` in the feature's repo
    /// context. Returns an attempt handle whose lifecycle is observed via
    /// the returned event stream + a cancel handle.
    async fn prepare_and_start(
        &self,
        repo_root: &Path,
        feature_id: &str,
        step: &str,
        config: &RunConfiguration,
        staging: &StagingArea,   // where the agent writes (primary or temp worktree)
    ) -> Result<AttemptHandle, RunnerError>;

    /// Send an answer/approval to a pending interaction (FR-013).
    async fn respond(&self, attempt: &AttemptHandle, interaction_id: &str, payload: InteractionPayload)
        -> Result<(), RunnerError>;

    /// Cancel a running/waiting attempt safely (FR-014).
    async fn cancel(&self, attempt: &AttemptHandle) -> Result<(), RunnerError>;
}
```

`AttemptHandle` exposes:
- `events: impl Stream<Item = RunnerEvent>` — the envelope below.
- `attempt_id`, `staging_root` (worktree dir), `child_pid`/handle (for
  cancel).
- `checkpoint()` — current safe checkpoint (Git tree-ish +
  last_confirmed_interaction_id).

## RunnerEvent envelope (over WS `/api/attempts/{id}/stream`)

Newline-delimited JSON; one record per line (research.md §1):

```json
{ "type": "progress",  "attempt_id": "...", "text": "..." }
{ "type": "tool",      "attempt_id": "...", "name": "edit", "summary": "plan.md +12 -3" }
{ "type": "question",  "attempt_id": "...", "interaction_id": "...", "prompt": "...", "choices": null }
{ "type": "approval",  "attempt_id": "...", "interaction_id": "...", "impact": "...", "boundary": "broad" }
{ "type": "output",    "attempt_id": "...", "file": "plan.md", "added": 12, "removed": 3 }
{ "type": "status",    "attempt_id": "...", "terminal": "succeeded|failed|cancelled", "duration_ms": 12345 }
{ "type": "error",     "attempt_id": "...", "message": "...", "recoverable": true }
```

## Invocation contract

- **Command**: `joey <skill-subcommand>` (e.g. `joey /speckit-plan`) **or**
  the relevant `.specify/scripts/bash/<step>.sh` when a skill wrapper is
  unavailable (mirrors `commands.rs::run_script_or_cli`).
- **Working directory**: the staging root — the primary worktree for
  `direct` mode, the dedicated temp worktree for `staged` mode
  (`contracts/staging-api.md`). This ensures run-attributed writes land in
  the right place and never collide with the user's unrelated uncommitted
  work (FR-016).
- **Feature context**: passed via the existing Spec-Kit mechanism
  (`.specify/feature.json` `feature_directory`, or `SPECIFY_FEATURE`
  env), set in the child's environment before spawn.
- **stdin**: line-delimited JSON `InteractionPayload` responses
  (`{"interaction_id":"...","answer":"..."}` or
  `{"interaction_id":"...","decision":"approve"}`) written by `respond()`.
- **stdout/stderr**: captured line-by-line (`tokio::BufReader` over the
  child's pipes) and classified into `RunnerEvent`s. Lines that are not
  valid JSON are forwarded as `progress` text (tolerant parsing, matching
  the `Status::Unparsed` philosophy in `model.rs`).

## Exit-code → terminal status mapping

| Child result | Terminal status | Notes |
|--------------|-----------------|-------|
| exit 0 | `succeeded` | |
| exit ≠ 0, recoverable classification | `failed` (reviewable) | change set preserved |
| signal / cancelled via `cancel()` | `cancelled` | truthful record of completed+incomplete effects (FR-014) |
| restart mid-run | `recovery_needed` → resume from checkpoint or `recovery_failed` | FR-033 |

## Cancellation semantics (FR-014)

`cancel()` drops stdin, sends a termination signal (`SIGTERM` on Unix,
`Child::kill()` on Windows), drains remaining buffered stdout/stderr, then
emits a terminal `status: cancelled` carrying the partial change set. The
step is **not** marked `succeeded`.

## Restart recovery (FR-033)

On backend startup, `recovery.rs` scans in-progress attempts (status
`running`/`awaiting_*` in JSONL). For each:
- **Valid checkpoint** (Git tree-ish exists + last confirmed interaction
  known): resume by re-spawning the agent with the feature context +
  transcript up to the confirmed interaction. Confirmed effects already
  live in the worktree — **no replay of unconfirmed actions**.
- **No valid checkpoint**: mark `recovery_failed`, preserve effects
  (worktree + transcript), emit recovery action. Never replay.

## Non-goals

- No in-process agent call (explicitly rejected — Constitution VI).
- No separate long-lived agent daemon (rejected — adds operational
  surface; per-step subprocess is simpler and already proven by
  `specs/001`).
- The runner does not interpret skill *logic*; it only orchestrates the
  subprocess and classifies its I/O.
