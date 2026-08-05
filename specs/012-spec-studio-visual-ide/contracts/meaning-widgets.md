# Contract: Meaning Widgets API (markdown-construct → visual-primitive mapping)

**Feature**: `012-spec-studio-visual-ide` | **Layer**: Meaning (P2) + frontend
**Frontend source**: `web/speckit-ui/src/meaning/` | **Data model**: [data-model.md §2](../data-model.md)

This contract defines the **server-side data** each meaning widget consumes
and the **frontend component** that renders it. The widgets are vanilla-TS
web components (research.md §1 — no React), each projecting from a slice of
the semantic graph. Every widget supports the three editing depths
([patch-engine.md](./patch-engine.md)) and round-trips edits through the
patch engine — none edit the DOM directly as a source of truth.

The mapping catalog is exhaustive over FR-009. Each entry below names: the
`SemanticKind` it renders, the server endpoint that serves its data, and the
component file.

## Widget catalog

| Widget | Renders `SemanticKind` | Endpoint | Component |
|--------|------------------------|----------|-----------|
| Story card | `UserStory` + nested `AcceptanceScenario` | `GET .../meaning/graph?kind=user_story` | `meaning/story-card.ts` |
| Requirement chip | `Requirement` + derived coverage | `GET .../meaning/graph?kind=requirement` | `meaning/requirement-chip.ts` |
| Metric card | `SuccessCriterion` + overlay evidence | `GET .../meaning/graph?kind=success_criterion` | `meaning/metric-card.ts` |
| Entity graph | `KeyEntity` + `EntityRelationship` | `GET .../meaning/graph?kind=entity` | `meaning/entity-graph.ts` |
| Task card | `Task` + derived requirement link | `GET .../meaning/graph?kind=task` | `board/task-card.ts` (extended) |
| Spec sheet | `TechnicalContextField` | `GET .../meaning/graph?kind=tech_context` | `meaning/spec-sheet.ts` |
| Gate row + violation card | `ConstitutionGate` + `ComplexityViolation` | `GET .../meaning/graph?kind=governance` | `meaning/gate-row.ts` |
| Tree diff | `ProjectStructureNode` + filesystem state | `GET .../meaning/tree-diff` | `meaning/tree-diff.ts` |
| Coverage matrix | `Requirement` × `UserStory` density + `Defect` | `GET .../meaning/coverage` | `trace/coverage-matrix.ts` |
| Clarify queue | `ClarifyMarker` + overlay answers | `GET .../meaning/clarify` | `trace/clarify-queue.ts` |
| Defect card | `Defect` + scaffold + follow-on | `GET .../defects` | `trace/defect-card.ts` |
| Spine highlight | any `SemanticId` selection | broadcast via `WS .../meaning/stream` | `trace/spine.ts` |

## Widget data shape (example: story card)

```jsonc
// GET /api/features/{id}/meaning/graph?kind=user_story
{
  "schema_version": 1,
  "feature_id": "012-spec-studio-visual-ide",
  "revision_hashes": { "spec.md": "sha256:…" },
  "nodes": [
    {
      "id": "user_story:US2",
      "kind": "UserStory",
      "origin": { "artifact": "spec.md", "node": "n42", "byte_start": 1234, "byte_end": 1487 },
      "origin_tag": "Source",
      "props": { "id": "US2", "priority": "P2", "title": "Visual Task Board" },
      "edges": [
        { "target": "requirement:FR-012", "rel": "DeliversValueFor" },
        { "target": "acceptance_scenario:US2.1", "rel": "Contains" }
      ]
    }
  ]
}
```

Each node carries its CST `origin` (artifact + node id + byte range) so the
widget can compile edits to `PatchOp`s addressed to that node.

## Edit flow (all widgets)

1. The developer activates an edit affordance on a widget (structured form,
   inline ⌥M, or raw ⌥⇧M).
2. The widget compiles the edit to `PatchOp`(`Replace` for structured edits
   of a single node; `InsertAfter`/`Delete` for add/remove).
3. The widget `POST`s the ops to `POST /api/features/{id}/patch` (see
   [patch-engine.md](./patch-engine.md)).
4. The engine returns `PatchResult`. On `Applied`, the widget re-renders
   from the refreshed semantic graph (pushed via the meaning stream); on
   `Conflict`, it surfaces the three-way merge card; on
   `AnchorUnresolved`, it degrades to read-only with a reopen prompt.
5. The widget offers the `undo` from `Applied` as an explicit undo action.

No widget ever writes to the DOM as a source of truth — every edit round-trips
through the patch engine, which writes through to the Truth layer (FR-041,
Constitution III).

## Accessibility (FR-037)

Every widget:
- is keyboard reachable with visible focus and descriptive ARIA labels;
- conveys state as color + icon + text (never color alone);
- meets WCAG AA contrast;
- uses native semantics (`<button>`, `<input>`, `<fieldset>`) over divs;
- exposes live regions for async state (saving, saved, conflict, stale).

Every drag interaction has a Move-menu equivalent (FR-019/035/037).

## Non-goals

- Widgets do **not** fetch raw markdown — they consume the typed semantic
  graph. Raw markdown is only accessible via the inline/raw editing depths
  (`editor/`), which themselves read the CST's `expected_bytes`.
- Widgets do **not** implement the mapping catalog — that is
  [semantic-graph.md](./semantic-graph.md). Widgets only render.
- Widgets do **not** persist layout/filter state — that is the
  [overlay-store.md](./overlay-store.md).

## Regression bar (Constitution VII)

No existing frontend module is rewritten. `board/task-card.ts` is *extended*
(not replaced) to add the four visual channels. `components/diff-view.ts`,
`components/pane-layout.ts`, `components/status-badges.ts` are reused. New
modules live under `meaning/`, `trace/`, `atlas/`, `firstrun/`, `activity/`,
`palette/`, `editor/`.
