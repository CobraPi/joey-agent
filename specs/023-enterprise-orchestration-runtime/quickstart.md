# Quickstart: Validating the Enterprise Orchestration Runtime

Runnable validation scenarios. Prerequisites: built workspace (`cargo build --workspace`), a scratch git repository with ≥2 modules and a failing-test fixture available, `JOEY_HOME` pointed at a temp dir for isolation. Contract references: [config-keys.md](./contracts/config-keys.md), [planner-json-format.md](./contracts/planner-json-format.md), [run-state-format.md](./contracts/run-state-format.md).

## 1. Flag-off parity (SC-001)
- Set `JOEY_HOME=/tmp/joey-qk` (fresh); run any existing hypercode pipeline and any neurocode context command.
- Expected: outputs identical to the current release; no `~/.joey/hypercode/` tree created; `cargo test --workspace` green (full backward parity suite).

## 2. Analysis plane on (flag: `neurocode.enterprise_context.enabled: true`)
- Request an analysis for a change touching a depended-on module.
- Expected: single report with target + impacted artifacts, combined policies (org/repo/module/scoped listed with sources and surfaced conflicts), complexity reasoning including fan-in/out and GraphHub signals, risk factors, tier, execution hint, scoped verification steps.

## 3. Plan validation (flag: `hypercode.execution_graph.enabled: true`)
- Feed a strict JSON plan containing (a) a dependency cycle, (b) an out-of-project path, (c) a high-risk task without verification, (d) two tasks with overlapping write sets.
- Expected: each rejected before execution with task ids + violated rule named.
- Feed legacy `<workstreams>` output: expected immediate conversion to a validated graph (undeclared write sets → SingleWorker routing).

## 4. Routing (graph-based)
- Prepare four plans shaped for each branch of the router (overlap / depth>2 / independent+coordination / independent).
- Expected: SingleWorker / DagSubagents / Team (team_tasks pre-seeded from graph) / ParallelSubagents respectively.

## 5. Parallel writers + isolation + join
- Run a 2-writer plan in the scratch repo.
- Expected: two `worktree/` checkouts from the same baseline sha; two ChangeBundles with declared==actual write sets; patches integrated into the baseline working tree; incremental index refresh; `git log` shows no new commits; introduce a third conflicting patch → conflict surfaced, nothing applied.

## 6. Interrupt & resume
- Kill a multi-task run mid-wave; resume it (unchanged baseline).
- Expected: completed tasks not re-executed (decision log proves it); in-flight tasks re-dispatched. Move the baseline (new commit) then resume → run refuses with a baseline-mismatch report.

## 7. Gates, repair, escalation, degraded
- Worker returns broken code: gate fails → DefectBundle names the failed command → repair worker fixes → gate passes → Completed.
- Permanently failing task: escalates economical → frontier after `max_repair_attempts`; run fails with a report only after frontier exhausts.
- Unavailable verification command: recorded Degraded; task not complete; not routed to code-defect repair; explicit acknowledgment override clears it.

## 8. Outcome memory
- After 7's repaired task: one OutcomeMemory record exists with provenance (signature, revision, artifact ids, evidence ids). Rewrite the referenced code; consult again → lesson down-ranked/expired. Unverified tasks produce zero records.

## 9. Audit reconstruction (SC-007)
- After any finished run, reconstruct every task's final status using only `graph.json` + `decisions.jsonl` + `evidence/`.
- Expected: every transition accounted for with a cause from the contract vocabulary.

## Validation Log (T031)

Date: 2026-09-02. Environment has no guaranteed live LLM provider; every scenario below was exercised via its deterministic integration-test harness (no live end-to-end model runs). Both feature flags remain defaulted false (flip is T033). The `cargo build/test --workspace` component of §1 is executed as the final workspace gate (T032).

- §1 Flag-off parity (SC-001): PASS — `cargo test -p joey-cli hypercode` → 77 passed; 0 failed; exit 0 (flag-off behavior tests). Filesystem probe with isolated `JOEY_HOME=$(mktemp -d /tmp/joey-qk-XXXX) cargo test -p joey-cli hypercode_team >/tmp/qk_s1.log 2>&1` → verbatim output: `exit=0` / `NO-HYPERCODE-TREE` (no `~/.joey/hypercode/` tree created). Method: integration harness + filesystem probe; workspace-wide parity suite deferred to T032.
- §2 Analysis plane: PASS — `cargo test -p joey-neurocode --test enterprise_analysis` → 7 passed; 0 failed; exit 0 (targets+impacted closure, combined policies/conflicts, GraphHub/tier scoring, risk/hint, scoped verification, flag-off no-graph-signals, revision-unknown). Method: integration harness; no live provider.
- §3 Plan validation: PASS — `cargo test -p joey-orchestration --test task_graph_validation` → 9 passed; 0 failed; exit 0 (cycle, path escape, unverified high-risk, write overlap, unknown dep, missing acceptance, rule+id messages, strict JSON acceptance, legacy `<workstreams>` conversion equivalence). Method: integration harness.
- §4 Routing (graph-based): PASS — `cargo test -p joey-cli hint` → 0 tests matched (1 filtered out) and `cargo test -p joey-cli route` → 0 tests matched (1 filtered out); per brief fallback ran `cargo test -p joey-cli hypercode_team` → 18 passed; 0 failed; exit 0 (includes `try_team_run_honors_graph_route`, `hint_flags_write_overlap_even_when_sequenced`). Method: integration harness; primary filters matched nothing, fallback used.
- §5 Parallel writers + isolation + join: PASS — `cargo test -p joey-orchestration --test isolation_join` → 3 passed; 0 failed; exit 0 (`worktrees_share_baseline`, `two_isolated_writers_integrate_cleanly`, `conflicting_patch_surfaced_untouched`). Method: integration harness on real scratch git repos.
- §6 Interrupt & resume: PASS — `cargo test -p joey-orchestration --test scheduler_resume` → 4 passed; 0 failed; exit 0 (`resumed_run_skips_completed_tasks`, `wave_partition_serializes_overlapping_writers`, `baseline_mismatch_refuses_resume`, `decision_log_reconstructs_final_statuses`). Method: integration harness.
- §7 Gates, repair, escalation, degraded: PASS — `cargo test -p joey-orchestration --test evaluator_loop` → 4 passed; 0 failed; exit 0 (`repaired_success`, `terminal_failure`, `escalation_then_eventual_pass`, `degraded_blocked_until_acknowledgment`). Method: integration harness.
- §8 Outcome memory: PASS — `cargo test -p joey-neurocode --test outcome_memory_recurrence` → 2 passed; 0 failed; exit 0 (`sc009_verified_lesson_guidance_cuts_recurrence_at_least_in_half`, `unverified_tasks_yield_no_guidance_so_failure_recurs`). Method: integration harness.
- §9 Audit reconstruction (SC-007): PASS — `cargo test -p joey-orchestration --test run_state` → 4 passed; 0 failed; exit 0 (includes `sc007_reconstruct_status_from_persisted_files`, `decision_log_is_append_only`, `layout_matches_contract`). Second command `cargo test -p joey-orchestration scheduler_resume` → exit 0 but 0 tests matched by that name filter (9 filtered out in lib, 7 in team_tools); the SC-007 decision-log reconstruction case `decision_log_reconstructs_final_statuses` lives in the `scheduler_resume` harness, which passed 4/4 under §6 above. Method: integration harness.

All 9 scenarios PASS via their harnesses; no FAILs to quote.
