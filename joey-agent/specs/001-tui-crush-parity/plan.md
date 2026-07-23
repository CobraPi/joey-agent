# Implementation Plan: TUI Crush Parity

**Branch**: `001-tui-crush-parity` | **Date**: 2026-07-23 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `specs/001-tui-crush-parity/spec.md`

## Summary

Rework `crates/joey-tui` so the terminal UI matches Crush's (`/Users/jo110366/Development/crush`,
Go/Bubble Tea/Lip Gloss/Ultraviolet) layout, interaction model, and default aurora-synthwave
theme, while keeping Joey Agent's own crate boundaries and Rust/ratatui stack. Concretely: add
a header + sidebar + transcript + editor layout with a 120x30 compact-mode breakpoint; extend
`TranscriptItem` rendering to real streaming-markdown + syntax-highlighted diffs with per-status
tool-call blocks; introduce a `Dialog` trait + registry so model/session/permission/confirm/
completions/quit/reasoning/notification/question-form overlays are pluggable; rewrite `theme.rs`
to Crush's exact palette + semantic roles + gradient math; add a `Capabilities` detector with
ANSI-16/ASCII fallback covered by unit tests; and extend `AppState` with sidebar-backing data
(changed files, LSP status, MCP status, skills status) sourced from existing crates
(`joey-tools::vcs`, `joey-mcp`, `joey-tools::tools::skills_tool`) plus a new minimal LSP-status
stub, per the Clarifications in spec.md.

## Technical Context

**Language/Version**: Rust 1.80+ (workspace `rust-toolchain.toml`), edition 2021

**Primary Dependencies**: `ratatui` 0.30 (crossterm backend), `crossterm` 0.28, `unicode-width` 0.2,
`textwrap` 0.16, `similar` 2 (diff), `joey-agent-core` (state source), and — new for this feature —
a markdown-to-styled-text renderer and a syntax highlighter for code blocks (selection below,
see research.md R1) added as `joey-tui` dependencies only.

**Storage**: N/A — the TUI is a pure rendering/interaction layer over `joey-agent-core`'s
in-memory `AgentEvent` stream and `joey-core`'s existing session store; no new persistence.

**Testing**: `cargo test -p joey-tui` (existing `tests/smoke.rs` pattern: build an `AppState`,
render into a `ratatui::backend::TestBackend`, assert buffer content) plus new seam-level tests
per Constitution Principle IV for the `Dialog` registry, theme palette values, and capability
fallback logic.

**Target Platform**: Same terminals Joey Agent already targets — macOS/Linux terminal emulators
with crossterm support; truecolor is preferred, ANSI-16 fallback is in scope per FR-011.

**Project Type**: Single Rust workspace, TUI crate (`crates/joey-tui`) consumed by the `joey-cli`
binary's `tui.rs` host loop. No frontend/backend split.

**Performance Goals**: Match existing `joey-tui` frame budget (`Tui::frame_budget`, currently
~60fps-class animation cadence per `anim.rs`); no regression in draw latency for a transcript at
`transcript_capacity` (1024 items).

**Constraints**: Must not break the existing `joey-cli` host loop's public contract
(`Tui::enter`, `Tui::handle_key`, `Tui::tick_animations`, `Tui::draw`, `TuiAction`) — `joey-cli`
is out of scope for structural rewrite in this feature, only pass-through wiring for new dialogs/
sidebar data as needed. Must not introduce cross-crate `pub(crate)` reach-arounds per Constitution
Principle I.

**Scale/Scope**: One crate (`joey-tui`), touching all 6 existing modules (`anim`, `app`, `input`,
`state`, `theme`, `widgets`) plus 2 new modules (`dialog`, `capabilities`) and a `sidebar` module;
~10 new dialog variants (model/session/permission/confirm/completions/quit/reasoning/
notification/question-form family); 4 new sidebar sections (files/LSP/MCP/skills).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

- **Principle I (Crate Boundaries)**: This feature touches `crates/joey-tui` only, as its
  single responsibility (terminal UI rendering/interaction) is exactly what's being reworked.
  It reads (does not modify) `joey-tools::vcs` (for changed-files data, via a new small public
  function if none exists — see research.md R2), `joey-mcp` (for MCP server status, already
  exposed), and `joey-tools::tools::skills_tool` (for skills status). No new crate is added
  because LSP status is scoped to a stub (see Clarifications) that lives entirely in
  `joey-tui::state` and does not require a real LSP client crate. `joey-cli/src/tui.rs` gets
  minimal additive wiring (new `TuiAction` variants, new event plumbing) — it is the existing
  integration seam, not a boundary violation.
- **Principle II (Traits/Registries)**: Two new extension points are introduced:
  (a) a `Dialog` trait + `DialogStack`/registry in the new `dialog` module — each dialog
  variant (model picker, session picker, permission, confirm, completions, quit, reasoning,
  notification, question-form single/multi/freetext/confirm/editor) implements it and is
  constructed via a `DialogKind` factory, so adding a new dialog later means adding one module,
  not editing a central draw/key-handling match; (b) a `TranscriptRenderer` trait per
  `TranscriptItem` variant so new item kinds (if Crush parity work continues later) plug in
  without touching the whole `widgets::draw_transcript` function. The existing tool-status
  enum-match in `widgets.rs` is refactored into this registry as part of this work (paying down
  the pre-existing conditional per Principle II's "don't deepen existing chains" rule).
- **Principle III (Minimal Public Surface)**: `dialog` and `capabilities` modules keep concrete
  dialog structs and downsampling internals `pub(crate)`; only the `Dialog` trait, `DialogKind`
  enum, `DialogStack`, `Capabilities` struct, and `detect_capabilities()` function are `pub`,
  since `joey-cli` needs to construct/query them. Sidebar backing data added to `AppState` is
  plain data (`Vec<SidebarFileChange>`, `LspStatus`, etc.) with no ambient globals.
  All are consistent with today's `app.rs`/`state.rs` visibility pattern (`pub struct App` with
  `pub` fields consumed directly by `joey-cli`).
- **Principle IV (Test the Seam)**: New seam-level tests: (1) register two fake `Dialog`
  implementations in `DialogStack` and assert push/pop/key-routing works generically; (2) a
  `capabilities` unit test table (truecolor→24bit passthrough, no-truecolor→ANSI-16 downsample,
  no-unicode→ASCII glyph substitution) satisfying FR-011/SC-004; (3) a `TranscriptRenderer`
  test registering a stub item kind and asserting `draw_transcript` calls it without special-
  casing. These are in addition to existing `tests/smoke.rs`-style buffer-content tests for the
  concrete new widgets (sidebar sections, diff view, dialogs).
- **Principle V (YAGNI)**: The `Dialog` and `TranscriptRenderer` traits are justified by a
  concrete near-term need already in this spec (≥10 dialog variants, 5 transcript item kinds
  today) — not speculative. The LSP-status sidebar section is intentionally a stub (no new LSP
  client crate) because Joey Agent has no LSP integration to report on yet; building a real LSP
  client is explicitly out of scope (see Assumptions in spec.md) and would violate this
  principle by adding infrastructure with no consumer.

No unresolved violations. Complexity Tracking table below is empty.

## Project Structure

### Documentation (this feature)

```text
specs/001-tui-crush-parity/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output
├── data-model.md         # Phase 1 output
├── quickstart.md         # Phase 1 output
├── contracts/            # Phase 1 output
│   ├── dialog-trait.md
│   ├── transcript-renderer-trait.md
│   └── capabilities-api.md
└── tasks.md              # Phase 2 output (/speckit-tasks — not created here)
```

### Source Code (repository root)

```text
crates/joey-tui/
├── Cargo.toml            # + similar (already a workspace dep), + markdown/highlight deps (research.md R1)
├── src/
│   ├── lib.rs             # + pub mod dialog; pub mod capabilities; pub mod sidebar;
│   ├── theme.rs            # rewritten palette/semantic roles/gradient to match Crush (FR-007/FR-008)
│   ├── state.rs             # + SidebarFileChange, LspStatus, McpStatusSummary, SkillsStatusSummary,
│   │                         #   DialogStack field, Capabilities field (FR-009a, FR-011, FR-012)
│   ├── input.rs              # + keybinding parity check against Crush's textarea (FR-010)
│   ├── widgets.rs              # rewritten transcript rendering: streaming markdown, tool-status
│   │                             # icons/colors, diff view; TranscriptRenderer registry (FR-003–006)
│   ├── anim.rs                 # unchanged (already provides spinners Crush-equivalent uses)
│   ├── app.rs                   # + compact-mode breakpoint (120x30), + DialogStack key routing,
│   │                             #   + sidebar layout region (FR-001/FR-002)
│   ├── dialog/                   # NEW module
│   │   ├── mod.rs                 # Dialog trait, DialogKind enum, DialogStack
│   │   ├── models.rs                # model picker
│   │   ├── sessions.rs               # session picker
│   │   ├── permission.rs              # permission request
│   │   ├── confirm.rs                  # yes/no confirmation (also backs quit confirmation)
│   │   ├── completions.rs               # slash/command completions
│   │   ├── reasoning.rs                  # reasoning-level picker
│   │   ├── notification.rs                # in-app notifications
│   │   └── question.rs                     # single/multi/freetext/confirm/editor question forms
│   ├── capabilities.rs             # NEW: Capabilities struct, detect_capabilities(), downsample fns
│   └── sidebar.rs                    # NEW: sidebar section renderers (session/model/files/lsp/mcp/skills)
└── tests/
    ├── smoke.rs                       # existing; extended with new widget assertions
    ├── dialog_seam.rs                  # NEW seam test (Principle IV)
    ├── capabilities.rs                  # NEW seam test (Principle IV)
    └── transcript_renderer_seam.rs       # NEW seam test (Principle IV)

crates/joey-cli/src/tui.rs   # additive wiring only: route new TuiAction variants, feed sidebar
                              # data sources (vcs/mcp/skills) into AppState each frame
```

**Structure Decision**: Single Cargo workspace, single crate in scope (`crates/joey-tui`), with
minimal additive wiring in the existing consumer crate (`crates/joey-cli`). No new workspace
members — the "Option 1" single-project layout from the template applies, mapped onto Joey
Agent's real crate/module structure above.

## Complexity Tracking

*No Constitution Check violations — table intentionally empty.*
