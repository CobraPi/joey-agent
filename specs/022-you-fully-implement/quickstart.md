# Quickstart — Validate HyperCode Agent Teams (Phase 1)

Prerequisites: the workspace builds (`cargo build --workspace`); a sandbox home (`export JOEY_HOME=$(mktemp -d)`) with a configured provider key; for scenarios 2–5 enable team mode by editing `$JOEY_HOME/config.yaml` to set `hypercode.team.enabled: true`.

## Scenario 1 — Disabled-by-default parity (SC-005)

- Run: `cargo test -p joey-orchestration -p joey-cli` (existing delegation suites) and, in the REPL, `/hypercode run summarize docs/tools.md`.
- Expect: the run completes via subagents exactly as before; the report shows `mode=subagent` with a rationale; no `$JOEY_HOME/teams/` directory is created.

## Scenario 2 — Routing decision (FR-007/FR-008, SC-001)

- Run in the REPL: `/hypercode run research agent-team patterns on the web AND independently add a docs section summarizing them` (two independent parts).
- Expect: the report states `mode=team` plus a one-to-two-sentence rationale; a team directory appears under `$JOEY_HOME/teams/`.

## Scenario 3 — Team collaboration (FR-001..FR-006, SC-002)

- Give an objective with at least 3 independent tasks; inspect `$JOEY_HOME/teams/<team>/tasks.json` during and after the run.
- Expect: every task passes Pending → Running → Done exactly once; dependency ordering is never violated; `inboxes/*.json` contain lead-to-teammate AND teammate-to-teammate messages; the final answer synthesizes all completed tasks.

## Scenario 4 — Lifecycle and cleanup (FR-011/FR-012, SC-004)

- End the session (exit the REPL) during or after a team run.
- Expect: all children are stopped; `config.json` and `inboxes/` are removed; `tasks.json` is retained; a new session can read past progress from `tasks.json`.

## Scenario 5 — Visibility and control (FR-014/FR-015)

- During a run: request team status (team_status output; `subagent_control` list shows teammates).
- Stop one teammate (`subagent_control stop <child>`): its Running task returns to Pending and another teammate (or the lead) can claim it.

Cross-references: entities and state machines → data-model.md; tool parameters, configuration keys, file formats → contracts/team-tools.md.

## Scenario 6 — SC-001 mode-selection scoring

- Eval set: the 10 canonical routing tasks encoded in `crates/joey-cli/src/tests/hypercode_team.rs` (`sc001_mode_selection_score`) — 5 team-suited goals (planner decomposition yields >=2 independent workstreams) and 5 subagent-suited goals (single workstream: sequential, same-file, or single-focus work).
- Rubric: a selection is correct when `route_mode` picks `Team` for the team-suited tasks and `Subagent` for the subagent-suited ones.
- Threshold: >=9/10 correct in a single run (`cargo test -p joey-cli sc001_mode_selection_score`); every selection carries mode + rationale via `HypercodeReport::mode_decisions` (FR-016).
