# Implementation Plan: Claude Code-Style CLI Animations

**Branch**: `002-claude-code-cli-style` | **Date**: 2026-07-24 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/002-claude-code-cli-style/spec.md`

**Note**: This template is filled in by the `/speckit-plan` command; its definition describes the execution workflow.

## Summary

Bring claude-code's interactive CLI animation feel to the line-based `joey` REPL while keeping the existing Crush/Pantera color palette and leaving the `joey-tui` crate untouched. The work centers on refactoring `render::render_turn` (currently a blocking event-receive loop with plain `print!`) into a tick-loop renderer using crossterm cursor control, adding: an animated startup banner entrance (R-006), a thinking spinner+label while awaiting first token (R-001/FR-002), progressive-raw-then-markdown-finalize streaming reveal (R-002/R-003/FR-003), per-tool entry/running/resolved animated lines (R-007/FR-004), a persistent token usage line + turn-complete summary (R-004/FR-005), and a subtle prompt caret blink (FR-006). No changes to `joey-agent-core`'s `AgentEvent` are required — all data already flows through existing events. Markdown finalize adds `pulldown-cmark` (already a workspace dep) to `joey-cli`.

## Technical Context

**Language/Version**: Rust (workspace `edition` from root Cargo.toml; stable).

**Primary Dependencies**:
- Existing, reused: `reedline 0.40`, `nu-ansi-term 0.50`, `crossterm 0.28`, `unicode-width 0.2`, `terminal_size`, `tokio` (full), `once_cell`, the `joey-core`/`joey-agent-core`/`joey-providers`/`joey-tools` workspace crates.
- Promoted from workspace to `joey-cli`: `pulldown-cmark 0.12` (already used by `joey-speckit-ui`) — for the markdown finalize step (R-003).
- No other new dependencies.

**Storage**: N/A — animations are ephemeral render state, not persisted. Session/token state remains in `SessionDb` (joey-core), unchanged.

**Testing**: `cargo test` (workspace). New unit tests for: markdown→ANSI renderer seam (R-003), `RenderCapability` profile selection + frame substitution (R-005/SC-004), spinner frame advancement, and the plain-text fallback path (FR-011). Manual QA: run `joey` interactively and pipe (`joey -q`) to verify both paths.

**Target Platform**: Cross-platform terminal (macOS/Linux primary; Windows supported via crossterm). No GUI.

**Project Type**: CLI (the `joey` binary, `joey-cli` crate) — specifically its interactive REPL rendering path.

**Performance Goals**: Animation tick at ~12 fps (~83ms interval) default; spinner/caret advance on tick. Streaming text prints immediately on `ContentDelta` (no added latency). Markdown finalize is a single reflow on completion (bounded, one message). CPU overhead when idle: negligible (tick timer dormant except during active turn/banner).

**Constraints**: Must not flicker or leave partial frames on resize (FR-007); must disable animations on non-TTY stdout and fall back to plain text (FR-011); must not alter `joey-tui` (SC-005); must not introduce a competing color palette (FR-009).

**Scale/Scope**: Single crate (`joey-cli`), ~3-5 new/modified modules within `crates/joey-cli/src/` (render.rs refactor + new animation/profile/markdown/capability submodules). No cross-crate API changes.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Constitution: `.specify/memory/constitution.md` v1.0.0 (ratified 2026-07-23). Five principles checked below.

### Principle I — Crate Boundaries Are the Modularity Unit (NON-NEGOTIABLE)

**Crate(s) touched**: `joey-cli` only.

**Why**: The interactive line-based CLI REPL and its rendering live entirely in `joey-cli` (`src/repl.rs`, `src/render.rs`). Animation is a presentation concern of this crate's binary; it does not belong in `joey-core` (branding/config/state), `joey-agent-core` (turn loop), `joey-tools`, `joey-providers`, or `joey-tui` (the full-screen app, explicitly out of scope per spec SC-005).

**New cross-crate dependencies introduced**: None. The `pulldown-cmark` promotion is workspace-internal (already a workspace dep used by `joey-speckit-ui`); `joey-cli` does not gain a dependency on a new external crate or on a sibling crate it didn't already depend on. Dependency direction (`joey-core` → … → `joey-cli`) is unchanged.

**Verdict**: PASS. Feature lives in the single crate whose responsibility matches it.

### Principle II — Extend via Traits and Registries, Not Conditionals

**Extension point introduced**: Yes — an `AnimationProfile` registry-style mapping keyed by animation kind (Banner, ThinkingSpinner, StreamingCaret, ToolLine, PromptCaret), each resolving a concrete frame-set/timing/color spec, including a reduced-capability variant.

**Why trait/registry over central match**: The animation system has N elements that each need (frames, interval, color, fallback). Enumerating them in one big `match` inside the render loop would be the forbidden central-chain pattern. Instead, an `AnimationProfile` value per kind (lookup table / small registry) lets a new animation be added by registering one new profile, without editing a central conditional.

**Implementation shape**: `AnimationProfile` is a plain data struct (frames: `Vec<String>`, interval_ms: u32, color: `Rgb`, reduced: `Box<AnimationProfile>`). A `profile(AnimationKind) -> &'static AnimationProfile` function (or a `const` registry array) provides the lookup. This is a data registry, not a deep trait hierarchy — appropriate because animations differ only in data, not behavior (Constitution Principle V: don't over-abstract).

**Verdict**: PASS. A data registry satisfies "extend without editing a central match/if chain."

### Principle III — Explicit, Minimal Public Surface Per Module

**New public surface in `joey-cli`**: Minimal.
- `render::banner_animated` (or the render module's animated entry) — called by `repl.rs`.
- `render::render_turn` signature is unchanged (`pub async fn render_turn(rx, opts) -> String`) — the refactor is internal.
- New submodules (`animation`, `profile`, `markdown`, `capability`) expose `pub(crate)` items only, except where `render.rs` re-exports the one or two entry points it needs. No new `pub` items leak beyond what `repl.rs` calls.
- `RenderOptions` gains at most an `animation_fps: Option<u32>` / `animations_enabled: bool` field — plain data, consumed internally.

**Why nothing smaller suffices**: the animation entry points must be callable from `repl.rs` (the only caller). Anything narrower would inline animation logic into the REPL, violating separation.

**No cross-crate surface change**: `joey-agent-core::AgentEvent`, `joey-providers::Usage`, `joey-core::theme::Theme` are consumed read-only as today; no new fields/methods are added to them.

**Verdict**: PASS. Surface is internal to `joey-cli`, minimal, plain-data where it crosses the render↔repl boundary.

### Principle IV — Test the Seam, Not Just the Implementation

**Seam-level tests required**:
1. **Markdown renderer seam**: `markdown_to_ansi(input, &theme)` — a pure function; test that given markdown input it emits ANSI-styled output with expected color roles (headings→gradient, code→accent). Fails if the pulldown-cmark event→ANSI mapping breaks.
2. **Capability/profile selection seam**: given a `RenderCapability` (non-TTY / no-truecolor / no-Unicode / full), assert the correct `AnimationProfile` variant (disabled / reduced / full) is selected. Fails if fallback selection regresses (SC-004).
3. **Plain-text fallback seam**: when `RenderCapability` is `NonInteractive`, assert `render_turn` emits plain text (no ANSI cursor escapes, no `\r`). Fails if animation escapes leak into piped output (FR-011).
4. **Spinner frame advancement**: given an `AnimationState` + tick count, assert the correct frame index is selected (wraps correctly). Lightweight unit test.

**Why these are seam tests, not just impl tests**: they exercise the contract (markdown in→ANSI out; capability in→profile out) that future changes must preserve, independent of the render loop internals.

**Verdict**: PASS. Four named seam-level tests are specified; they will be implemented alongside the modules.

### Principle V — Simplicity and YAGNI Bound the Modularity Effort

**Abstractions added and their concrete justification**:
- `AnimationProfile` registry: justified by the near-term need for 5 distinct animations each with a fallback variant (Banner/Spinner/Caret/ToolLine/PromptCaret) — a data table is the simplest structure that avoids a central match and keeps fallback logic uniform. Concrete feature: FR-001/002/003/004/006.
- `markdown_to_ansi` pure function: justified by FR-003's finalize step; not speculative (single caller, immediate use).
- `RenderCapability` enum: justified by FR-007/FR-011/SC-004 requiring capability-based fallback; three variants (Full/Reduced/NonInteractive) — no finer granularity needed.

**Abstractions explicitly rejected** (YAGNI):
- No `Animator` trait with boxed dyn impls — animations differ only in data, so a data registry suffices (a trait hierarchy here would be speculative indirection with no second implementation).
- No per-element render trait objects — each element is rendered by a small named function called from the render loop; a `Renderable` trait would add indirection for one call site each.
- No syntax highlighting (syntect) — single-call-site, single-feature; code blocks get one accent color. Noted as a future enhancement, not this feature.

**Verdict**: PASS. Each abstraction names its concrete near-term feature; rejected alternatives recorded.

### Pre-design Gate Result: PASS (all five principles). No violations to track.

## Project Structure

### Documentation (this feature)

```text
specs/002-claude-code-cli-style/
├── plan.md              # This file
├── research.md          # Phase 0 output (R-001..R-007 decisions)
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   └── render-animation-seam.md
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT created here)
```

### Source Code (repository root)

```text
crates/joey-cli/
├── Cargo.toml                 # +pulldown-cmark workspace dep (promoted)
└── src/
    ├── repl.rs                # call banner_animated at startup; RenderOptions gains animation fields
    ├── render.rs              # render_turn refactored to tokio::select! + tick; re-exports submodule entries
    ├── animation.rs           # NEW — AnimationState, tick advancement, cursor-control repaint helpers (crossterm)
    ├── profile.rs             # NEW — AnimationProfile data registry, AnimationKind, reduced/fallback variants
    ├── markdown.rs            # NEW — markdown_to_ansi(&str, &Theme) -> String via pulldown-cmark (Pantera colors)
    └── capability.rs          # NEW — RenderCapability (Full/Reduced/NonInteractive) from IsTerminal + COLORTERM

tests/                         # workspace-level integration (if needed) or crate-level
└── (joey-cli unit tests live in crates/joey-cli/src/ alongside modules or a tests/ dir per crate convention)
```

**Structure Decision**: Single-crate change within `joey-cli`. Four new sibling modules under `src/` (animation/profile/markdown/capability) keep each concern in its own file (Principle III: minimal surface, each module one responsibility), wired together by the refactored `render.rs`. No new crate is warranted — all of this is CLI presentation, the existing crate's single responsibility (Principle I). `joey-tui`, `joey-agent-core`, `joey-core`, and all other crates are unchanged.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No violations. Table intentionally empty — all five principles PASS at pre-design gate. Re-evaluated after Phase 1 design in the Completion Report.
