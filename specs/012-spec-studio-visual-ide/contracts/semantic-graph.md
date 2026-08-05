# Contract: Semantic Graph (derived graph + edges + defect detection)

**Feature**: `012-spec-studio-visual-ide` | **Layer**: Meaning (P0/P2)
**Source**: `crates/joey-speckit-ui/src/meaning/` | **Data model**: [data-model.md §2/§3](../data-model.md)

The `SemanticGraph` is the derived, in-memory projection the meaning widgets
render and the traceability/coverage analysis runs against. It is derived
purely from CST(s) by pattern-matching (the mapping catalog in FR-009); it
is **never** a source of truth (Constitution III) and is **never** persisted.
It is rebuilt lazily by the semantic cache on file changes
([research.md §4](../research.md)).

## Builder trait

```rust
pub trait SemanticGraphBuilder {
    /// Derive a SemanticGraph from one or more CST documents belonging to
    /// the same feature. Inputs: spec.md, plan.md, tasks.md, checklists/,
    /// data-model.md, constitution.md (as available — missing artifacts
    /// simply contribute no nodes).
    ///
    /// The graph's `revision_hashes` capture exactly which artifact
    /// revisions it was derived from, so the cache can detect staleness.
    fn build(&self, feature_id: &str, documents: &[CstDocument]) -> SemanticGraph;
}
```

## Mapping catalog (FR-009)

`meaning/mapping.rs` classifies each CST node into at most one
`SemanticKind` by pattern-matching its `kind` + `props` + text. The mapping
is exhaustive over FR-009's catalog:

| Markdown pattern (CST shape) | `SemanticKind` | Classification rule |
|------------------------------|----------------|---------------------|
| `### User Story N (Priority: PN)` heading + body | `UserStory` | regex on heading text |
| `**Given** … **When** … **Then** …` within a story | `AcceptanceScenario` | GWT keyword pattern |
| `- **FR-NNN**: …` list item | `Requirement` | `^\s*-\s*\*\*FR-\d+\*\*` |
| `[NEEDS CLARIFICATION: …]` inline text | `ClarifyMarker` | bracket-marker pattern |
| `- **SC-NNN**: … <number> <unit>` list item | `SuccessCriterion` | `^\s*-\s*\*\*SC-\d+\*\*` + numeric extraction |
| `### Key Entities` section + `**Name**:` items | `KeyEntity` | section + bold-colon pattern |
| `has many` / `belongs to` prose near entities | `EntityRelationship` | prose pattern → `Proposed` confidence (FR-011) |
| `- [ ] TNNN [P] [USN] … in path` list item | `Task` | checkbox + task-id pattern |
| `## Phase N: …` heading | `Phase` | heading pattern |
| `**Checkpoint**: …` | `Checkpoint` | bold-keyword pattern |
| `**Language/Version**: …` etc. | `TechnicalContextField` | bold-colon key/value |
| Constitution Check table row | `ConstitutionGate` | table row in plan |
| Complexity Tracking table row | `ComplexityViolation` | table row in plan |
| Project Structure code fence | `ProjectStructureNode` | code fence under that heading |
| `- [ ] CHKNNN …` list item | `Check` | checkbox + check-id pattern |

A CST node that matches no pattern produces no semantic node (it remains
purely syntactic). This keeps the CST and meaning layers decoupled
(Constitution VI).

## Origin tagging (FR-010)

Every `SemanticNode` carries an `OriginTag`:

- **`Source`** — the value was read directly from markdown (e.g. a
  requirement's text, a success criterion's target number).
- **`Derived`** — computed from the graph (e.g. a requirement's coverage
  count, an entity edge inferred from prose).
- **`Overlay`** — external evidence or private state (e.g. a success
  criterion's current measured value from a configured source, a comment
  thread).

The UI renders each origin distinctly (FR-010). Current values for success
criteria appear **only** when a named evidence source is configured;
otherwise the card says "not measured" and no decorative element implies
data that does not exist (FR-010).

## Proposed edges (FR-011)

Entity relationships inferred from prose (e.g. "Feature has many Artifacts")
are classified `Confidence::Proposed`. They appear in the entity graph as
dashed/provisional edges and **do not** affect traceability until the
developer confirms or rejects them. Explicit relationships (declared in a
relationships table or matched against a strict syntax) are
`Confidence::Explicit`.

## Coverage + defect detection (FR-022/023, SC-009)

`meaning/coverage.rs` computes the coverage matrix and the four defect
classes from the graph edges:

| Defect class | Detection rule | One-click fix (hybrid, clarification Q3) |
|--------------|----------------|------------------------------------------|
| **Orphan requirement** | a `Requirement` with zero incoming `Implements` edges from `Task`s | deterministic: `InsertAfter` a stub task line owning the requirement, in the owning phase; generative follow-on: agent writes the task body as a staged patch |
| **Rogue task** | a `Task` with no outgoing `Implements` edge to any `Requirement` | deterministic: link to the nearest requirement or promote; generative: agent drafts the new/updated requirement |
| **Unverified** | a `Task` (or implemented requirement) with no incoming `Verifies` edge from a `Check` | deterministic: `InsertAfter` a stub checklist item; generative: agent drafts the check text |
| **Constitution breach** | a `Task` that violates a principle with no entry in Complexity Tracking | deterministic: surface the breach card for justify/redesign; generative: agent drafts the justification as a Complexity Tracking row |

Defect detection runs at 100% recall on the fixture corpus (SC-009):
`tests/meaning_graph.rs` asserts every defect present in the seeded data is
detected and surfaced with its scaffold.

## Cache + invalidation (FR-040, research.md §4)

`meaning/cache.rs` holds the current `SemanticGraph` per open feature,
keyed by `feature_id`. It hooks the existing `watcher.rs` events:

1. Any change to a `.md` file in the feature directory marks the graph
   `Stale` and drops the affected artifact's CST.
2. The next read triggers a reparse + rebuild (≤400 ms budget for a
   200-task file — SC-010).
3. The UI subscribes via `WS /api/features/{id}/meaning/stream` and
   receives a refreshed graph on each recompute.

The cache is **never** persisted (Constitution III): losing it costs a
one-time reparse, never authored work.

## Selection + cross-view highlighting (FR-021)

Selecting any `SemanticId` in any view broadcasts a selection event. Every
open view receives it and:
- dims nodes not reachable via the selected node's edges;
- highlights the full traceability spine (principle → story → requirement →
  task → file → check);
- scrolls the relevant widget into view.

This is a UI concern (the frontend implements highlighting), but the graph
provides the edge data that makes it O(edges) rather than a re-scan.

## Non-goals

- The semantic graph does **not** persist. Re-derivation is always from the
  Truth layer.
- The graph does **not** classify prose quality, only structure. A
  well-written-but-unstructured paragraph is a CST `Paragraph` with no
  semantic node — the meaning layer does not judge content.
- The graph does **not** drive agent runs. It informs the UI and the defect
  fix affordance; runs flow through the existing `runner.rs` (Constitution
  VI).

## Regression bar (Constitution VII)

The semantic graph is strictly additive — no existing endpoint or type is
modified. New endpoints (`GET .../meaning/graph`, `GET .../meaning/coverage`,
`WS .../meaning/stream`) extend the `specs/001`/`010` REST/WS surface.
New tests: `tests/meaning_graph.rs`.
