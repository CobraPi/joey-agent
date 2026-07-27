# Implementation Plan: Expandable Diffs, Thinking & Tool Calls (TUI + CLI)

**Branch**: `005-expandable-diff-ui` | **Date**: 2026-07-25 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `specs/005-expandable-diff-ui/spec.md`

## Summary

Render file changes produced during a turn as inline, syntax-highlighted
unified diffs in both the interactive TUI and the streaming/one-shot CLI,
and make reasoning sections and tool calls collapsible/expandable per-item,
matching the `crush` reference UI's interaction model.

The technical approach exploits a critical existing asset: `joey-tools`
already ships a `file_tracker` module (a port of crush's
`filetracker` + `diff` + `diffdetect`) that is **already wired** into
`read_file` (records the baseline), `write_file`/`patch` (records the
write), and already consumed by a `/changes` slash command in both the REPL
(`repl.rs:1453`) and TUI (`tui.rs:757`). The feature is therefore
**additive plumbing on top of a working tracker**, not a from-scratch
build: (1) surface per-write diffs through the agent event stream so they
render inline at the moment of change rather than only on `/changes`;
(2) add an expand/collapse state machine to reasoning and tool transcript
items; (3) add a syntax-highlighting layer to diff rendering; (4) extend
file-mutation detection to terminal commands.

## Technical Context

**Language/Version**: Rust, stable channel, edition 2021 (per
`rust-toolchain.toml`).

**Primary Dependencies** (existing, in the workspace `[workspace.dependencies]`):
- `similar = "2"` — already declared (a Myers diff library; available but
  the existing `file_tracker::generate_diff` uses a hand-rolled LCS).
- `ratatui = "0.30"` — TUI rendering (`joey-tui`).
- `crossterm = "0.28"` — terminal control (`joey-cli` streaming renderer).
- `nu-ansi-term = "0.50"` — ANSI color painting for the CLI renderer.
- `pulldown-cmark = "0.12"` — markdown (already used by the CLI renderer).

**New dependency under evaluation** (see `research.md` for full decision):
- A syntax-highlighting engine for diff code lines (FR-003, Clarification
  Q5). `syntect = "5"` is the leading candidate; the decision and its
  Principle-VIII cost/benefit analysis are recorded in `research.md`.

**Storage**: In-memory only. The existing `FileTracker` is a process-global
`Lazy<Mutex<FileTracker>>` keyed by normalized path; baselines are
per-session, in-memory, not persisted (Clarification Q2). No SQLite schema
change, no on-disk format change.

**Testing**: `cargo test -p <crate>` per crate; `cargo test --workspace`
for the full suite (~520+ tests, must stay green). The existing
`file_tracker.rs` already has unit tests (`diff_detect_basic`,
`generate_simple_diff`, `stat_line_formats`, …) that must keep passing.
New tests land alongside implementation in the affected crates
(`joey-tools`, `joey-agent-core`, `joey-tui`, `joey-cli`).

**Target Platform**: Cross-platform CLI/TUI (macOS/Linux), same as today.
No new platform targets.

**Project Type**: CLI + TUI within a Cargo workspace (12 member crates).

**Performance Goals**:
- Diff generation for a single file edit: **< 1 ms** for files up to 2,000
  lines (the existing LCS is O(n*m) time and space; acceptable at this
  scale, and consistent with today's `/changes` behavior).
- Syntax highlighting of one diff block: **< 5 ms** p95 for up to 200
  changed lines (warm cache; see `research.md` for the caching strategy,
  mirroring crush's per-line `syntaxCache`).
- Expand/collapse toggle: **< 16 ms** (one frame) — pure state flip + cache
  invalidate, no IO.
- Streaming diff rendering must not regress the existing 12 fps tick loop
  in `render_turn` (FR-014).

**Constraints**:
- `cargo build --workspace` and `cargo test --workspace` MUST stay green
  (constitution Principle VII).
- No SQLite schema bump, no on-disk format bump (constitution Principle VII
  — `SCHEMA_VERSION` stays at 22).
- Strictly additive: the existing `/changes` command, streaming renderer,
  and TUI transcript behavior MUST remain intact (FR-014).
- New dependency weight (binary size, compile time) MUST be justified in
  `research.md` (constitution Principle VIII).

**Scale/Scope**: Single-user local agent session. A turn may touch on the
order of 1–20 files; individual files up to a few thousand lines. No
multi-user/concurrency concerns beyond the existing single-threaded turn
loop (tool execution is sequential for mutating tools).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Evaluated against `.specify/memory/constitution.md` v1.1.0 (all eight
principles):

| # | Principle | Status | Evidence |
|---|-----------|--------|----------|
| I | Workspace-First Rust | ✅ PASS | All work lands in existing crates (`joey-tools`, `joey-agent-core`, `joey-tui`, `joey-cli`). No new crate required — the feature composes existing modules (`file_tracker`) and extends existing event/render types. No code added to the workspace root. |
| II | CLI/TUI Parity | ✅ PASS | The design is explicit about parity: both surfaces consume the same `AgentEvent::FileChange` stream and the same `file_tracker` data. Non-interactive CLI resolves all expand/collapse states to "fully shown" (FR-012), so no content is hidden on a surface without interaction. No capability is TUI-only or CLI-only. |
| III | Filesystem Is Source of Truth | ✅ N/A | This feature does not visualize or edit spec-kit artifacts (`.specify/`). It renders transient agent transcript state. The `FileTracker` reads real on-disk file content as its baseline source. No drift risk. |
| IV | Test-First for New Crates | ✅ PASS | No new crate. New logic in existing crates adds tests alongside implementation (per-module): new event-variant handling in `joey-agent-core`, expand/collapse state in `joey-tui`/`joey-cli`, syntax highlighting in the render path. Existing `file_tracker` tests must keep passing (regression). |
| V | Incremental, Reviewable Delivery | ✅ PASS | The three user stories decompose into independently shippable increments: (1) inline diffs via event stream, (2) expandable thinking, (3) expandable tool calls. Each builds and tests on its own. |
| VI | Modularity and Decoupling | ✅ PASS | The design composes the existing `file_tracker` behind its existing public API (`record_read`, `record_write`, `diff_for_file`) and adds a new event variant behind the existing `AgentEvent` enum. No new logic is threaded through shared core paths: syntax highlighting is a render-layer concern, not an agent-core concern. The `ExpandableSection` state lives on transcript items, not in the turn loop. |
| VII | Backward Compatibility and Non-Regression (NON-NEGOTIABLE) | ⚠️ REQUIRES JUSTIFICATION | Adding a new `AgentEvent` variant is additive and non-breaking (exhaustive matches gain a new arm). **However**, the syntax-highlighting dependency (Clarification Q5) is a new workspace dependency and is recorded here as the only constitutionally-sensitive surface. The mitigation: (a) the dependency is declared in `joey-tools` (the DAG-valid shared ancestor) but invoked only from render paths in `joey-cli`/`joey-tui`, keeping the render concern isolated (see C1 resolution in Structure Decision), (b) its cost is justified in `research.md`, (c) it degrades gracefully (plain coloring fallback), (d) existing rendering behavior is preserved behind a feature flag / config key. No existing CLI flag, config key, exit code, on-disk format, or public API changes. Regression coverage: the existing `file_tracker` and `render_turn` tests must stay green. |
| VIII | Performance Discipline and Lean Code | ⚠️ REQUIRES JUSTIFICATION | Two performance-sensitive paths: (1) diff generation — already exists and meets the <1 ms budget; (2) per-line syntax highlighting in streaming output — new, hot path. The highlighting engine choice, its binary-size/compile-time cost, the per-line caching strategy, and the explicit performance budget are recorded in `research.md`. The default for unrecognized languages is no-op (zero cost). |

**Gate result**: PASS with two justified, documented sensitivities (VII and
VIII), both attributable to the user's Clarification Q5 decision to ship
syntax highlighting in v1. No gate blocks Phase 0.

## Project Structure

### Documentation (this feature)

```text
specs/005-expandable-diff-ui/
├── plan.md              # This file
├── research.md          # Phase 0: syntax-highlighting dependency decision
├── data-model.md        # Phase 1: entities, state machines
├── quickstart.md        # Phase 1: runnable validation scenarios
├── contracts/           # Phase 1: interface contracts
│   ├── agent-event.md   # AgentEvent::FileChange variant contract
│   └── expandable.md    # ExpandableSection state contract
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT this command)
```

### Source Code (repository root)

```text
crates/
├── joey-tools/                      # EXISTING — file_tracker already here
│   └── src/
│       ├── file_tracker.rs          # EXISTING + extend: drain_pending_diffs,
│       │                            #   terminal-mutation detection hook, Delete path
│       ├── highlight.rs             # NEW — shared syntect highlight helper (T001)
│       └── tools/
│           ├── file_tools.rs        # EXISTING — records writes via record_write
│           │                        #   (does NOT emit AgentEvent; turn loop does — T011)
│           └── terminal_tool.rs     # EXISTING + post-run file-change snapshot (T012)
├── joey-agent-core/                 # EXISTING
│   └── src/
│       ├── events.rs                # EXISTING + new AgentEvent::FileChange variant
│       └── agent.rs                 # EXISTING + emit FileChange after mutating tool calls (T011)
├── joey-tui/                        # EXISTING
│   └── src/
│       ├── state.rs                 # EXISTING + ExpandableSection on transcript items
│       └── widgets.rs               # EXISTING + render FileChange diffs, expand/collapse
├── joey-cli/                        # EXISTING
│   └── src/
│       ├── render.rs                # EXISTING + inline FileChange diff rendering +
│       │                            #   calls joey-tools highlight helper (shared, T015)
│       ├── repl.rs                  # EXISTING + per-item expand state for REPL transcript
│       └── tui.rs                   # EXISTING (TUI entry; consumes joey-tui state)
```

**Structure Decision**: No new crate. The feature composes the existing
`file_tracker` module (already a port of the crush reference) and extends
existing event and render types across four existing crates. The dependency
graph stays a strict DAG: `joey-tools` (tracker + diff + shared highlight
helper) → `joey-agent-core` (event variant) → `joey-tui`/`joey-cli` (render).

**Syntax-highlight helper location (C1 resolution)**: Both render crates
(`joey-cli`, `joey-tui`) need the same per-line highlighting helper, and
neither depends on the other. The only DAG-valid shared home is their common
ancestor `joey-tools`, so the `syntect` dependency and a new
`joey-tools/src/highlight.rs` helper module live there. The dependency is
*declared* in `joey-tools` but only *invoked* from render paths in
`joey-cli`/`joey-tui`; it is not used by the tracker, turn loop, or any
non-render code. This keeps the render concern logically isolated while
avoiding code duplication (DRY) and the cost of a speculative new crate
(Principle VIII).

## Complexity Tracking

> Filled because two Constitution Check items (VII, VIII) carry justified
> sensitivities attributable to the syntax-highlighting decision
> (Clarification Q5).

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| New workspace dependency for syntax highlighting (affects VII, VIII) | Clarification Q5 explicitly chose "ship syntax highlighting in v1" — add/remove/context-only coloring was rejected by the user. | Plain `+`/`-` color-only diff (the alternative) does not satisfy the user's explicit decision and the strengthened FR-003. The cost is bounded by confining the dep to the render layer + a per-line syntax cache (see `research.md`); a feature flag/config key preserves the lean-code escape hatch. |
| New `AgentEvent::FileChange` variant (affects VII — public enum) | Inline per-tool diffs require the diff data to reach the renderer through the event stream (Clarification Q1 chose structured tracking); without an event, only the deferred `/changes` summary is possible. | A polling/pull model (renderer asks `FileTracker` on each frame) was rejected because it cannot attribute a diff to the specific tool call that produced it, and it breaks the single-event-stream parity between CLI and TUI (Principle II). The new variant is purely additive (exhaustive matches gain an arm), non-breaking. |
