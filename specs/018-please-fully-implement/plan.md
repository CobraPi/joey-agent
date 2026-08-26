# Implementation Plan: Concurrent Agent Terminal Performance & UI Responsiveness

**Branch**: `018-please-fully-implement` | **Date**: 2026-08-24 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `specs/018-please-fully-implement/spec.md`

## Summary

Eliminate intermittent UI freezing and stabilize machine behavior when many agents run concurrently, by (1) introducing a process-global Terminal Governor in `joey-tools` that caps the number of concurrently executing agent-initiated terminal command executions, queueing excess requests with per-agent round-robin admission and an auto-sized default (CPU-derived, clamped 4-16, user-overridable via new additive config key `terminal.max_concurrent`); (2) preserving per-call timeout semantics by counting timeouts from admission, not queue wait; (3) wiring cancellation so queued and running requests are cleaned up without orphaned children; (4) surfacing active-vs-queued state through additive events to both CLI and TUI (on-demand `/status` plus a contention-only indicator); and (5) converting the four known residual synchronous process-call sites on UI-reachable paths (clipboard, two `/paste` sites, tmux control) to async or blocking-pool execution. No new crates or dependencies; all public-surface changes are additive.

## Technical Context

**Language/Version**: Rust stable channel, edition 2021 (per `rust-toolchain.toml`)

**Primary Dependencies**: tokio (existing: process, sync, time), std (`std::thread::available_parallelism`). No new dependencies.

**Storage**: Existing layered YAML config (`joey-core/src/config.rs`) + env overrides; one additive key. No on-disk session/state format changes.

**Testing**: `cargo test --workspace` (~520+ tests must stay green); new integration tests in `crates/joey-tools/tests/` (cap enforcement, round-robin fairness, cancellation cleanup, timeout-after-admission, back-compat) + unit tests for auto-sizing clamp and UI indicator state.

**Target Platform**: macOS, Linux, Windows (governor sits above the existing unix/windows spawn twins in `terminal_tool.rs`).

**Project Type**: CLI + TUI application (Cargo workspace, 12 crates, strict DAG).

**Performance Goals**: input echo/scroll < 150ms and no interaction stall > 1s with ≥8 busy agents; process count never above cap under a ≥50-call burst; cancel cleanup ≤ 2s with zero orphans; ≤5% added latency for a lone sequential agent; bounded UI update rate under bursty multi-agent output.

**Constraints**: zero new dependencies; backward-compatible public surfaces (constitution); CLI/TUI parity; upstream fidelity where guidance text is involved.

**Scale/Scope**: ≥8 concurrent agents (main + subagents + background tasks); bursts of ≥50 queued terminal calls.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| # | Gate | Status | Notes |
|---|------|--------|-------|
| 1 | Independent crate buildability | PASS | No new crate; changes confined to existing crates |
| 2 | Backward compatibility of public surfaces | PASS (with verification task) | Additive only: config key `terminal.max_concurrent` (absent = auto default); `ToolContext` gains optional `queue_key` + builder method; `AgentEvent` gains additive variants per the documented additive-event pattern (`events.rs:96-110`) — tasks must verify `#[non_exhaustive]` and update any workspace-internal exhaustive matches; no CLI flag, exit-code, or on-disk format changes |
| 3 | Cross-platform compatibility | PASS | Governor is platform-neutral (admission before both unix and windows spawn paths); platform-scoped fixes stay platform-scoped |
| 4 | CLI/TUI parity | PASS | Active/queued counts reachable in both `/status` (CLI `repl.rs:2290`, TUI `tui.rs:1417`) and TUI status bar (`widgets.rs:draw_status`); contention indicator in both renderers |
| 5 | Tests alongside implementation | PASS | Per-module tests enumerated in research.md D-tasks and to be decomposed in tasks.md |
| 6 | Lean dependencies | PASS | Zero new crates; std + existing tokio features only |

## Project Structure

### Documentation (this feature)

```text
specs/018-please-fully-implement/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md        # Phase 1 output (/speckit-plan command)
├── quickstart.md        # Phase 1 output (/speckit-plan command)
├── contracts/           # Phase 1 output (/speckit-plan command)
├── checklists/          # requirements quality checklist (specify step)
└── spec.md              # feature specification (source of truth)
```

### Source Code (repository root)

```text
crates/
├── joey-tools/
│   ├── src/tools/terminal_governor.rs      # NEW: TerminalGovernor (limit, per-agent queues,
│   │                                        #   round-robin drain, stats snapshot, auto-sizing)
│   ├── src/tools/terminal_tool.rs           # admission acquire before unix/windows spawn;
│   │                                        #   deadline computed AFTER admission; interrupt race
│   ├── src/context.rs                       # additive queue_key + with_queue_key builder
│   └── tests/terminal_governor.rs           # NEW: cap/fairness/cancel/timeout/back-compat tests
├── joey-core/
│   └── src/config.rs                        # default YAML: terminal.max_concurrent (auto)
├── joey-agent-core/
│   ├── src/agent.rs                         # ctx_for_tool sets queue_key; forward queued events
│   └── src/events.rs                        # additive AgentEvent variant(s) + stats payload
├── joey-cli/
│   ├── src/clipboard.rs                     # async clipboard (pbcopy/xclip/wl-copy)
│   ├── src/repl.rs                          # /paste off hot path; /status shows active/queued
│   ├── src/render.rs                        # queued badge near ToolStart arm
│   └── src/tui.rs                           # /paste off pump; /status extension
├── joey-tui/
│   └── src/widgets.rs                       # contention span in draw_status (only when queued>0)
└── joey-omo/
    └── src/team.rs                          # tmux ops via blocking pool / async
```

**Structure Decision**: No new crate. The governor lives in `joey-tools` as a process-global singleton (mirroring the existing `process_registry()` precedent at `process_tool.rs:165`) because admission must span every terminal execution in the process — main agent, subagents, background tasks — and `joey-tools` is the single choke point they all flow through; a constructor-injected or `ToolContext`-carried governor would require cross-crate plumbing through `register_all` and every agent constructor for no additional isolation benefit. Agent identity reaches the governor via a minimal additive `ToolContext` field instead.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

None — no violations.
