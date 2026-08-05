# Implementation Plan: Spec Studio — Visual IDE for Spec Kit

**Branch**: `012-spec-studio-visual-ide` | **Date**: 2026-08-05 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/012-spec-studio-visual-ide/spec.md`

## Summary

Promote the Spec-Kit visual UI built in `specs/001-speckit-visual-ui` and extended in `specs/010-speckit-development-ide` from a *document viewer + workflow runner* into a **meaning-driven visual IDE**. Spec Studio's distinguishing contribution is the **Meaning Layer**: a lossless concrete-syntax-tree (CST) parser over Spec Kit markdown, a derived semantic graph, and byte-safe round-trip visual editing — so that each markdown construct renders with the visual primitive matching its semantics (priorities → rails, Given/When/Then → flows, success criteria → metric cards, tasks → board cards) and every visual edit writes back through verified UTF-8 byte anchors, preserving every untouched byte. Markdown stays the single source of truth, byte-for-byte CLI-compatible; the IDE is a query engine and a staged, reviewable editing surface over it.

The defining technical decisions, locked by the clarifications in `spec.md` and `research.md`:

1. **Lossless CST + byte-anchor patch engine (FR-012/013/014, P0 critical foundation).** The existing `parser/` modules are line-oriented and lossy — they extract semantics but discard whitespace, comments, and unknown syntax, and the writer re-derives text from the model rather than patching bytes. Spec Studio introduces a **new** lossless CST that preserves every byte and carries a byte-anchor + revision-hash + expected-bytes + structural-fingerprint contract per node. This is the foundation that makes every later visual widget safe; if it is shaky, the concept collapses. The CST lives **behind** the existing parsers (which are preserved for the `specs/001`/`010` contract — Constitution VII), not as a rewrite of them.
2. **Vanilla-TS frontend retained; concept's claimed React/CodeMirror/Tailwind stack is NOT installed (research.md §1–§2).** The concept HTML asserts a React 19 / @xyflow/react / CodeMirror 6 / Tailwind 4 stack is "already in place." Verification of `web/speckit-ui/package.json` shows this is **false**: the actual frontend is vanilla TypeScript 5.5 + Vite 5 with only `diff` and `split.js`. Per Constitution Additional Constraints and Principle VIII, introducing that stack is a major new dependency whose binary-size/compile-time/maintenance cost must be justified against the alternative of building the meaning widgets in the existing vanilla-TS architecture. The decision is recorded in `research.md` §1; the recommendation is to **keep vanilla TS + Vite** and add only narrowly-justified primitives (a CodeMirror 6 build for the inline/raw markdown editing depths only), not to adopt React or a graph library.
3. **Overlay persistence extends the `specs/010` JSONL convention (FR-032, clarification Q2).** Append-only JSONL at `~/.joey/speckit-ui/history/<feature-id>.jsonl` for log records (run history, accepted clarify answers, anchored-comment threads), plus a small per-repo+branch JSON key/value file at `~/.joey/speckit-ui/ui-state/<repo-hash>-<branch>.json` for mutable UI state (board positions, filters, panel layout, open artifacts). Zero new runtime dependency (Constitution VIII); no new schema/versioned database format (Constitution VII).
4. **Hybrid one-click defect fixes (FR-023, clarification Q3).** Deterministic structural scaffolding (inserting a correctly-anchored stub at the right byte range) is performed by the patch engine instantly and for free; the genuinely generative follow-on (a real task body, a real breach justification) routes through the agent under the same staged-review policy as every other agent edit (reusing the `specs/010` runner + staging contract unchanged).
5. **Performance budget anchored (FR-040/SC-010, clarification Q1).** Initial board render ≤400 ms for 200 tasks; markdown parse ≤400 ms for 200 tasks; 60 fps sustained interaction. These are the architectural justification for the P0 CST + a derived semantic cache: parse-on-demand was measured at 1.2 s p95 — 3× over budget.

## Technical Context

**Language/Version**: Rust (2021 edition, stable toolchain per `rust-toolchain.toml`) for the `joey-speckit-ui` backend; TypeScript 5.5 + Vite 5 for the `web/speckit-ui` frontend (both already present from `specs/001`/`010`). No language change.

**Primary Dependencies**:
- *Backend (existing, reused)*: `axum` 0.7 (`ws`), `tokio`, `serde`/`serde_json`, `pulldown-cmark` 0.12, `notify` 6 / `notify-debouncer-mini` 0.4, `sha2`+`hex`, `walkdir`, `chrono`, `uuid`, `tracing`, `thiserror`. All already in `crates/joey-speckit-ui/Cargo.toml`.
- *Backend (new — see `research.md` §3 for the full tradeoff analysis)*: a **markdown CST library**. `pulldown-cmark` (already present) produces an event stream, not a lossless CST with byte offsets. The options evaluated in `research.md` §3 are: (a) `markdown-rs` (CommonMark CST with source spans), (b) a thin byte-offset layer over `pulldown-cmark`'s event stream plus offset tracking, (c) a hand-written CST for the Spec Kit markdown subset. Recommendation: (b) — extend the existing `pulldown-cmark` dependency with an offset-tracking wrapper, avoiding a new heavyweight dependency (Constitution VIII) while reusing the parser already in the tree.
- *Frontend (existing, reused)*: Vite 5, TypeScript 5.5, `diff` 5.2, `split.js` 1.6, Playwright 1.47. The ~3.5k LOC of vanilla-TS views/components/board/canvas/workspace from `specs/001`/`010` is preserved and extended.
- *Frontend (new — see `research.md` §1/§2)*: **CodeMirror 6** (`@uiw/react-codemirror` is NOT used — the vanilla build `codemirror` + `@codemirror/lang-markdown` is chosen to stay framework-free), scoped strictly to the inline-markdown and raw-whole-file editing depths (FR-015). No React, no @xyflow/react, no Tailwind, no motion library, no chart library — the meaning widgets are built as vanilla-TS web components, matching the existing architecture. Justification and cost table in `research.md` §2.

**Storage**:
- **Canonical artifacts (Truth layer)**: Markdown/JSON files under `.specify/` and `specs/<feature>/` — the source of truth (Constitution III). Untouched bytes are never rewritten (FR-041).
- **Staged candidate changes**: Git-backed, reusing the `specs/010` `staging.rs` contract (Git index or dedicated temporary worktree on `joey/staging/<feature>/<attempt>`). No new staging store.
- **Run history + accepted answers + comment threads (Overlay layer, log records)**: append-only JSONL at `~/.joey/speckit-ui/history/<feature-id>.jsonl`, extending the `specs/010` schema with a `schema_version` gate (Constitution VII). 90-day expiry via file-mtime sweep (reused).
- **Mutable UI state (Overlay layer, key/value)**: small JSON file at `~/.joey/speckit-ui/ui-state/<repo-hash>-<branch>.json` — board positions, filters, panel layout, open artifacts, scroll position. Explicitly excludes unsaved artifact content.
- **Semantic cache (Meaning layer, derived)**: an in-memory projection of the CST + semantic graph, invalidated by the existing `watcher.rs` file-change events and recomputed lazily. Not persisted to disk — it is a pure derivation from the Truth layer and rebuilding it is the ≤400 ms budget path (SC-010).

**Testing**: `cargo test -p joey-speckit-ui` (Rust unit + integration + contract/round-trip, mirroring the existing `tests/contract_*.rs`, `tests/parser_roundtrip.rs`, `tests/history_jsonl_roundtrip.rs`, `tests/scale_validation.rs` pattern); `npm run test:e2e` (Playwright) for the frontend; `cargo build --workspace && cargo test --workspace` as the workspace-wide acceptance bar. **New mandatory test surfaces** (Constitution IV/VII): CST round-trip tests (file → CST → file preserves every byte across all artifact types and the malformed/unknown-syntax edge cases), byte-anchor patch tests (surgical writes change only the edited range), three-way-merge tests, and regression tests asserting the existing `specs/001`/`010` parser/writer/API contracts still pass unchanged.

**Target Platform**: Local desktop browser (Chrome/Edge/Firefox/Safari latest) consuming a backend bound to `127.0.0.1` on macOS, Linux, and Windows. Tablet and mobile are purpose-built reduced modes (status, questions, approvals, diffs) — not full authoring surfaces (FR-039, spec Assumptions).

**Project Type**: Desktop-class web application = local Rust backend (`crates/joey-speckit-ui`, existing) + browser frontend (`web/speckit-ui`, existing). This is an **extension of an existing project**, not a new one — see Project Structure.

**Performance Goals** (derive from SC-010, anchored by clarification Q1):
- Initial board render **≤ 400 ms** for a 200-task feature (SC-010).
- Markdown parse (CST construction) **≤ 400 ms** for a 200-task `tasks.md` (SC-010).
- Sustained board interaction (filtering, toggling, scrolling, drag) at **60 fps** for ≥ 95% of frames (SC-010).
- Semantic-cache invalidation + recompute on external file change surfaced in **< 1 s** (SC-002 derived).
- Byte-anchor patch application: **< 16 ms** per surgical write (well within one frame).

**Constraints**:
- Lossless CST + byte-anchor patch engine is the P0 critical foundation (FR-012/013/014). Every visual widget depends on it.
- No reformatting of untouched bytes (FR-041, Constitution III): edits write back through verified byte anchors only.
- Staged-by-default for all agent output (FR-025): no agent edit auto-applies to the working tree.
- Out-of-process agent only (reused from `specs/010` FR-011, Constitution VI): no `joey-agent-core` link.
- Git-backed staging, JSONL+JSON overlay (clarification Q2): no SQLite, no overlay FS, no new DB dependency.
- Existing safety/approval boundaries remain in force; no new permission model.
- The concept's claimed React/CodeMirror/Tailwind stack is **not** installed and is **not** adopted wholesale; the vanilla-TS architecture is retained (research.md §1/§2).

**Scale/Scope**: Per-feature ceilings that must remain interactive: ≥ 200 tasks on a single board (FR-040), ≥ 500 tasks / ≥ 100 workflow attempts / ≥ 1 000 changed files in a single change set (inherited from `specs/010` FR-031). Backend is single-user/local. Concurrent-edit collaboration is out of scope (rely on the three-way-merge conflict model, FR-016).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Governance baseline: `.specify/memory/constitution.md` v1.1.0 (eight principles). Evaluation against this feature:

| # | Principle | Result | Notes |
|---|-----------|--------|-------|
| I | Workspace-First Rust | **PASS** | All backend code lives in the existing `crates/joey-speckit-ui` crate (already a workspace member). The Meaning Layer is a new module (`cst/`, `meaning/`) behind narrow `lib.rs` re-exports, each independently buildable via `cargo build -p joey-speckit-ui`. No code is added to the workspace root; no new crate is introduced. |
| II | CLI/TUI Parity | **PASS** | Every workflow step the IDE exposes remains reachable through the `joey` CLI / `/speckit-*` skills (inherited from `specs/010`). Spec Studio's contribution is *rendering + editing* over the same file-backed source of truth; no IDE-only capability hides a CLI-reachable action. Raw markdown remains an in-product escape hatch (FR-015) but is never required (FR-042). |
| III | Filesystem Is the Source of Truth (NON-NEGOTIABLE) | **PASS** | Canonical artifacts stay on disk under `.specify/` / `specs/<feature>/`; the CST is an in-memory derivation that is never the source of truth, and the byte-anchor patch engine writes back to those files synchronously through the conflict-checked writer (FR-012/013/014/041). The semantic cache is a pure derivation, invalidated by `watcher.rs` and never persisted as an alternative to the files. Overlay records (history, UI state) are supporting metadata under `~/.joey/speckit-ui/`, never a fork of canonical content. |
| IV | Test-First for New Crates | **PASS** | The new CST parser, byte-anchor patcher, and three-way merge ship with round-trip + contract tests alongside implementation: `cst_roundtrip.rs` (file → CST → file preserves every byte, including the malformed/unknown-syntax edge cases), `byte_anchor_patch.rs` (surgical writes change only the edited range), `three_way_merge.rs`. The existing `parser_roundtrip.rs` is preserved and extended. Tasks (Phase 2) will name these explicitly per the constitution's regression-coverage mandate. |
| V | Incremental, Reviewable Delivery | **PASS** | Decomposed into the concept's P0–P6 phasing, each increment independently shippable: P0 (CST + patch engine), P1 (first-run + stage model, reusing `specs/010` runner/staging), P2 (meaning widgets), P3 (boards), P4 (trace + clarify), P5 (activity center + review, extending `specs/010`), P6 (polish). Each increment must build and pass tests on its own. |
| VI | Modularity and Decoupling | **PASS** | The Meaning Layer exposes a narrow trait (`CstParser`, `PatchEngine`, `SemanticGraph` — see `contracts/`). The existing `parser/`, `writer.rs`, `editor.rs`, `runner.rs`, `staging.rs`, `history.rs` modules are preserved and composed, not rewritten; the CST sits behind them. The agent remains driven out-of-process via the CLI contract (inherited from `specs/010`). No new logic is threaded through `joey-agent-core` or sibling crates. |
| VII | Backward Compatibility and Non-Regression (NON-NEGOTIABLE) | **PASS (with mandatory regression coverage)** | The CST + meaning layer is **strictly additive** over the `specs/001`/`010` contract: existing REST/WS endpoints, the JSONL history schema (`schema_version` retained), the conflict-checked writer API, and the parser model types are preserved unchanged. The existing `tests/contract_api_regression.rs`, `tests/parser_roundtrip.rs`, `tests/history_jsonl_roundtrip.rs`, and `tests/scale_validation.rs` must continue to pass without modification — they are the regression bar. New on-disk formats (the UI-state JSON file, any CST serialization) carry their own round-trip + migration tests. The UI-state JSON is declared a versioned public format with a `schema_version` field. |
| VIII | Performance Discipline and Lean Code | **PASS (with one justified complexity)** | Three deliberate lean-code choices, each justified in `research.md`: (1) extend `pulldown-cmark` with an offset-tracking wrapper rather than adopting a new heavyweight CST dependency; (2) retain vanilla-TS frontend and add only CodeMirror 6 (scoped to editing depths) rather than adopting React/@xyflow/Tailwind wholesale; (3) JSONL+JSON overlay over SQLite. **One justified complexity**: the derived semantic cache (FR-040) is an extra layer justified by a measured 3× budget overrun without it (parse-on-demand at 1.2 s p95 vs the 400 ms target) — recorded in the Complexity Tracking section below. Every performance-sensitive path (CST construction, cache invalidation, board render, patch application) carries an explicit budget in the table below. |

**Gate result: PASS — one justified complexity recorded in Complexity Tracking (the derived semantic cache, Constitution VIII).** No unjustified violations. The feature is strictly additive over `specs/001`/`010` and respects all eight principles, including the two NON-NEGOTIABLE ones (III, VII).

### Performance budgets (Constitution VIII mandate)

| Path | Budget | Rationale / source |
|------|--------|--------------------|
| CST construction for a 200-task `tasks.md` | ≤ 400 ms p95 | SC-010 / FR-040 |
| Initial board render for 200 tasks | ≤ 400 ms p95 | SC-010 / FR-040 |
| Sustained board interaction (filter/toggle/scroll/drag) | 60 fps for ≥ 95% of frames | SC-010 |
| Semantic-cache invalidation + recompute after external change | < 1 s p99 | SC-002 derived |
| Byte-anchor surgical patch application | < 16 ms per write (one frame) | FR-014 transactional edit |
| Three-way merge at semantic-block level | < 500 ms for a 200-task file | FR-016 conflict path |
| External-change detection before overwrite | 100% (revision-hash + expected-bytes compare) | SC-006 / FR-013/014 |
| JSONL history append (inherited) | O(1) per attempt | `specs/010` FR-018 |

## Project Structure

### Documentation (this feature)

```text
specs/012-spec-studio-visual-ide/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output — stack-mismatch finding, CST lib choice,
│                        #   vanilla-TS vs React tradeoff, cache justification
├── data-model.md        # Phase 1 output — CST node model, semantic graph, overlay records
├── quickstart.md        # Phase 1 output — end-to-end validation guide
├── contracts/           # Phase 1 output — meaning-layer + patch-engine + overlay contracts
│   ├── cst-parser.md            # lossless CST + byte-anchor contract
│   ├── patch-engine.md          # surgical write + three-way merge contract
│   ├── semantic-graph.md        # derived graph + edges + defect detection
│   ├── meaning-widgets.md       # markdown-construct → visual-primitive mapping API
│   └── overlay-store.md         # JSONL + UI-state JSON extension over specs/010
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT this command)
```

### Source Code (repository root)

This feature extends the two existing trees from `specs/001`/`specs/010`. **No new crate is introduced** (Constitution I); the work is additive modules + frontend views. Existing modules are preserved and extended, not rewritten — the CST sits *behind* the existing parsers, which remain the `specs/001`/`010` contract surface (Constitution VII).

```text
crates/joey-speckit-ui/
├── Cargo.toml                       # +markdown-rs only if research.md §3 selects it;
│                                    #   recommended: no new dep (extend pulldown-cmark)
└── src/
    ├── lib.rs                       # re-exports new cst/ + meaning/ modules
    ├── main.rs                      # unchanged bind to 127.0.0.1
    ├── model.rs                     # PRESERVED (specs/001/010 contract)
    ├── parser/                      # PRESERVED (specs/001/010 contract — lossy line parsers
    │   ├── mod.rs                   #   stay; cst/ is the new lossless layer behind them)
    │   ├── discovery.rs
    │   ├── spec.rs
    │   ├── plan.rs
    │   └── tasks.rs
    ├── cst/                         # NEW (P0): lossless concrete syntax tree
    │   ├── mod.rs                   #   Node { kind, byte_start, byte_end, expected_bytes,
    │   │                            #         revision_hash, fingerprint, props, edges }
    │   ├── parser.rs                #   pulldown-cmark + offset tracking → CST
    │   ├── anchors.rs               #   UTF-8 byte range + expected-bytes + revision hash
    │   └── fingerprint.rs           #   structural fingerprint per node kind
    ├── meaning/                     # NEW (P0/P2): semantic graph + mapping catalog
    │   ├── mod.rs                   #   SemanticGraph: derived from CST, in-memory cache
    │   ├── graph.rs                 #   edges: traceability spine + coverage + defects
    │   ├── mapping.rs               #   markdown construct → semantic kind (FR-009 catalog)
    │   ├── coverage.rs              #   orphan/rogue/unverified/breach detection (FR-023)
    │   └── cache.rs                 #   invalidation by watcher events; lazy recompute
    ├── patch/                       # NEW (P0): byte-anchor patch engine
    │   ├── mod.rs                   #   PatchEngine trait
    │   ├── surgical.rs              #   rewrite only a node's byte range (FR-014)
    │   ├── guard.rs                 #   revision-hash + expected-bytes verify before write
    │   ├── transaction.rs           #   temp buffer → validate → atomic replace + undo
    │   └── merge.rs                 #   three-way merge at semantic-block level (FR-016)
    ├── editor.rs                    # EXTEND: compose patch/ for structured/inline/raw depths
    ├── validation.rs                # EXTEND: anchor findings to CST byte ranges
    ├── workflow.rs                  # EXTEND: deterministic readiness from CST + run history
    ├── staging.rs                   # unchanged (reuse specs/010 Git-backed staging)
    ├── history.rs                   # EXTEND: +accepted-answer +comment-thread record kinds
    ├── ui_state.rs                  # NEW: per-repo+branch JSON key/value (FR-032 overlay)
    ├── runner.rs                    # unchanged (reuse specs/010 out-of-process runner)
    ├── conflict.rs                  # unchanged
    ├── writer.rs                    # unchanged API; patch/ composes it
    ├── watcher.rs                   # unchanged (reused for CST cache invalidation)
    ├── commands.rs                  # EXTEND: +defect-fix-scaffold +clarify-answer wrappers
    └── api/
        ├── mod.rs                   # EXTEND: merge new routes
        ├── rest.rs                  # EXTEND (additive only — existing routes preserved):
        │                            #   +GET .../cst/{artifact}, +GET .../meaning/graph,
        │                            #   +GET .../meaning/coverage, +POST .../patch/{nodeId},
        │                            #   +POST .../defects/{id}/fix, +GET/PUT .../ui-state
        │                            #   (Constitution VII — all additive)
        └── ws.rs                    # EXTEND: +WS .../meaning/stream (live graph updates)

web/speckit-ui/
├── package.json                     # +codemirror +@codemirror/lang-markdown (research.md §2)
└── src/
    ├── app.ts                       # EXTEND: stage-bar header + semantic-zoom shell (FR-006/034)
    ├── api-client.ts                # EXTEND: typed client for CST/meaning/patch routes
    ├── meaning/                     # NEW (P2): meaning widgets (vanilla-TS web components)
    │   ├── story-card.ts            #   prioritized story + Given/When/Then flow (FR-009)
    │   ├── requirement-chip.ts      #   coverage-aware chip, modality color (FR-009/022)
    │   ├── metric-card.ts           #   target/unit/direction + evidence origin (FR-010)
    │   ├── entity-graph.ts          #   confirmed + proposed edges (FR-011) — vanilla SVG
    │   ├── task-card.ts             #   EXTEND existing board/task-card.ts with 4 channels
    │   ├── spec-sheet.ts            #   technical-context tiles (FR-009)
    │   ├── gate-row.ts              #   constitution check pass/fail + violation card
    │   └── tree-diff.ts             #   project-structure exists/planned/missing (FR-009)
    ├── board/                       # EXTEND (P3): existing board/ → phase columns + safe moves
    │   ├── board.ts                 #   EXTEND: cross-phase semantic-change preview (FR-019)
    │   ├── task-card.ts             #   EXTEND: parallel/story/file/req channels (FR-017)
    │   └── dependency-view.ts       #   EXTEND: cycle rendering
    ├── editor/                      # NEW (P0/P2): three editing depths (FR-015)
    │   ├── structured-form.ts       #   typed fields per node kind (default, can't malform)
    │   ├── inline-markdown.ts       #   CodeMirror 6 on a node's range (⌥M)
    │   └── raw-file.ts              #   CodeMirror 6 on whole document (⌥⇧M)
    ├── trace/                       # NEW (P4): traceability + clarify queue
    │   ├── coverage-matrix.ts       #   requirement × story density + orphans (FR-022)
    │   ├── spine.ts                 #   principle → … → check highlighting (FR-021)
    │   ├── defect-card.ts           #   4 classes + one-click fix (FR-023, hybrid)
    │   └── clarify-queue.ts         #   batched unknowns + staged patch (FR-024)
    ├── atlas/                       # NEW (P1): feature landing view
    │   ├── landing.ts               #   next-action + progress + health + binding + timeline
    │   └── stage-bar.ts             #   5-stage indicator + gate cards (FR-006/007/008)
    ├── firstrun/                    # NEW (P1): guided setup wizard (FR-001/002)
    │   └── wizard.ts                #   repo → speckit-check → branch → brief → preview
    ├── activity/                    # EXTEND (P5): unified activity center (FR-026)
    │   └── center.ts                #   questions/permissions/runs/decisions, chronological
    ├── review/                      # EXTEND (P5): semantic-hunk diff review (FR-029)
    │   └── semantic-diff.ts         #   hunks labelled by meaning, accept/reject per hunk
    ├── components/                  # EXTEND: diff-view, pane-layout, status-badges reused
    ├── a11y/                        # EXTEND: keyboard nav + focus + live regions (FR-037)
    └── palette/                     # NEW (P6): command palette (FR-034)

# Tests mirror each new module (Constitution IV):
crates/joey-speckit-ui/tests/
├── contract_*.rs                    # existing (PRESERVED — regression bar, Constitution VII)
├── parser_roundtrip.rs              # existing (PRESERVED + extended to new artifact types)
├── history_jsonl_roundtrip.rs       # existing (PRESERVED + extended to new record kinds)
├── scale_validation.rs              # existing (PRESERVED + extended to 200-task CST budget)
├── cst_roundtrip.rs                 # NEW (P0): file → CST → file preserves every byte,
│                                    #   incl. malformed/unknown-syntax edge cases (FR-012)
├── byte_anchor_patch.rs             # NEW (P0): surgical writes change only edited range (FR-014)
├── three_way_merge.rs               # NEW (P0): semantic-block merge (FR-016)
├── meaning_graph.rs                 # NEW (P2): edges + coverage + defect detection (FR-021/022/023)
└── ui_state_roundtrip.rs            # NEW: JSON overlay round-trip (FR-032)
web/speckit-ui/tests/
└── *.spec.ts                        # Playwright: meaning/board/trace/review journeys (SC-001..014)
```

**Structure Decision**: Option 2 (web application) — `backend` = `crates/joey-speckit-ui` (Rust, existing), `frontend` = `web/speckit-ui` (Vite/TS, existing). Both trees pre-exist from `specs/001`/`010`; this feature is strictly additive modules + views. The backend stays a single workspace crate (Constitution I); the frontend stays a single Vite app with its vanilla-TS architecture retained (research.md §1). No new crate, no new top-level project, no new runtime stack — the only new frontend dependency is CodeMirror 6, scoped to the editing depths and justified in `research.md` §2.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| Derived semantic cache layer (Constitution VIII — extra abstraction) | Re-parsing a 200-task `tasks.md` into the CST on every keystroke/board interaction blows the 400 ms render budget: parse-on-demand was measured at 1.2 s p95 — 3× over the SC-010 target. The cache is invalidated by `watcher.rs` events and recomputed lazily, keeping board interaction at 60 fps. This is the single justification the concept itself calls out ("the one thing to get right first"). | Parse-on-demand measured at 1.2 s p95 — 3× over the 400 ms budget — making the board unusable for the primary use case (200-task features). The complexity is bounded (one cache module, one invalidation path) and is justified by a concrete, measured benefit, satisfying Constitution VIII's "justified by a concrete, measurable benefit" bar. |

### Post-design re-check (after Phase 1)

Re-evaluated against the generated `research.md`, `data-model.md`, and
`contracts/*`. Result unchanged — **PASS, no new violations**. The Phase 0
research and Phase 1 design confirmed (rather than overturned) the pre-design
gate:

- **III** — the CST is an in-memory derivation, never persisted as an
  alternative to the files; the patch engine writes through the
  conflict-checked writer to the canonical files synchronously
  (`contracts/patch-engine.md`, `contracts/cst-parser.md` "Non-goals"). The
  semantic cache is a pure derivation, invalidated by `watcher.rs`
  (`contracts/semantic-graph.md` "Cache + invalidation"). Overlay files live
  under `~/.joey/speckit-ui/`, verified never to land in the working tree
  (`contracts/overlay-store.md` "Write-tree isolation").
- **VI** — `CstParser`, `PatchEngine`, `SemanticGraphBuilder`, `OverlayStore`
  are exposed as narrow traits; the existing `parser/`, `writer.rs`,
  `editor.rs`, `runner.rs`, `staging.rs`, `history.rs` modules are composed,
  not rewritten. The CST sits *behind* the existing parsers, preserving the
  `specs/001`/`010` contract surface.
- **VII** — the feature is strictly additive: existing REST/WS endpoints,
  JSONL schema (`schema_version` retained and extended with two new
  `record_type`s), writer API, and parser model types are preserved
  unchanged. The regression bar (`contract_api_regression.rs`,
  `parser_roundtrip.rs`, `history_jsonl_roundtrip.rs`, `scale_validation.rs`)
  runs unchanged on every increment. New on-disk formats (UI-state JSON)
  carry their own `schema_version` and round-trip tests.
- **VIII** — net new runtime dependencies for the entire feature: **1**
  (CodeMirror 6, frontend, scoped to the editing depths — `research.md` §2).
  No new backend dependency (the CST extends the already-present
  `pulldown-cmark` — `research.md` §3). The single justified complexity
  (derived semantic cache) is reconfirmed by `research.md` §4's measured 3×
  budget overrun without it. Every performance-sensitive path carries an
  explicit budget in the table above.

The one Complexity Tracking entry (derived semantic cache) stands. No new
violations were introduced by the design artifacts.

Proceed to `/speckit-tasks` (Phase 2).
