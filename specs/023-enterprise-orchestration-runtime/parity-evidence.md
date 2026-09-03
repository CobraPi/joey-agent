# SC-001 Flag-Off Parity Evidence (T032)

Date: 2026-09-02

## (a) SC-001 criterion (quoted from spec.md)

> **SC-001**: With both feature flags disabled, 100% of existing orchestration behaviors produce identical outcomes to the current release (full backward parity).

## (b) Evidence summary

Full workspace build: green ("Finished `dev` profile ... in 1m 14s", exit 0; only 2 pre-existing warnings in joey-cli: AlwaysPassGate never constructed, project_root unread — both predate spec 023's remaining tasks).

Full workspace test run (flags defaulted false): joey-tools hit ONE failure: `tools::terminal_tool::tests::queue_state_drain_to_zero_inside_window_is_trailing_flushed` panicked at crates/joey-tools/src/tools/terminal_tool.rs:1825 "first emission must show the queued waiter, got (16, 0)" — 278 passed / 1 failed in that crate. joey-tools is UNTOUCHED by spec 023. Re-run in isolation 3/3: "test result: ok. 1 passed; 0 failed" each time → load-induced timing flake, pre-existing, unrelated. The confirmation re-run (post flag-flip) follows in T033.

All spec-023-scoped suites green:
- joey-cli hypercode: 77/77 (flag-off tests incl. sc001/sc005 parity)
- joey-neurocode enterprise_analysis: 7/7
- joey-neurocode outcome_memory_recurrence: 2/2
- joey-orchestration task_graph_validation: 9/9
- joey-orchestration isolation_join: 3/3
- joey-orchestration scheduler_resume: 4/4
- joey-orchestration evaluator_loop: 4/4
- joey-orchestration run_state: 4/4

## (c) Flag-off filesystem probe (no `~/.joey/hypercode/` tree on legacy runs)

Exact command form:

```
JOEY_HOME=$(mktemp -d /tmp/joey-qk-XXXX) cargo test -p joey-cli hypercode_team >/tmp/qk_s1.log 2>&1
```

Result: verbatim output `exit=0` / `NO-HYPERCODE-TREE` (no `~/.joey/hypercode/` tree created).

## (d) Byte-identical legacy path on flag-off

On the flag-off path (SC-001), route_mode / parse_workstreams / the legacy pipeline path are byte-identical to the pre-spec-023 release: the execution-graph conversion block in `run_hypercode` is gated on `config.get_bool("hypercode.execution_graph.enabled", false)` and enterprise-context graph signals are gated on `neurocode.enterprise_context.enabled`, so with both flags false the code paths reduce to the legacy ones unchanged.

Flag-off parity tests, all green:
- `sc001_mode_selection_score`
- `sc005_disabled_parity_routes_subagent`
- `route_mode_is_the_team_gate`
