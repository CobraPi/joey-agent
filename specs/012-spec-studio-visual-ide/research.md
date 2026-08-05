# Research: Spec Studio — Visual IDE for Spec Kit

**Branch**: `012-spec-studio-visual-ide` | **Date**: 2026-08-05 | **Plan**: [plan.md](./plan.md)

This document resolves every technical unknown and dependency choice raised in
`plan.md`'s Technical Context and Constitution Check. Each section records the
**Decision**, the **Rationale**, and the **Alternatives considered** with the
cost/benefit tradeoff the Constitution (especially Principles VII and VIII)
demands.

The single most important finding driving this research: **the concept HTML's
claim that a React 19 / @xyflow/react / CodeMirror 6 / Tailwind 4 stack is
"already in place" is factually incorrect for this repository.** The actual
`web/speckit-ui/package.json` is vanilla TypeScript 5.5 + Vite 5 with only
`diff` and `split.js` as runtime deps. This reframes the entire feature from
"composition work over an installed stack" (the concept's framing) into "a
dependency-introduction decision that the Constitution requires be justified
against the existing architecture." Sections §1 and §2 address this directly.

---

## §1 — Frontend architecture: retain vanilla TS vs adopt the concept's React stack

**Decision**: Retain the existing **vanilla TypeScript + Vite** architecture.
Build the meaning widgets as framework-free web components (the same pattern
the existing `board/`, `canvas/`, `workspace/`, `components/` modules already
use). Do **not** adopt React, @xyflow/react, Tailwind, motion, or a chart
library.

**Rationale**:
- The existing `web/speckit-ui/src` is ~3,500 LOC of vanilla-TS views, board,
  canvas, workspace, and component modules from `specs/001`/`010`. Adopting
  React would require rewriting all of it, which is a massive regression risk
  (Constitution VII) and a huge scope expansion for zero user-visible benefit
  in this feature (the meaning widgets are the deliverable, not a framework
  migration).
- The meaning widgets (story card, requirement chip, metric card, entity
  graph, task card, spec sheet, gate row, tree diff) are all small, isolated,
  presentational components. They do not need a virtual DOM or a reactive
  state tree — they project from a typed semantic graph the backend already
  computes. Vanilla TS web components with a tiny store (the existing
  `api-client.ts` + `nanostores`-free pattern) are sufficient and idiomatic
  for this codebase.
- Constitution Additional Constraints: "No new runtime dependency (JS
  framework, GUI toolkit, etc.) is added without recording the alternatives
  considered in `research.md` for the feature, including the dependency's
  impact on binary size and compile time." React 19 + ReactDOM + a reactive
  graph lib + Tailwind's JIT pipeline would roughly **double the frontend
  bundle** and introduce a large transitive surface and a separate build
  paradigm — unjustified when the existing architecture already serves the
  need.

**Alternatives considered**:

| Option | Benefit | Cost / risk | Verdict |
|--------|---------|-------------|---------|
| **Adopt React 19 + @xyflow/react + Tailwind 4 + motion + CodeMirror (concept's stack)** | Matches the concept doc's framing; rich ecosystem for graph editing | ~3,500 LOC rewrite of existing vanilla-TS views (Constitution VII regression risk); doubles bundle; new build pipeline; the concept's claimed "no new runtime dependency required" is false | Rejected — the concept's premise here is factually wrong for this repo |
| Adopt React alone, keep graph as vanilla SVG | Smaller blast radius | Still a partial rewrite + a mixed-paradigm frontend (React + vanilla islands) that is harder to maintain than either pure option | Rejected — worst of both |
| **Retain vanilla TS, add only CodeMirror 6 (chosen)** | Zero rewrite; existing modules preserved and extended; meaning widgets are small web components matching the existing pattern | CodeMirror 6 is a real new dep, but it is narrowly scoped to the inline-markdown and raw-file editing depths (FR-015) where a purpose-built editor is genuinely needed | **Chosen** — see §2 |

**Cost note (Constitution VIII)**: Retaining vanilla TS adds **zero** new
runtime dependencies to the rendering path. The only new frontend dep is
CodeMirror 6, scoped and justified in §2.

---

## §2 — Code editor for the inline-markdown and raw-whole-file editing depths

**Decision**: Add **CodeMirror 6** via the framework-free `codemirror` +
`@codemirror/lang-markdown` packages (NOT `@uiw/react-codemirror`, which would
pull React — rejected per §1). Scope it strictly to the two non-structured
editing depths in FR-015: inline markdown on a node's range (⌥M) and the raw
whole file (⌥⇧M).

**Rationale**:
- FR-015 mandates three editing depths always be available, including inline
  markdown on a node's range and the raw whole file. A purpose-built code
  editor with markdown syntax highlighting, line tracking, and undo/redo is a
  genuine need — the structured form (depth 1) cannot substitute, and a raw
  `<textarea>` gives poor UX for markdown editing (no highlighting, no line
  anchoring back to the CST).
- CodeMirror 6 is the leanest mainstream option: it is framework-free (unlike
  `@uiw/react-codemirror`), modular (you import only the extensions you use),
  and has first-class TypeScript support matching the existing frontend.
- Scope is tight: two editing surfaces. The meaning widgets themselves do not
  use CodeMirror — they are vanilla web components. This keeps the dependency
  surface small and the cost predictable.

**Alternatives considered**:

| Option | Bundle impact (est.) | Markdown highlight | Line/byte anchoring | Framework-free | Verdict |
|--------|----------------------|--------------------|---------------------|----------------|---------|
| **CodeMirror 6 (`codemirror` + `@codemirror/lang-markdown`)** | ~150 KB gzipped (tree-shaken to needed extensions) | First-class via lang-markdown | Yes — line/offset API maps to CST byte ranges | Yes | **Chosen** |
| Monaco Editor | ~1.5 MB+ | Via extension | Yes | Yes | Rejected — far too heavy for a markdown-only editing surface (Constitution VIII) |
| raw `<textarea>` + a lightweight highlighter (e.g. `highlight.js` overlay) | ~30 KB | Partial, external overlay | Manual, error-prone | Yes | Rejected — poor UX, fragile anchoring back to the CST |
| `@uiw/react-codemirror` | ~150 KB + React | First-class | Yes | **No** (pulls React) | Rejected — violates §1's no-React decision |

**Cost note (Constitution VIII)**: ~150 KB gzipped, scoped to two editing
surfaces, tree-shaken to the needed extensions. This is the single new
frontend runtime dependency in the entire feature, and it is justified by a
concrete need (FR-015's editing depths). Recorded against the alternatives
above.

**Measured cost (T111, 2026-08-05)**:

| Metric | specs/010 baseline (no CodeMirror) | With CodeMirror 6 | Delta | Notes |
|--------|-------------------------------------|--------------------|-------|-------|
| Production bundle (raw) | 59.81 KB | 554.38 KB | +494.57 KB | Vite 5 production build, tree-shaken to `@codemirror/state` + `@codemirror/view` + `@codemirror/commands` + `@codemirror/lang-markdown` (the four sub-packages the two editor depths import). |
| Production bundle (gzipped) | 15.56 KB | 187.70 KB | **+172.14 KB gzipped** | 15% over the §2 pre-build estimate of ~150 KB. The overrun is from `@codemirror/view` (1.2 MB unpacked) and `@codemirror/state` (448 KB unpacked) — both are mandatory for a functioning editor (state holds the document model; view renders it). |
| `cargo build -p joey-speckit-ui` (clean, dev profile) | 11.94 s | 11.94 s | **0 s** | CodeMirror is a frontend-only dependency; it adds zero Rust compile time. The CST extends the already-present `pulldown-cmark` (research.md §3), so the backend compile time is unchanged. |
| node_modules footprint | — | 3.1 MB (`@codemirror/*`) + 44 KB (`codemirror` umbrella) | +3.1 MB | Dev-only; not shipped to users. The umbrella `codemirror` package (44 KB) is declared in `package.json` but the code imports the sub-packages directly, so the umbrella is unused at runtime. |

**Reconciliation with the §2 estimate**: the pre-build estimate of "~150 KB
gzipped" was 15% under the measured 172 KB. The estimate assumed aggressive
tree-shaking to "needed extensions"; in practice, CodeMirror 6's `@codemirror/view`
(1.2 MB unpacked) contributes the bulk of the delta because a functioning
editor requires the full view + state + commands stack, not just a highlighter
overlay. This does not change the §2 decision (CodeMirror 6 remains the leanest
framework-free option — Monaco is ~1.5 MB+, and a raw textarea lacks the
byte-anchoring FR-015 requires), but the updated number is recorded here per
Constitution VIII's weight-must-be-recorded mandate.

---

## §3 — Markdown CST library for the lossless parser (P0)

**Decision**: Build the lossless CST on top of the **already-present
`pulldown-cmark` 0.12** by adding an **offset-tracking wrapper** that records
UTF-8 byte ranges for each parsed construct. Do **not** introduce a new
heavyweight CST dependency (`markdown-rs` or `comrak`).

**Rationale**:
- The hard requirement (FR-012) is a *lossless* parse that preserves every
  byte (whitespace, comments, unknown extensions, untouched ranges). The
  existing `parser/` modules are line-oriented and lossy — they drop bytes
  the model doesn't care about. The CST layer must not repeat that.
- `pulldown-cmark` is already a workspace dependency and is CommonMark-spec
  compliant. It emits a streaming event model (`Event::Start`, `Event::Text`,
  `Event::End`, …). Crucially, modern `pulldown-cmark` exposes source spans
  via the `OffsetIter` / `text()` APIs, so byte offsets are recoverable
  **without a new dependency**.
- The Spec Kit markdown subset is small and regular (headings, bullet lists
  with `**FR-NNN**:` patterns, `### User Story N (Priority: Px)`, code fences,
  GWT blocks, tables). Mapping these to semantic CST nodes with byte anchors
  is a bounded wrapper problem, not a from-scratch parser problem.
- Constitution VIII: a new CST library (`markdown-rs` ~large, or `comrak`
  which pulls C bindings via `libcmark-sys`) would each be a heavy addition
  for a subset we can already parse. The wrapper keeps the dependency surface
  flat.

**The CST construction contract** (detailed in `data-model.md` and
`contracts/cst-parser.md`):
1. Run `pulldown-cmark` with offset tracking enabled over the file's UTF-8
   bytes.
2. Walk the event stream, building a `CstNode` per construct with:
   `{ kind, byte_start, byte_end, expected_bytes, revision_hash, fingerprint, props, child_ranges }`.
3. Any byte range not consumed by a recognized construct becomes a
   `Raw` node that preserves its bytes verbatim — this is what makes the
   parse lossless and is what `cst_roundtrip.rs` will assert.
4. The semantic graph (`meaning/graph.rs`) is derived from the CST by
   pattern-matching node `kind` + `props` (e.g. a list-item whose text matches
   `^\s*-\s*\*\*FR-\d+\*\*` becomes a `Requirement` semantic node pointing at
   the CST node's byte range).

**Alternatives considered**:

| Option | New dependency | Lossless | Byte offsets | Spec compliance | Verdict |
|--------|----------------|----------|--------------|-----------------|---------|
| **`pulldown-cmark` + offset-tracking wrapper (chosen)** | None (already present) | Yes (Raw nodes for unrecognized ranges) | Yes via OffsetIter | CommonMark | **Chosen** |
| `markdown-rs` | New, ~large pure-Rust | Yes | Yes | CommonMark | Rejected — duplicates a parser already in the tree for a subset we can already handle; violates VIII's lean bar |
| `comrak` (with sourcepos) | New + C bindings (`libcmark-sys`) | Yes | Yes (sourcepos) | GFM + CommonMark | Rejected — C bindings complicate the single-binary build (the project deliberately uses `rusqlite` bundled to avoid system deps); GFM extras beyond what pulldown-cmark already covers aren't needed |
| Hand-written CST for the Spec Kit subset | None | Yes (by construction) | Yes (by construction) | Custom (risk of drift from CommonMark) | Rejected — higher maintenance burden and reimplementation risk; pulldown-cmark already handles the CommonMark edge cases (nested lists, setext, reference links) for free |

**Cost note (Constitution VIII)**: Zero new backend dependency. The wrapper is
~300–500 LOC of bounded, well-tested code with round-trip tests
(`cst_roundtrip.rs`) as the safety net.

---

## §4 — Derived semantic cache: justification and invalidation strategy

**Decision**: Maintain an **in-memory derived semantic graph** per open
feature, invalidated by `watcher.rs` file-change events and recomputed lazily
on next access. This is the one justified complexity recorded in
`plan.md`'s Complexity Tracking section.

**Rationale**:
- SC-010 / FR-040 require a 200-task board to render in ≤400 ms and a 200-task
  parse to complete in ≤400 ms, with 60 fps sustained interaction. The board
  is a live, interactive surface (filtering, toggling, dragging) — re-running
  the CST parse + semantic derivation on every interaction frame is
  infeasible.
- The concept document measured parse-on-demand at **1.2 s p95** for a
  200-task file — 3× over the 400 ms budget. With the cache, the parse
  happens once per file change (well within budget for an async watcher
  event), and subsequent board interactions read the in-memory graph
  (microseconds).
- Invalidation is already solved by the existing `watcher.rs` from
  `specs/001`/`010`, which fires on feature-directory file changes. The cache
  hooks those events: any change to a `.md` file drops the affected
  artifact's CST + the feature's semantic graph, and the next read
  re-derives. External edits thus propagate in <1 s (the budget in
  `plan.md`'s performance table).
- The cache is **not persisted** — it is a pure derivation from the Truth
  layer (Constitution III). Losing it costs a one-time reparse, never
  authored work.

**Alternatives considered**:

| Option | Interaction cost | Parse cost | Invalid. complexity | Verdict |
|--------|------------------|------------|---------------------|---------|
| **In-memory derived graph + watcher invalidation (chosen)** | Microseconds (read memory) | Once per file change (async) | Low (hook existing watcher) | **Chosen** |
| Parse-on-demand at every interaction | 1.2 s p95 × every frame | Per interaction | None | Rejected — 3× over budget, breaks SC-010 |
| Persisted cache to disk | Microseconds + disk load | Once per change | High (cache coherence, now a second source of truth — risks Constitution III) | Rejected — reintroduces a source-of-truth fork risk for no gain (in-memory reparse is already <400 ms) |

**Cost note (Constitution VIII)**: One extra module (`meaning/cache.rs`), one
invalidation path (existing watcher). The complexity is bounded and justified
by a measured 3× budget overrun without it — satisfying VIII's "concrete,
measurable benefit" bar.

---

## §5 — Overlay persistence: JSONL + JSON over SQLite (clarification Q2)

**Decision**: Extend the existing `~/.joey/speckit-ui/` data directory
convention from `specs/010`. Use **append-only JSONL** for log records and a
**small per-repo+branch JSON key/value file** for mutable UI state. No SQLite.

**Rationale**: This is the recorded answer to clarification Q2 in `spec.md`.
The reasoning is preserved here for completeness:

- `specs/010` already chose append-only JSONL for run history
  (`~/.joey/speckit-ui/history/<feature-id>.jsonl`) and explicitly rejected
  SQLite as "unjustified for a sequential append-and-read log." Spec Studio's
  Overlay adds *mutable* UI state (board positions, filters, panel layout,
  open artifacts) which is a poor fit for append-only JSONL — but a poor fit
  for SQLite too, since it is a single small key/value blob per repo+branch,
  not a queryable multi-row dataset.
- The natural split: JSONL for the append-log records (history, accepted
  clarify answers, anchored-comment threads — all genuinely append-only),
  JSON for the mutable blob (rewritten atomically on each change, which is
  rare — only on layout/filter/selection changes).
- Zero new dependencies (Constitution VIII); no new schema/versioned database
  format alongside the existing JSONL convention (Constitution VII). Both
  files carry a `schema_version` field so a future breaking change is gated
  by the MAJOR-bump + migration rule.

**Alternatives considered**:

| Option | New dep | Fits log records | Fits mutable UI state | Schema/version surface | Verdict |
|--------|---------|------------------|----------------------|------------------------|---------|
| **JSONL + JSON (chosen)** | None | Yes (append-only) | Yes (atomic rewrite of small blob) | Extends existing JSONL `schema_version`; adds one JSON `schema_version` | **Chosen** |
| Single SQLite DB | New (`rusqlite` already in workspace, but a new schema in `joey-speckit-ui`) | Yes | Yes | New schema/versioned format in this crate; requires justifying against the `specs/010` JSONL decision | Rejected — reintroduces a second persistence paradigm and the exact schema/format concern `specs/010` rejected |
| JSONL for everything (rewrite whole file for mutable state) | None | Yes | Poor (O(n) rewrite per layout change) | One schema | Rejected — wasteful for the mutable portion |

**Cost note (Constitution VIII)**: Zero new runtime dependency.

---

## §6 — Three-way merge at semantic-block level

**Decision**: When an external on-disk change conflicts with a developer's
unsaved edits (FR-016), perform a **three-way merge at the semantic-block
(CST node) level**, not a line-level textual merge.

**Rationale**:
- The CST already carves the file into semantic nodes with byte ranges and
  structural fingerprints. A three-way merge at this granularity produces
  meaningful conflict units ("User Story 2's acceptance scenario changed in
  two ways") rather than textual noise ("line 47 changed").
- The base, current-file, and proposed-patch CSTs are all available; the
  merge walks the node lists, pairing nodes by `fingerprint` (structural
  identity) and flagging nodes that differ in `expected_bytes` on either side
  as conflicts. Unchanged nodes on both sides auto-merge; only genuinely
  conflicting nodes surface to the developer.
- This reuses the CST infrastructure (no new parser) and is testable in
  isolation (`three_way_merge.rs`).

**Alternatives considered**:

| Option | Granularity | Meaningful conflicts | New infra | Verdict |
|--------|-------------|----------------------|-----------|---------|
| **CST-node-level three-way merge (chosen)** | Semantic block | Yes ("FR-016 conflicts") | Reuses CST | **Chosen** |
| Line-level textual merge (e.g. `diff3` style) | Line | No (line-noise conflicts) | Could use the existing `diff` crate | Rejected — the concept explicitly calls for semantic-block merge; line-level is the anti-pattern |
| Whole-file reject + reload (the `specs/001` model) | File | N/A (no merge) | None | Rejected — `specs/001` rejects on conflict with no merge; FR-016 now requires a merge path when the developer has unsaved edits |

**Cost note (Constitution VIII)**: No new dependency; reuses the CST + the
existing `diff` crate if needed for within-node text merge.

---

## §7 — Performance budget validation strategy

**Decision**: The ≤400 ms render / ≤400 ms parse / 60 fps interaction budget
(clarification Q1) is validated by extending the existing
`tests/scale_validation.rs` pattern and by a Playwright performance test in
the frontend e2e suite.

**Rationale**:
- `specs/010` already established `scale_validation.rs` for the 500-task /
  100-attempt / 1000-file scale budget. Spec Studio extends it with a
  200-task CST-construction timing assertion (≤400 ms) and a semantic-graph
  derivation assertion.
- The frontend 60 fps budget is validated by a Playwright trace-based test
  that loads a 200-task fixture and asserts frame timing during scroll/toggle
  interactions, mirroring how the concept's own "318 ms · Playwright trace"
  evidence was produced.
- These are real, runnable budget gates — not aspirational. They go green or
  the feature is not done.

**Cost note**: No new dependency. Uses existing `cargo test` + Playwright
infrastructure.

---

## Summary of dependency changes

| Dependency | Status | Justification section |
|------------|--------|----------------------|
| `pulldown-cmark` 0.12 | **Already present** (no change) | §3 — extended with an offset-tracking wrapper, no new dep |
| `codemirror` + `@codemirror/lang-markdown` (frontend) | **New** (~150 KB gz, scoped) | §2 — the single new frontend dep, scoped to FR-015 editing depths |
| React, @xyflow/react, Tailwind, motion, chart lib | **NOT adopted** | §1 — the concept's claim that these are installed is false; adoption is unjustified |
| `markdown-rs` / `comrak` | **NOT adopted** | §3 — duplicates an existing parser for a subset |
| SQLite (overlay) | **NOT adopted** | §5 — JSONL+JSON fits, no new schema surface |

**Net new runtime dependencies for the entire feature: 1 (CodeMirror 6, frontend, scoped).**
This satisfies Constitution Principle VIII's lean-code and
dependency-justification bar.
