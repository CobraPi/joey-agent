# Quickstart: Spec-Kit Development IDE

**Feature**: `010-speckit-development-ide` | **Date**: 2026-08-03

This is a **validation/run guide** — runnable scenarios that prove the
feature works end-to-end against the contracts (`contracts/*.md`) and data
model (`data-model.md`). Implementation detail lives in `tasks.md`
(Phase 2), not here.

## Prerequisites

- Rust stable toolchain (`rust-toolchain.toml`); Node 18+ for the frontend.
- A git repository with Spec-Kit initialized (`.specify/` present) and at
  least one feature under `specs/` (e.g. this very feature,
  `010-speckit-development-ide`).
- The `joey` CLI built and on `PATH` (`cargo build --workspace`), plus the
  installed `/speckit-*` skills the steps depend on. The backend drives
  the agent out-of-process (`contracts/workflow-runner.md`).
- `git` on `PATH` (required for staged-mode worktree/apply primitives —
  `contracts/staging-api.md`).
- A configured provider/model so the agent can actually run (else the UI
  shows `missing_credential`, FR-028).

## Setup

```bash
# Backend (local, binds 127.0.0.1)
cargo build -p joey-speckit-ui
./target/debug/joey-speckit-ui --repo "$PWD" --port 7777 &

# Frontend (Vite dev server)
cd web/speckit-ui && npm install && npm run dev
# open the printed localhost URL
```

For a release smoke:
```bash
cargo build --workspace && cargo test --workspace      # acceptance bar
cargo test -p joey-speckit-ui                          # this crate
cd web/speckit-ui && npm run build && npm run test:e2e # frontend
```

## Validation scenarios

Each scenario maps to acceptance criteria in `spec.md` and exercises the
named contract. Run them manually in the browser and/or via the Playwright
journeys under `web/speckit-ui/tests/`.

### 1. Author every artifact (US1 / FR-003/004/005/006/007)
1. Open a feature with spec + plan + tasks + checklists populated.
2. `GET /api/features/{id}/artifacts` lists every kind by workflow phase
   (`contracts/speckit-ui-api.md`).
3. Open `plan.md`, edit a section, save. Expect `PATCH
   /artifacts/{path}` 200 with a new `content_hash`; unrelated content
   preserved; save state transitions `dirty → saving → saved`
   (`data-model.md` §2).
4. Open an artifact with an unresolved marker / malformed section; expect
   a `ValidationFinding` anchored to a location (FR-007).
5. With unsaved edits, switch features; expect a save/discard prompt (no
   data loss).

**Expected outcome**: edits persist to disk synchronously; reloading the
page reflects them; validation findings are navigable.

### 2. Inspect & control the workflow (US2 / FR-008/009/010/034)
1. `GET /api/features/{id}/workflow` shows every step in lifecycle order
   with derived `state` (`ready|blocked|running|attention_needed|
   succeeded|failed|stale|unavailable`) and `blocking_reason` where
   blocked.
2. An unavailable step (e.g. a missing extension) is labelled
   `unavailable`, not simulated.
3. Open a ready step's run config; modify instructions/scope; pick an
   advertised option (`GET /api/options`) and an explicit `change_mode`.
4. Save instructions as a project override (`PUT .../override`); verify a
   later `GET .../config` shows `effective = installed ⊕ override`; remove
   it (`DELETE .../override`) and confirm reversion (FR-034).

**Expected outcome**: every step's state is derived from artifacts +
runs; installed definitions stay read-only.

### 3. Run a step through the native Joey Agent (US2 / FR-011/012/013/014)
1. `POST /api/features/{id}/workflow/{step}/run` with a prepared config;
   expect 202 + `attempt_id`.
2. Subscribe to `WS /api/attempts/{attempt_id}/stream`; observe
   `progress`/`tool`/`output` events streaming (`contracts/workflow-runner.md`).
3. When a `question`/`approval` event arrives, respond via
   `POST .../answer` / `POST .../approve`; the same run continues (FR-013).
4. Cancel a run (`POST .../cancel`); expect a terminal `status: cancelled`
   with a truthful partial change set; the step is **not** marked
   succeeded (FR-014).
5. Start a conflicting second run whose scope overlaps an in-flight one;
   expect 409 `conflicting_run` (FR-015). A disjoint feature runs
   concurrently.

**Expected outcome**: the agent runs out-of-process in feature/repo
context; interactions stream within 2 s (SC-004); cancellation is safe.

### 4. Review changes hunk-by-hunk (US3 / FR-016/017/020 / SC-016)
1. After a completed staged run, `GET /api/attempts/{id}/changes` lists
   files + hunks + `depends_on` warnings.
2. Accept one hunk, reject another that has a dependent; expect a warning
   **before** `POST .../changes/apply` applies (SC-016).
3. In staged mode, confirm accepted hunks land in the primary tree and
   rejected ones do not (`git status` shows only the accepted change).
4. Edit a file on disk after the IDE loaded it, then attempt to apply;
   expect a 409 `conflict` + reload/compare choice (FR-020/SC-005).
5. Recover a failed/cancelled run via `POST .../recover` (FR-017); confirm
   unrelated user changes are preserved (SC-009).

**Expected outcome**: changes are inspectable, selectively reversible, and
external changes are never silently overwritten.

### 5. Stale propagation & readiness (US5 / FR-021/022/032)
1. Edit an upstream artifact (e.g. `spec.md`) and save.
2. Within 3 s, dependent `plan`/`tasks`/steps show `stale` with an
   explanation, **without** their content being deleted (SC-007).
3. `GET .../workflow` blocking reasons point at the right prerequisite;
   trace a requirement → plan section → task → attempt → finding
   (FR-023/032).

**Expected outcome**: readiness is derived, not hand-set; staleness is
explained and recoverable.

### 6. History & restart recovery (US3 / FR-018/019/033 / SC-014/015)
1. Run a step, then re-run it after an edit; `GET
   /api/features/{id}/history` shows two attempts linked by
   `prior_attempt_id` (FR-019).
2. Kill the backend mid-run; restart it. With a valid checkpoint the
   attempt resumes **without replaying unconfirmed actions** (SC-015);
   without one it reports `recovery_failed` with preserved effects
   (FR-033).
3. Confirm records remain reviewable across restarts within 90 days; an
   `expires_at` past 90 days is swept (SC-014).
4. Scale check: load a fixture feature with 500 tasks + 100 attempts + a
   1000-file change set; open artifact / filter tasks / inspect a run in
   < 2 s for ≥ 95 % of interactions (SC-010).

**Expected outcome**: history is durable, streamed, and recovers safely.

### 7. Workspace & accessibility (US4 / FR-002/025/026/027 / SC-011)
1. Resize/collapse/reorder panes, switch views, reload the page; layout +
   last-open artifacts restore (`GET/PUT .../preferences`, FR-026) with no
   unsaved content persisted outside the repo.
2. Search across artifacts, requirement ids, task ids, run records
   (FR-025).
3. Complete the authoring → run → review → approve → recover journeys with
   keyboard only; verify visible focus + descriptive labels (FR-027 /
   SC-011).

**Expected outcome**: navigation is coherent, state survives reload, and
core flows are keyboard accessible.

## References

- Contracts: [`contracts/speckit-ui-api.md`](./contracts/speckit-ui-api.md),
  [`contracts/workflow-runner.md`](./contracts/workflow-runner.md),
  [`contracts/staging-api.md`](./contracts/staging-api.md),
  [`contracts/history-jsonl.md`](./contracts/history-jsonl.md).
- Data model: [`data-model.md`](./data-model.md).
- Tradeoffs: [`research.md`](./research.md).
- Spec acceptance: [`spec.md`](./spec.md) User Stories + FR + SC.
