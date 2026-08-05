# Quickstart: Spec Studio — Visual IDE for Spec Kit

**Branch**: `012-spec-studio-visual-ide` | **Date**: 2026-08-05
**Plan**: [plan.md](./plan.md) | **Spec**: [spec.md](./spec.md)

This is a runnable validation guide, not an implementation. It proves the
feature works end-to-end against the contracts and data model, using the
test fixtures and commands the implementation will ship with. Implementation
code, full test bodies, and migrations belong in `tasks.md` and the
implementation phase — not here.

## Prerequisites

- Rust stable toolchain (per `rust-toolchain.toml`), Node 20+, a working
  `joey` CLI build.
- A checkout of this repository on branch `012-spec-studio-visual-ide`.
- A Spec Kit feature with populated artifacts to exercise the Meaning Layer.
  This repo's own `specs/010-speckit-development-ide/` (spec.md, plan.md,
  tasks.md, data-model.md, contracts/, checklists/) is the canonical
  fixture; `specs/001` and `specs/009` provide secondary fixtures.

## Build

```bash
# Backend: the joey-speckit-ui crate (existing, extended).
cargo build -p joey-speckit-ui

# Frontend: the web/speckit-ui Vite app (existing, extended).
cd web/speckit-ui && npm install && npm run build && cd ../..

# Workspace acceptance bar (Constitution VII).
cargo build --workspace && cargo test --workspace
```

## Scenario 1 — Lossless CST round-trip (P0 foundation, FR-012)

Proves the core invariant: `file → cst → file` is the identity, including
for malformed/unknown-syntax files. This is the test that, if green, makes
every later widget safe.

```bash
cargo test -p joey-speckit-ui --test cst_roundtrip
```

Expected: every fixture under `tests/fixtures/*.md` (including the
malformed and unknown-syntax cases) round-trips byte-for-byte. The test
fails on any byte drift, documenting which construct broke.

Reference: [contracts/cst-parser.md](./contracts/cst-parser.md),
[data-model.md §1](./data-model.md).

## Scenario 2 — Surgical byte-anchor patch (P0, FR-014/041, SC-005)

Proves a visual edit changes only the edited node's byte range and that the
guard catches every external change.

```bash
cargo test -p joey-speckit-ui --test byte_anchor_patch
cargo test -p joey-speckit-ui --test conflict_detection   # existing, must stay green
```

Expected:
- After a `Replace` op on a requirement node, every byte outside that
  node's range is byte-identical to the pre-patch file.
- When an external change lands between read and write, the guard returns
  `PatchResult::Conflict(ThreeWayMerge)`, never silent write-through
  (SC-006 — 100% detection).

Reference: [contracts/patch-engine.md](./contracts/patch-engine.md).

## Scenario 3 — Three-way merge at semantic-block level (P0, FR-016)

Proves a conflict surfaces meaningful, block-granular conflicts rather than
line noise.

```bash
cargo test -p joey-speckit-ui --test three_way_merge
```

Expected: when base/current/proposed all differ on a requirement node, the
merge produces a `MergeConflict` labelled by `fingerprint`
(`"requirement:FR-016"`), not a line number; auto-mergeable nodes resolve
silently.

## Scenario 4 — Meaning graph + defect detection (P2/P4, FR-023, SC-009)

Proves the four defect classes are detected at 100% recall on seeded data.

```bash
cargo test -p joey-speckit-ui --test meaning_graph
```

Expected: given the `specs/010` fixture (which contains a known orphan
requireed, a rogue task, an unverified item, and a planted breach), every
defect is detected and each carries a valid `Scaffold`. The hybrid
follow-on (clarification Q3) is validated by asserting the scaffold's
`stub_bytes` round-trips through the patch engine cleanly.

Reference: [contracts/semantic-graph.md](./contracts/semantic-graph.md),
[data-model.md §2/§3](./data-model.md).

## Scenario 5 — Performance budget (FR-040, SC-010)

Proves the ≤400 ms / ≤400 ms / 60 fps budget holds at the 200-task scale.

```bash
# Backend: CST construction + graph derivation timing.
cargo test -p joey-speckit-ui --test scale_validation -- --ignored --nocapture
```

Expected (asserted, not printed-and-eyed):
- CST construction for the 200-task fixture: ≤400 ms p95.
- Board render (frontend, below): ≤400 ms.
- Semantic-cache invalidation + recompute after a watcher event: <1 s.

```bash
# Frontend: Playwright performance trace.
cd web/speckit-ui && npx playwright test tests/perf-board-200.spec.ts
```

Expected: a 200-task board renders its initial view in ≤400 ms, and
scroll/toggle/filter interactions hold 60 fps for ≥95% of frames (the
concept's "318 ms · Playwright trace" bar).

## Scenario 6 — Overlay isolation (FR-032)

Proves overlay files never land in the working tree and that comment
threads detach honestly.

```bash
cargo test -p joey-speckit-ui --test ui_state_roundtrip
cargo test -p joey-speckit-ui --test history_jsonl_roundtrip   # extended
```

Expected:
- The UI-state JSON round-trips and is written under
  `~/.joey/speckit-ui/ui-state/`, never inside any `specs/` directory.
- A `comment_thread` whose `anchor_fingerprint` no longer resolves renders
  as "detached," not silently re-anchored.
- The new JSONL `record_type`s (`accepted_clarify`, `comment_thread`) carry
  `schema_version: 1` and the 90-day sweep covers them.

Reference: [contracts/overlay-store.md](./contracts/overlay-store.md).

## Scenario 7 — Regression: existing contracts unchanged (Constitution VII)

Proves the feature is strictly additive over `specs/001`/`010`.

```bash
cargo test -p joey-speckit-ui --test contract_api_regression
cargo test -p joey-speckit-ui --test parser_roundtrip
cargo test -p joey-speckit-ui --test contract_patch_spec
cargo test -p joey-speckit-ui --test contract_patch_task
```

Expected: all pass without modification — the existing REST/WS endpoints,
parser model, conflict-checked writer, and patch contracts behave exactly
as before. The CST + meaning layer + patch engine are additive behind them.

## Scenario 8 — End-to-end IDE journey (manual, SC-001)

Once the above are green, exercise the full IDE journey by hand against a
local backend:

```bash
# Terminal 1: backend.
cargo run -p joey-speckit-ui

# Terminal 2: frontend dev server.
cd web/speckit-ui && npm run dev
```

Then in the browser:
1. Open the IDE, select this repo, point at `specs/010-…` as the feature.
2. Confirm the Atlas renders: deterministic next action, health, progress,
   binding, artifact list, recent activity (FR-004/005).
3. Open the spec board: confirm each construct renders its matching meaning
   widget (FR-009), success-criterion cards show "not measured" without
   decorative bars (FR-010).
4. Edit a requirement via the structured form; confirm only that node's
   bytes changed on disk (FR-014/041).
5. Open the coverage matrix; confirm the orphan requirement is flagged with
   a one-click fix; click it and confirm the deterministic scaffold inserts
   a stub task line at the right anchor (FR-023, clarification Q3).
6. Trigger a `tasks` run; confirm streamed tool-call timeline, progressive
   task-card preview, and staged review with semantic-hunk labels (FR-027/029).

Expected outcome: the developer completed a specify→tasks→review loop
without a terminal (SC-001), and `git status` shows changes only in the
feature directory under review (staged-by-default, FR-025).

## Notes

- Scenarios 1–3 are the P0 gate: they MUST be green before any meaning
  widget work begins (the concept's "one thing to get right first").
- Scenario 5's budgets are the SC-010 / FR-040 numbers anchored in
  clarification Q1; they are real assertions, not aspirations.
- Scenario 7 is the Constitution VII regression bar — it runs on every
  increment, not just at the end.
