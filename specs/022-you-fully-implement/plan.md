# Implementation Plan: HyperCode Agent Teams

**Branch**: `022-you-fully-implement` | **Date**: 2026-09-02 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/022-you-fully-implement/spec.md`

**Note**: This template is filled in by the `/speckit-plan` command; its definition describes the execution workflow.

## Summary

Add an agent-team execution mode to HyperCode: when a delegated task warrants it, the orchestrator spawns a team lead (configurable model, default inherited from the orchestrator) which decomposes the objective into a shared, file-backed task list and spawns named teammates through the existing delegation machinery. Teammates hold role profiles (read-only investigator or write-capable implementer), claim tasks dependency-safely, message each other directly through per-member mailboxes, and notify the lead on idle or completion. The orchestrator keeps per-task authority to route between the existing subagent mode and the new team mode (documented guidance: independent, parallelizable work → team; sequential, same-file, or interdependent work → subagents), states the chosen mode plus rationale, and records it in the run report. Disabled by default (`hypercode.team.enabled: false`) for strict non-regression.

## Technical Context

**Language/Version**: Rust stable channel (rust-toolchain.toml), edition 2021

**Primary Dependencies**: Existing workspace crates only — joey-core, joey-tools, joey-orchestration, joey-omo, joey-cli (tokio, std::sync; NO new external dependencies)

**Storage**: File-backed team state under `~/.joey/teams/<team-name>/` (honors JOEY_HOME override): `config.json`, `tasks.json`, `inboxes/<member>.json`

**Testing**: `cargo test` — inline `#[cfg(test)]` unit tests alongside new modules + per-crate integration tests; `cargo test --workspace` stays green (constitution acceptance bar)

**Target Platform**: macOS/Linux terminal (existing joey CLI/TUI surfaces)

**Project Type**: Cargo workspace — library crates + CLI binary

**Performance Goals**: Teammate idle/finish/error notification within 30 seconds (SC-003); concurrency bounded by existing capacity logic (4–32 children clamp)

**Constraints**: Disabled by default; zero behavioral regression to existing delegation when disabled (SC-005); one active team per session; no nested teams; teammates get no background-subagent ability; no new external dependencies

**Scale/Scope**: 3–5 teammates typical (hard cap 8), ~5–6 tasks per teammate, pending-inbox cap 10 messages

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | How satisfied |
|---|---|---|
| 0. Complete Cross-Platform Compatibility | PASS | Pure Rust + std/tokio; no platform-specific APIs (tmux visualization explicitly out of scope per spec assumptions) |
| I. Workspace-First Rust | PASS | All work lands in existing workspace crates; no new crate |
| II. CLI/TUI Parity | PASS | Team mode is agent/tool-level; CLI and TUI sessions get identical tool surfaces; the mode decision appears in the shared run report |
| III. Filesystem Is the Source of Truth | PASS | Team task list and mailboxes are file-backed under `~/.joey/teams/<name>/`, written synchronously on every change |
| IV. Test-First for New Crates | PASS | No new crate; the new module `joey-orchestration/src/team.rs` ships inline unit tests alongside it (claim atomicity, dependency blocking, mailbox delivery, persistence round-trip) |
| V. Incremental, Reviewable Delivery | PASS | Delivery slices map 1:1 to spec user stories P1→P4 (routing → collaboration → lifecycle → visibility), each independently testable |
| VI. Modularity and Decoupling | PASS | Team state and tools isolated in one new module depending only on existing types (DelegationRequest, SubagentManager); DAG direction preserved (joey-omo → joey-orchestration) |
| VII. Backward Compatibility and Non-Regression (NON-NEGOTIABLE) | PASS | Strictly additive: optional `delegate_task` params, new `team` toolset, new `hypercode.team.*` keys with safe defaults, optional HypercodeReport field; disabled by default; regression-coverage tasks mandated |
| VIII. Performance Discipline and Lean Code | PASS | No new dependencies; reuses existing spawn/notice/capacity machinery; team state is small JSON documents |

Post-Phase-1 re-check: design (research.md D1–D7, data-model.md, contracts/team-tools.md) introduces no new dependency, crate, or public-surface break — all gates remain PASS.

## Project Structure

### Documentation (this feature)

```text
specs/022-you-fully-implement/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md        # Phase 1 output (/speckit-plan command)
├── quickstart.md        # Phase 1 output (/speckit-plan command)
├── contracts/           # Phase 1 output (/speckit-plan command)
│   └── team-tools.md
└── tasks.md             # Phase 2 output (/speckit-tasks command - NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-orchestration/src/
│   ├── team.rs               # NEW — team registry; file-backed TeamTaskList/TeamMailbox;
│   │                         #      team tools (team_status, team_message, team_tasks);
│   │                         #      TEAM_LEAD_DIRECTIVE + TEAMMATE_DIRECTIVE; inline unit tests
│   ├── delegation_tool.rs    # EDIT — optional `team`/`name` params; team-enabled gating;
│   │                         #      register child as named team member on spawn
│   ├── background.rs         # EDIT (small) — completion notices identify team member name
│   ├── lib.rs                # EDIT — register team tools with delegation group; re-export team types
│   └── tests/
│       └── team_tools.rs     # NEW — integration: registry lifecycle + file round-trip
├── joey-tools/src/
│   └── toolsets.rs           # EDIT — "team" toolset; included for orchestrator-capable children
├── joey-core/src/
│   └── config.rs             # EDIT — hypercode.team.* defaults block in DEFAULT_CONFIG_YAML (+ test)
├── joey-cli/src/
│   ├── hypercode.rs          # EDIT — parse hypercode.team.*; team routing guidance in orchestrator
│   │                         #      overlay; lead dispatch with hypercode.team.lead_model override; HypercodeReport
│   │                         #      gains mode_decisions field
│   └── tests/
│       └── hypercode_team.rs # NEW — config parsing + routing/report tests
├── joey-omo/src/
│   └── team.rs               # UNCHANGED — in-memory primitives + TmuxVisualizer + eligibility stay
└── docs/
    ├── orchestration.md      # EDIT — team mode section
    ├── tools.md              # EDIT — team toolset rows
    ├── features.md           # EDIT — HyperCode team bullet
    ├── state-and-config.md   # EDIT — hypercode.team.* keys + ~/.joey/teams layout
    └── ../PORTING.md         # EDIT — agent-teams parity notes (Claude Code v2.1.178 reference)
```

**Structure Decision**: Single-workspace layout: the feature lands in existing crates along the dependency DAG — team state + tools in joey-orchestration (the lowest crate with SubagentManager access), config defaults in joey-core, orchestration/CLI wiring in joey-cli. joey-omo's place in the DAG (it depends on joey-orchestration) forbids putting execution there, so its team.rs remains a primitives/visualization module (see research.md D7).

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No violations — table intentionally empty.
