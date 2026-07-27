---

description: "Task list template for feature implementation"
---

# Tasks: Git Checkpoint Startup Performance

**Input**: Design documents from `/specs/004-git-checkpoint-perf/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/checkpoint-manager-api.md, quickstart.md

**Tests**: Included — the constitution (Principle IV/VII) requires regression coverage for any feature touching a public surface, and this feature explicitly requires the existing `checkpoint_lifecycle`/`checkpoint_noop_on_no_changes` tests to keep passing (FR-006/SC-004) plus new coverage for lazy-init, dedup, pruning, and timeouts (SC-001, SC-002, SC-003, SC-005).

**Organization**: Tasks are grouped by user story per spec.md priorities (US1 = P1 instant startup, US2 = P1 checkpoint/revert unchanged, US3 = P2 bounded disk usage). All work is confined to `crates/joey-tools/src/vcs.rs` plus one call-site change in `crates/joey-cli/src/repl.rs` (per plan.md Project Structure — no new crate).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (US1, US2, US3)
- Exact file paths included in every task

## Path Conventions

Single Rust workspace project. All paths are relative to
`/Users/jo110366/Development/joey-agent`.

---

## Phase 1: Setup

**Purpose**: Confirm environment/baseline before touching the module.

- [X] T001 Confirm `cargo build --workspace` and `cargo test --workspace` are green on `004-git-checkpoint-perf` before any changes (baseline snapshot for regression comparison)
- [X] T002 [P] Record current `checkpoint_lifecycle` / `checkpoint_noop_on_no_changes` test output (`cargo test -p joey-tools checkpoint -- --nocapture`) as the behavioral baseline these must still satisfy after the rewrite

**Checkpoint**: Baseline captured; safe to start structural changes.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core shared-store plumbing that every user story depends on — none of US1/US2/US3 can be implemented or tested until this lands, since they all operate on the rewritten `CheckpointManager` internals in the same file.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

- [X] T003 Add project-hash helper (`sha256(canonicalized_abs_path)[:16]`) and shared-store path constants (`store/`, `indexes/`, `projects/`, ref prefix `refs/joey/`) to `crates/joey-tools/src/vcs.rs`, per data-model.md Shadow Store / Project Ref entities. Store path MUST be derived via `joey_core::joey_home()` (already `JOEY_HOME`-aware) so `HomeOverrideGuard`-based tests transparently exercise FR-010's override behavior with no extra plumbing
- [X] T004 Implement `ensure_store_initialized(store_path)` in `crates/joey-tools/src/vcs.rs`: idempotent (checks `store/HEAD` exists first), `git init --bare`, writes `info/exclude` with the DEFAULT_EXCLUDES list from research.md R4, sets `commit.gpgsign=false`/`tag.gpgSign=false`/`gc.auto=0` via isolated git config calls
- [X] T005 [P] Implement git env-construction helper in `crates/joey-tools/src/vcs.rs` setting `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE` (per-project index path), `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_SYSTEM=/dev/null`, `GIT_CONFIG_NOSYSTEM=1` on every git invocation (FR-004, research.md R6)
- [X] T006 [P] Implement a wall-clock timeout wrapper around `Command::spawn`/`wait` in `crates/joey-tools/src/vcs.rs` enforcing the 5-second limit from FR-005/research.md R5 (poll `try_wait()` + `kill()` on deadline), replacing the current blocking `.output()` calls in `run_git`/`run_git_capture`
- [X] T007 Implement per-project metadata read/write (`store/projects/<hash16>.json`: `workdir`, `created_at`, `last_touch`) in `crates/joey-tools/src/vcs.rs` per data-model.md Project Metadata entity, updated on every successful checkpoint
- [X] T008 Implement `refs/joey/<hash16>` ref-based commit/list/checkout logic in `crates/joey-tools/src/vcs.rs` replacing the current `refs/heads/main`-per-shadow-repo scheme, so `list()`/`revert()` operate against the project's ref in the shared store instead of a dedicated repo's HEAD

**Checkpoint**: Shared store plumbing (init, env isolation, timeouts, metadata, refs) exists and compiles (`cargo build -p joey-tools`) — user story work can now proceed.

---

## Phase 3: User Story 1 - Instant startup regardless of project size (Priority: P1) 🎯 MVP

**Goal**: Reaching the interactive prompt is not blocked by any full-repository scan/add/commit, and repeated sessions in the same project are measurably faster than the first (FR-001, FR-002).

**Independent Test**: Start `joey` in a large repo with `node_modules`/`target`, measure wall time to interactive prompt with checkpoints on vs. off (quickstart.md Scenario 1); start repeatedly in the same project and confirm subsequent sessions are faster (quickstart.md Scenario 2).

### Tests for User Story 1 ⚠️

- [X] T009 [P] [US1] Add test `lazy_init_no_store_before_checkpoint` in `crates/joey-tools/src/vcs.rs` asserting `CheckpointManager::new(...)` does NOT create `store/` on disk, and `store/HEAD` only appears after the first `checkpoint()` call
- [X] T009a [P] [US1] Add test `graceful_degradation_when_git_missing` in `crates/joey-tools/src/vcs.rs` asserting that when `which::which("git")` fails (simulate via an empty/stripped `PATH` in the test env), `CheckpointManager::new(...)` sets `is_enabled() == false` and a subsequent `checkpoint()` call returns `None` without panicking or touching the filesystem (FR-008 non-regression)
- [X] T010 [P] [US1] Add test `dedup_across_manager_instances` in `crates/joey-tools/src/vcs.rs` asserting two separate `CheckpointManager` instances for the same work_tree path reuse the same store/ref/index (same `hash16`) and that re-checkpointing unchanged content adds ~0 bytes to `store/objects`
- [X] T011 [P] [US1] Add test `excludes_applied_to_snapshot` in `crates/joey-tools/src/vcs.rs` creating a `node_modules/` file and a `.env` file in the work tree, checkpointing, then asserting neither path appears in `git show --stat` for that commit (FR-003)

### Implementation for User Story 1

- [X] T012 [US1] Rewrite `CheckpointManager::new()` in `crates/joey-tools/src/vcs.rs` to be cheap: resolve `hash16`/ref name/index path, probe `which::which("git")`, set `enabled` from that probe alone — perform NO filesystem/store mutation (removes the current `init_shadow_repo()` eager call and its whole-tree initial commit) (FR-001)
- [X] T013 [US1] Wire lazy initialization into `checkpoint()` in `crates/joey-tools/src/vcs.rs`: on first call, invoke `ensure_store_initialized()` (T004) + register/update project metadata (T007) before staging/committing; subsequent calls skip store init (idempotent check)
- [X] T014 [US1] Audit `crates/joey-cli/src/repl.rs`'s session-start block (currently ~lines 331-337, eagerly constructing `CheckpointManager`) and remove/guard any code path that triggers a `checkpoint()` call before the interactive prompt is shown; if none exists, add a regression comment noting `CheckpointManager::new()` is now cheap by construction (T012) so no guard is needed
- [X] T015 [US1] Update `repo_path()` in `crates/joey-tools/src/vcs.rs` to return the shared store path instead of a per-session directory, and update `cleanup()` to be a no-op (shared store persists across sessions) per contracts/checkpoint-manager-api.md behavioral contract table

**Checkpoint**: At this point, User Story 1 should be fully functional — `cargo test -p joey-tools` (T009-T011) passes, and quickstart.md Scenario 1/2 can be manually validated.

---

## Phase 4: User Story 2 - Checkpoint/revert functionality is unchanged (Priority: P1)

**Goal**: `/checkpoint` and `/revert` behave identically to today (FR-006), verified via the existing test suite continuing to pass plus manual session validation.

**Independent Test**: Run existing `checkpoint_lifecycle` and `checkpoint_noop_on_no_changes` tests; manually create checkpoints, revert, confirm exact file-state restoration including added/modified/deleted files (spec.md User Story 2 Acceptance Scenarios).

### Tests for User Story 2 ⚠️

- [X] T016 [US2] Update `checkpoint_lifecycle` test in `crates/joey-tools/src/vcs.rs` to match the new lazy-init/shared-store API surface (e.g. first `checkpoint()` call now creates checkpoint #1 instead of `new()` doing so) while preserving all existing assertions about revert correctness (added/modified/deleted file state)
- [X] T017 [US2] Update `checkpoint_noop_on_no_changes` test in `crates/joey-tools/src/vcs.rs` to match the new API (first `checkpoint()` call creates the initial checkpoint; a second no-op call returns the same number) preserving the existing assertion contract

### Implementation for User Story 2

- [X] T018 [US2] Verify/adjust `list()` in `crates/joey-tools/src/vcs.rs` to parse checkpoint commits from the project's `refs/joey/<hash16>` history (via `git log <ref> --pretty=...`) instead of the old shadow repo's default HEAD, preserving the exact `Checkpoint` struct shape and numbering format (`[N] message`)
- [X] T019 [US2] Verify/adjust `revert(number)` in `crates/joey-tools/src/vcs.rs` to checkout from the project's ref in the shared store (`git checkout <hash> -- .` scoped via `GIT_WORK_TREE`), preserving exact current semantics (added/modified/deleted file restoration logic unchanged)
- [X] T020 [US2] Manual validation per quickstart.md Scenario 3: run a live session, `/checkpoint`, edit files, `/checkpoint` again, `/revert <n>`, confirm working directory matches exactly

**Checkpoint**: At this point, User Stories 1 AND 2 both work independently — `cargo test -p joey-tools` fully green, checkpoint/revert semantics unchanged (SC-004).

---

## Phase 5: User Story 3 - Bounded, self-maintaining disk usage (Priority: P2)

**Goal**: Checkpoint storage stays within configured caps and orphaned/stale project data is pruned automatically (FR-007, SC-002, SC-003).

**Independent Test**: Simulate many sessions across multiple project directories over time; confirm total store size stays under the 2GB cap and stale/orphaned data is pruned without user intervention (quickstart.md Scenario 4).

### Tests for User Story 3 ⚠️

- [X] T021 [P] [US3] Add test `prune_orphaned_project` in `crates/joey-tools/src/vcs.rs`: create project metadata for a workdir that no longer exists on disk, run the prune pass, assert its ref/metadata/index are removed
- [X] T022 [P] [US3] Add test `prune_stale_project` in `crates/joey-tools/src/vcs.rs`: create project metadata with `last_touch` set >90 days in the past, run the prune pass, assert it is removed (FR-007's 90-day window per spec Clarifications)
- [X] T023 [P] [US3] Add test `prune_size_cap_drops_oldest_first` in `crates/joey-tools/src/vcs.rs`: simulate/mock total store size exceeding the 2GB cap (or use a lowered test-only cap), assert oldest checkpoints per project are dropped first until under cap
- [X] T024 [P] [US3] Add test `prune_per_project_snapshot_cap` in `crates/joey-tools/src/vcs.rs`: create more than 50 checkpoints for one project, run the prune pass, assert only the 50 most recent remain reachable
- [X] T025 [P] [US3] Add test `git_timeout_enforced` in `crates/joey-tools/src/vcs.rs`: substitute a slow/hanging fake `git` script on `PATH` (via a temp dir prepended to `PATH` in the test), assert a checkpoint operation returns (fails gracefully) within ~5-6 seconds rather than hanging (SC-005)
- [X] T026 [P] [US3] Add test `legacy_shadow_repos_discarded` in `crates/joey-tools/src/vcs.rs`: create a stale pre-v2-style per-session directory under `~/.joey/checkpoints/`, run the prune pass, assert it is removed and no error occurs (FR-009)

### Implementation for User Story 3

- [X] T027 [US3] Implement `.last_prune` marker-file throttle logic in `crates/joey-tools/src/vcs.rs` so the prune pass runs at most once per some interval (not on every checkpoint), per research.md R7
- [X] T028 [US3] Implement orphan pruning (workdir no longer exists) in `crates/joey-tools/src/vcs.rs`, removing that project's ref, index file, and metadata JSON
- [X] T029 [US3] Implement stale pruning (`last_touch` older than 90 days) in `crates/joey-tools/src/vcs.rs`, using the same removal logic as T028
- [X] T030 [US3] Implement per-project snapshot cap (50) in `crates/joey-tools/src/vcs.rs`: when a project's checkpoint count exceeds 50, drop the oldest via history rewriting/ref reset so only the newest 50 remain reachable for `gc`
- [X] T031 [US3] Implement total-store size-cap pass (2GB) in `crates/joey-tools/src/vcs.rs`: measure `store/objects` size, and if over cap, drop oldest checkpoints across projects (oldest-first) until under cap
- [X] T032 [US3] Wire `git gc --prune=now` invocation (with the 5s timeout from T006) after any ref deletions in the prune pass in `crates/joey-tools/src/vcs.rs`
- [X] T033 [US3] Implement legacy per-session shadow-repo discard sweep in `crates/joey-tools/src/vcs.rs`: during the prune pass, remove any non-`store` directories found directly under `~/.joey/checkpoints/` (FR-009, research.md R8)
- [X] T034 [US3] Wire the prune pass (T027-T033) to run opportunistically at the end of `checkpoint()` in `crates/joey-tools/src/vcs.rs`, respecting the `.last_prune` throttle so it never adds startup-adjacent latency

**Checkpoint**: All user stories independently functional — disk usage bounded, orphan/stale/size-cap pruning verified by tests (SC-002, SC-003).

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Final documentation, doc-comment updates, and full regression gate per constitution.

- [X] T035 [P] Update the module-level doc comment at the top of `crates/joey-tools/src/vcs.rs` (currently describes the per-session shadow-repo design) to describe the new shared-store architecture, matching plan.md Summary
- [X] T036 [P] Update `PORTING.md` to record this feature's parity status against `hermes-agent/tools/checkpoint_manager.py` (shared store, excludes, lazy init, retention, timeouts) per AGENTS.md's "PORTING.md is a living audit document" convention
- [X] T037 Run full quickstart.md validation (all 5 scenarios) and record actual startup-latency numbers against the SC-001 ≤100ms budget (Scenarios 1/2's `--print-banner-and-exit`/`--checkpoints` flags don't exist in the CLI, so validated via direct wall-clock timing of `joey home` in a 1050-file fixture repo instead: ~17-25ms per run, no `checkpoints/` dir created — confirms lazy init and the ≤100ms budget with wide margin; Scenarios 3/4/5 validated via their corresponding automated tests, which all pass)
- [X] T038 Run `cargo build --workspace` and `cargo test --workspace`; confirm zero regressions vs. the Phase 1 baseline (T001/T002) — this is the constitution-mandated acceptance bar (Principle VII). Verified: `cargo build --workspace` finished in 16.5s clean; `cargo test --workspace` finished with 0 failures across all crates (joey-tools 138/138 incl. 12 new vcs.rs tests, all other crates unchanged/green); the 5-second git-subprocess timeout's poll interval was tightened (20ms→2ms) after the initial pass to cut wall-clock overhead from many sequential real `git` subprocess calls in the pruning tests, with a follow-up full `cargo test -p joey-tools --lib` re-run confirming 138/138 still green post-tweak

---

## Phase 7: Convergence

- [X] T039 Enforce the 50MB max single tracked file size cap (`MAX_SINGLE_FILE_SIZE_BYTES`) in `crates/joey-tools/src/vcs.rs`'s `checkpoint_internal()` staging step — skip/exclude any file exceeding the cap from `git add` (mirroring `DEFAULT_EXCLUDES` handling) instead of leaving the constant unenforced, and add a regression test asserting an oversized file never appears in the resulting commit per FR-007 (partial). Implemented via a new `unstage_oversized_files()` step run right after `git add --all` in `checkpoint_internal()`: stats every currently-staged path and `git rm --cached`s (index-only, leaves the file on disk) any exceeding the cap — `git rm --cached` was used instead of `git reset HEAD --` because it works correctly even on the very first checkpoint (no HEAD commit yet). Cap is overridable via `JOEY_TEST_MAX_FILE_SIZE_BYTES` for tests, mirroring the existing `JOEY_TEST_STORE_CAP_BYTES` pattern. New test `oversized_file_excluded_from_snapshot` passes; all 13 vcs.rs tests green (139/139 joey-tools lib tests); `cargo build --workspace` clean

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — start immediately.
- **Foundational (Phase 2)**: Depends on Setup completion — BLOCKS all user stories (T003-T008 touch the same core structs/helpers every story builds on).
- **User Story 1 (Phase 3)**: Depends on Foundational completion. This is the MVP.
- **User Story 2 (Phase 4)**: Depends on Foundational completion; in practice best done immediately after US1 since both touch `checkpoint()`/`list()`/`revert()` in the same file, but is independently testable via the existing test suite.
- **User Story 3 (Phase 5)**: Depends on Foundational completion (needs Project Metadata from T007); independently testable via its own prune-focused tests, does not require US1/US2 tasks to be merged first but is easiest to validate once US1/US2 land since it reuses their checkpoint() call path.
- **Polish (Phase 6)**: Depends on all three user stories being complete.

### User Story Dependencies

- **User Story 1 (P1)**: No dependencies on US2/US3, but shares the same file (`vcs.rs`) — sequential implementation is more practical than true parallel team execution here.
- **User Story 2 (P1)**: Builds on US1's rewritten `checkpoint()` (needs lazy-init to exist so tests can construct the new checkpoint numbering), but is scoped to regression-proving list()/revert() only.
- **User Story 3 (P2)**: Builds on Foundational's Project Metadata (T007) and US1's `checkpoint()` call site (T034 hooks pruning into it) — can be developed in parallel with US2 by a second contributor since it touches pruning-specific code paths, not list()/revert().

### Within Each User Story

- Tests written first (T009-T011, T016-T017, T021-T026) and confirmed failing before their corresponding implementation tasks.
- Foundational plumbing (T003-T008) before any user story implementation.
- Story complete (tests green) before moving to the next priority phase.

### Parallel Opportunities

- T002 can run in parallel with T001 (different concerns, same baseline gate).
- T005 and T006 (env isolation, timeout wrapper) are independent helpers and can be implemented in parallel.
- T009, T010, T011 (US1 tests) can be written in parallel (independent test functions in the same file, no shared mutable state across tests given `test_env_lock()`).
- T021-T026 (US3 tests) can all be written in parallel for the same reason.
- T035 and T036 (Polish docs) can run in parallel.

---

## Parallel Example: User Story 1

```bash
# Launch all US1 tests together (same file, independent test fns):
Task: "Add test lazy_init_no_store_before_checkpoint in crates/joey-tools/src/vcs.rs"
Task: "Add test graceful_degradation_when_git_missing in crates/joey-tools/src/vcs.rs"
Task: "Add test dedup_across_manager_instances in crates/joey-tools/src/vcs.rs"
Task: "Add test excludes_applied_to_snapshot in crates/joey-tools/src/vcs.rs"
```

## Parallel Example: User Story 3

```bash
# Launch all US3 pruning tests together:
Task: "Add test prune_orphaned_project in crates/joey-tools/src/vcs.rs"
Task: "Add test prune_stale_project in crates/joey-tools/src/vcs.rs"
Task: "Add test prune_size_cap_drops_oldest_first in crates/joey-tools/src/vcs.rs"
Task: "Add test prune_per_project_snapshot_cap in crates/joey-tools/src/vcs.rs"
Task: "Add test git_timeout_enforced in crates/joey-tools/src/vcs.rs"
Task: "Add test legacy_shadow_repos_discarded in crates/joey-tools/src/vcs.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL — blocks all stories)
3. Complete Phase 3: User Story 1
4. **STOP and VALIDATE**: Run quickstart.md Scenarios 1 & 2, confirm SC-001/SC-002
5. This alone fixes the reported user complaint (slow startup)

### Incremental Delivery

1. Setup + Foundational → shared-store plumbing ready
2. User Story 1 → validate startup latency (MVP!)
3. User Story 2 → validate checkpoint/revert regression suite is green
4. User Story 3 → validate bounded disk usage over simulated multi-session use
5. Polish → docs, PORTING.md, full workspace regression gate

### Parallel Team Strategy

With multiple developers, after Foundational (Phase 2) completes:
- Developer A: User Story 1 (lazy init wiring)
- Developer B: User Story 3 (pruning logic, once T007 metadata exists)
- User Story 2 is best done by whoever finishes US1 first, since it directly regression-tests US1's rewritten `checkpoint()`/`list()`/`revert()`.

---

## Notes

- [P] tasks = different test functions in the same file with no shared mutable state (all tests use `crate::test_env_lock()` to serialize `HomeOverrideGuard` usage — verify this lock still correctly serializes new tests too).
- All tasks are confined to two files: `crates/joey-tools/src/vcs.rs` (bulk of the work) and `crates/joey-cli/src/repl.rs` (one call-site check).
- No new crate or dependency is introduced (research.md Dependency Weight Summary).
- Constitution Principle VII requires `cargo build --workspace` / `cargo test --workspace` green at completion (T038) — this is the final acceptance gate, not optional polish.
- Avoid: reintroducing eager store initialization in `CheckpointManager::new()` (this is the exact regression FR-001 forbids).
