# Implementation Plan: Async Delegation & Subagent Control

**Branch**: `020-async-delegation-control` | **Date**: 2026-08-26 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/020-async-delegation-control/spec.md`

## Summary

Turn delegation into a non-blocking capability and give the orchestrator (and
operator) live authority over running subagents. Today delegate_task blocks the
orchestrator's whole turn until every child finishes (manager.rs dispatch drains
a JoinSet before the tool future resolves), and the only control is a
manager-wide interrupt. This feature adds: opt-in background delegation
returning a work handle immediately; distilled completion notices delivered at
turn boundaries (and proactively waking an idle engine); per-subagent
steer/stop with recorded stop reasons; parent-enforced budgets
(turns/tokens/wall-clock); a subagent inspection/control tool mirroring the
background process tool; operator stop/steer from TUI subagent panes; a
reserved capacity share for the orchestrator; and graceful session-end
wind-down. All mechanics build on existing primitives: per-child steer handles
and interrupt flags in Agent, the ToolContext pending-completions queue, the
orchestration event tap, and the shared provider semaphore. Design decisions
and alternatives are in [research.md](./research.md); data shapes in
[data-model.md](./data-model.md); interface changes in [contracts/](./contracts/).

## Technical Context

**Language/Version**: Rust stable (edition 2021, rust-toolchain.toml), tokio
async runtime (existing).

**Primary Dependencies**: existing workspace crates only — joey-orchestration
(control plane), joey-agent-core (Agent loop, AgentEvent), joey-tools
(ToolContext completions queue, toolsets), joey-cli (engine idle wake, REPL
wind-down, tool registration), joey-tui (operator pane controls). No new
external dependencies (Principle VIII).

**Storage**: none new — delegation overview and child records are in-memory,
session-lifetime only (FR-019); durable copies exclusively via existing opt-in
subagent session persistence (SQLite, unchanged).

**Testing**: cargo test per-crate integration tests + inline unit tests; new
tests alongside implementation per constitution (manager registry, budgets
watcher, notice distillation, tool schema, wind-down; regression tests for
blocking-path parity per Principle VII).

**Target Platform**: existing desktop targets (CLI + TUI) — no new platform
surface.

**Project Type**: cli agent (cargo workspace member crates).

**Performance Goals**: background handle returned < 2s (SC-001); orchestrator
control/inspection actions complete < 5s under full saturation (SC-007); notice
delivery adds no provider calls; watcher tasks event-driven (no busy polling
beyond the existing 50ms interrupt forwarder).

**Constraints**: strictly additive public surfaces (Principle VII);
delegate_task default behavior byte-compatible; no new on-disk formats
(Principle III); workspace must stay green (cargo build/test --workspace).

**Scale/Scope**: default delegation limits unchanged (3 children / 5 requests /
depth 1, auto-capacity ceiling 32); completion-notice queue capped at 64
(existing cap).

## Constitution Check

GATE: evaluated honestly against all eight principles BEFORE Phase 0 (research
facts confirmed via code exploration) and re-checked after Phase 1 design
below. No unjustified violations; two justified deviations recorded in
Complexity Tracking.

| Principle | Status | Notes |
|---|---|---|
| 0. Cross-platform | PASS | Pure Rust/tokio; no platform-specific APIs. |
| I. Workspace-first Rust | PASS | All work in existing crates; cargo build/test -p per crate. |
| II. CLI/TUI Parity | DEVIATION (justified) | Proactive idle wake and operator pane controls are event-loop surfaces (engine/TUI). The line REPL cannot be woken mid-read (reedline owns stdin synchronously, repl.rs:732) and has no subagent panes today; it degrades to next-interaction notice delivery and session-wide interrupt. See Complexity Tracking row 1. |
| III. FS is source of truth | PASS | No new on-disk formats; overview is in-memory, session-lifetime (FR-019). |
| IV. Test-first for new crates | PASS (N/A) | No new crates; tests added alongside modules. |
| V. Incremental reviewable delivery | PASS | 5 incremental milestones (below), each independently buildable/testable. |
| VI. Modularity | PASS | Control plane lives in joey-orchestration; agent-core only gains additive event variants; UI wiring in joey-tui/joey-cli. |
| VII. Backward compat | PASS (note) | delegate_task default path unchanged; TaskSpec/DelegationResult gain additive fields with defaults; new optional config keys; new tool in existing delegation toolset. Pub-struct field additions are constructed exhaustively only in-repo; noted in Complexity Tracking row 2. |
| VIII. Perf discipline | PASS | No new dependencies; perf budget recorded below; watchers are event-driven. |

## Project Structure

### Documentation (this feature)

```text
specs/020-async-delegation-control/
├── plan.md              # This file
├── research.md          # Phase 0 output — design decisions and alternatives
├── data-model.md        # Phase 1 output — data shapes
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output — interface changes
│   ├── delegation-tools.md
│   └── config-and-events.md
├── checklists/          # requirements checklist
└── tasks.md             # Phase 2 output (/speckit-tasks command - NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-orchestration/src/
│   ├── manager.rs           # child-handle registry: steer/interrupt/budget/meta;
│   │                        # stop_child/steer_child/status; reserved-capacity pools;
│   │                        # shutdown wind-down
│   ├── background.rs        # NEW — background dispatch + completion watcher + budget watcher
│   ├── types.rs             # TaskSpec.background/budgets; DelegationResult.stop_reason;
│   │                        # StopReason; Budgets
│   ├── delegation_tool.rs   # background param; handle result formatting
│   └── control_tool.rs      # NEW — subagent_control tool
│
├── joey-agent-core/src/
│   └── events.rs            # additive variants only (SubagentStopped)
│
├── joey-tools/src/
│   └── toolsets.rs          # delegation toolset gains subagent_control
│
├── joey-cli/src/
│   ├── repl.rs              # tool registration, wind-down hook at session end
│   └── engine.rs            # idle wake command
│
└── joey-tui/src/
    ├── app.rs               # focused-pane stop/steer keybindings → TuiAction → EngineCommand
    └── tui.rs               # focused-pane stop/steer keybindings → TuiAction → EngineCommand

tests:
├── crates/joey-orchestration/tests/   # registry, budgets, notices, wind-down
├── joey-cli/tests                     # wake
└── joey-tui inline tests              # actions
```

**Structure Decision**: single-workspace extension of existing crates — no new
crates, no directory relocations; mirrors how orchestration features landed in
specs/002.

### Incremental Delivery Plan

- M1: registry + per-child steer/stop + stop reasons (manager)
- M2: background dispatch + notice queue + budget watcher
- M3: subagent_control tool + toolset + name-list updates
  (guardrails/compressor/breakdown) + guidance text
- M4: engine idle wake + TUI operator controls + session wind-down
- M5: docs (docs/orchestration.md, docs/state-and-config.md) + PORTING.md note
  (beyond-upstream extension)

Each milestone: cargo build/test --workspace green.

### Performance Budget

| Path | Budget | Measurement Method |
|------|--------|--------------------|
| Handle return (SC-001; excludes provider latency — acceptance is immediate scheduling) | < 2s | Instrument dispatch-to-handle-return in orchestration tests |
| Orchestrator control/inspection under full saturation (SC-007) | < 5s | Integration test with saturated semaphore |
| Notice distillation | ≤ 500-token summaries | Existing child summary budget |
| Watcher tasks | event-driven; zero periodic wakeups added | Assert no timer streams in watcher tests (budget/capacity watchers react to tap events and semaphore state) |
| Memory | overview records O(children); notice queue capped 64 entries (existing) | Registry size assertions in tests |
| Provider-call hot path | no change except which semaphore a permit is acquired from | Code review + existing provider tests |

## Complexity Tracking

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| II: line REPL lacks proactive wake + per-pane operator controls | reedline owns stdin synchronously; waking mid-read requires replacing the line editor; line REPL has no subagent panes to control | Replacing reedline with a custom async reader: large regression risk to core UX for a secondary surface; engine/TUI (HyperCode's surfaces) get full behavior; CLI still gets notices at next interaction and session-wide interrupt |
| VII: additive fields on public structs TaskSpec/DelegationResult | background flag, budgets, stop_reason must travel with existing request/result types to avoid duplicating parallel types | Separate wrapper types (BackgroundTaskSpec etc.): doubles serialization surface and forks tool schemas; additive fields with defaults keep every existing construction site valid (verified: exhaustive literals exist only in-workspace) |
