# Implementation Plan: TUI & CLI Spacing / Vertical Rhythm (Crush-Style Readability)

**Branch**: `013-tui-cli-spacing-readability` | **Date**: 2026-08-05 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/013-tui-cli-spacing-readability/spec.md`

## Summary

Port crush's vertical rhythm to the joey TUI and CLI: (1) TUI — exactly one
trailing blank line per `item_lines` variant (uniform inter-block
separator), plus a ~120-column body-text width cap (`capped_content_width`)
applied to assistant/user/reasoning body wraps only; (2) CLI — a single
`pending_separator` flag drained before each renderable element and set
after, giving uniform one-blank spacing across all `AgentEvent` arms with a
trailing-metadata exception for the token-usage line. Strictly additive on
specs 007 (TUI block layout) and 008 (CLI fully-expanded block layout);
presentation-only (no `AgentEvent`/`TranscriptItem`/public-surface change,
no new dependency). See [research.md](research.md) for file:line grounding
of every decision.

## Technical Context

**Language/Version**: Rust, stable channel, edition 2021 (`rust-toolchain.toml`).

**Primary Dependencies**: existing only — `ratatui` + `textwrap` (TUI,
already in `joey-tui/Cargo.toml`); `crossterm`, `tokio`, `joey-core` theme
helpers (CLI, already in `joey-cli/Cargo.toml`). **No new dependency**
(FR-018, research.md §9).

**Storage**: N/A — presentation-only; no SQLite, config, or on-disk format
change (FR-017/018).

**Testing**: `cargo test -p joey-tui`, `cargo test -p joey-cli`,
`cargo test --workspace`. New unit tests alongside implementation
(constitution Principle IV): TUI `item_lines` separator + width-cap tests;
CLI `pending_separator`/spacing tests. The existing
`transcript_hit_test`-delegates-to-`item_lines` design means no hit-test
code change (research.md §3), but a regression test verifies click accuracy
(SC-006).

**Target Platform**: macOS/Linux terminal (the existing `joey` CLI/TUI
targets). No new platform.

**Project Type**: CLI agent + TUI (existing workspace crates `joey-cli`,
`joey-tui`).

**Performance Goals**: TUI render stays viewport-proportional
(`draw_transcript` lazy-build, FR-004); adding one line per item does not
change complexity (research.md §1). CLI adds O(1) work per event (one flag
check). No steady-state latency/throughput regression (constitution
Principle VIII).

**Constraints**: no public-surface change (FR-017); no new dependency
(FR-018); no double-blank/dangling-blank (INV-1, FR-015); TUI live tail
stays visible (FR-004); CLI in-place tool rewrite uncorrupted (FR-014).

**Scale/Scope**: 2 production files (`crates/joey-tui/src/widgets.rs`,
`crates/joey-cli/src/render.rs`) + inline tests. ~8 distinct edit sites in
widgets.rs (5 variants need a trailing blank added; 1 const + 1 helper + 3
call-site caps), ~15 event arms in render.rs get drain/set calls (mechanical,
per the contract table). Small, surgical change.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Evaluated against `.specify/memory/constitution.md` v1.1.0 (all eight
principles). **No violations.** No Complexity Tracking entries required.

| # | Principle | Status | Evidence |
|---|---|---|---|
| I | Workspace-First Rust | PASS | No new crate. Changes confined to existing `joey-tui` and `joey-cli` crates; both remain independently buildable/testable (`cargo build -p`, `cargo test -p`). |
| II | CLI/TUI Parity | PASS | Both surfaces consume the same `AgentEvent` stream unchanged. The feature applies the SAME rhythm intent to both (FR-019); no capability is hidden from one surface. The TUI file-backed `TranscriptItem` source of truth is untouched. |
| III | Filesystem Is the Source of Truth | PASS | N/A — feature has no spec-kit artifact editing UI. No file-backed state is introduced or diverged from. |
| IV | Test-First for New Crates | PASS | No new crate/module. New behavior (separator, width cap, pending_separator flag) gets unit tests alongside implementation (research.md §9; quickstart.md automated gates). Existing `item_lines`/hit-test delegation preserved. |
| V | Incremental, Reviewable Delivery | PASS | Three independently-shippable stories (P1 TUI rhythm, P2 TUI width cap, P3 CLI spacing). Each builds and tests on its own. tasks.md will phase them in this order. |
| VI | Modularity and Decoupling | PASS | No new cross-crate edge. `joey-tui` does not depend on `joey-cli` and vice versa; the feature adds no coupling. The `capped_content_width` helper is a private fn in `widgets.rs` (same pattern as spec 008's local `is_terminal_block` in `render.rs`, research.md §3 of spec 008). |
| VII | Backward Compatibility & Non-Regression (NON-NEGOTIABLE) | PASS | Strictly additive. No `AgentEvent`/`TranscriptItem` variant change, no CLI flag/config-key/on-disk-format change (FR-017/018, INV-4). Regression coverage mandated: hit-test accuracy (SC-006), CLI rewrite (FR-014), gate preservation (FR-016). `cargo test --workspace` stays green (SC-005). |
| VIII | Performance Discipline & Lean Code | PASS | No new dependency (FR-018). O(1) per-event CLI work; O(+1 line/item) TUI work, still viewport-proportional. `MAX_CONTENT_WIDTH = 120` is a single `const` mirroring crush's hardcoded `maxTextWidth` — no speculative configurability. The `pending_separator` flag is the minimal mechanism for uniform dedup spacing (research.md §4 alternatives rejected as more complex). |

**Post-Phase-1 re-check**: research.md §1–§9 and the contracts confirm no
principle is violated by the design. The only ordering-sensitive area
(FR-014, CLI rewrite) is addressed by a precise sequence in
`contracts/cli-render-spacing.md` §3 and a dedicated regression scenario
(quickstart.md Scenario E). No Complexity Tracking entries needed.

## Project Structure

### Documentation (this feature)

```text
specs/013-tui-cli-spacing-readability/
├── plan.md                                  # This file (/speckit-plan output)
├── research.md                              # Phase 0 output — design decisions, file:line grounding
├── data-model.md                            # Phase 1 output — presentational model, invariants
├── quickstart.md                            # Phase 1 output — validation scenarios
├── contracts/
│   ├── tui-item-lines-spacing.md            # Phase 1 — TUI separator + width-cap contract
│   └── cli-render-spacing.md                # Phase 1 — CLI pending_separator contract
├── checklists/
│   └── requirements.md                      # (from /speckit-specify; re-validated in /speckit-clarify)
└── tasks.md                                 # Phase 2 output (/speckit-tasks — NOT created here)
```

### Source Code (repository root)

```text
crates/
├── joey-tui/src/
│   └── widgets.rs                           # item_lines separator (§1) + capped_content_width (§2)
│       └── tests/ (inline #[cfg(test)])     # separator + width-cap unit tests
└── joey-cli/src/
    └── render.rs                            # pending_separator flag + per-arm drain/set (§4)
        └── tests/ (inline #[cfg(test)])     # spacing unit tests
```

No edits under `crates/joey-core/`, `crates/joey-agent-core/`,
`crates/joey-tools/`, or any other crate (FR-017, INV-4). Verifiable via
`git diff --stat` showing only the two renderer files (+ their inline
tests).

**Structure Decision**: Single-project workspace, edits confined to the two
existing renderer files per research.md §9. No new files, no new crates, no
new modules. This is the minimal structure consistent with Principles VI
and VIII.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified.**

No violations. Table intentionally empty — no justified deviations.

## Phases (preview for /speckit-tasks)

The tasks command will decompose this into ordered, independently-testable
tasks. The phase boundaries align with the spec's three user stories
(P1 → P2 → P3) so each increment ships on its own (Principle V):

- **Phase A (P1 — TUI vertical rhythm)**: add one trailing blank per
  `item_lines` variant (research.md §1; contract §1). Unit tests assert
  exactly one trailing blank per variant and one blank between concatenated
  items. SC-001, FR-001/002/003/004.
- **Phase B (P2 — TUI width cap)**: add `MAX_CONTENT_WIDTH` const +
  `capped_content_width` helper; apply at the 3 body-wrap call sites
  (research.md §2; contract §2). Unit tests assert cap on wide panels,
  graceful degradation on narrow, border alignment preserved. SC-002,
  FR-005/006/007/008.
- **Phase C (P3 — CLI spacing)**: add `pending_separator` flag; wire
  drain/set per the contract table across all `AgentEvent` arms
  (research.md §4/§5/§6; contract §1–§5). Unit tests assert one-blank
  rhythm, trailing-metadata exception, no double-blank, gate preservation.
  SC-003/004, FR-009..016.
- **Phase D (regression)**: hit-test accuracy (Scenario C), CLI rewrite
  (Scenario E), full `cargo test --workspace` green (SC-005/006). These are
  coverage tasks mandated by Principle VII for any feature touching a
  public-adjacent surface (the renderers are the user-facing surface).

Each phase ends with `cargo build -p <crate>` + `cargo test -p <crate>`
green (Principle I), and Phase D ends with `cargo test --workspace` green
(Principle VII).

## Done When (planning phase)

- [x] Plan workflow executed (Technical Context, Constitution Check, Project Structure)
- [x] Phase 0 research.md generated — all design unknowns resolved, no NEEDS CLARIFICATION
- [x] Phase 1 design artifacts generated — data-model.md, contracts/, quickstart.md
- [x] Constitution Check re-evaluated post-design — no violations
- [ ] Extension hooks dispatched or skipped (N/A — `.specify/extensions.yml` absent)
- [ ] Completion reported (next)
