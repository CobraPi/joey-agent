# Data Model: Spec Studio — Visual IDE for Spec Kit

**Branch**: `012-spec-studio-visual-ide` | **Date**: 2026-08-05
**Plan**: [plan.md](./plan.md) | **Spec**: [spec.md](./spec.md)

This document defines the entities introduced by Spec Studio's **Meaning
Layer** (FR-012 through FR-024) and the **Overlay layer** extension
(FR-032). It is strictly additive over the `specs/001`/`010` data model
(`crates/joey-speckit-ui/src/model.rs`): existing types (`Feature`,
`Specification`, `UserStory`, `Requirement`, `Task`, `Plan`,
`ConstitutionGate`, `ArtifactKind`, `WorkflowPhase`, `WorkflowStep`,
`WorkflowAttempt`, `AgentInteraction`, `ChangeSet`, etc.) are preserved
unchanged (Constitution VII). New types are grouped under clearly-labelled
sections and re-exported from `model.rs` alongside the existing ones.

The three layers named in FR-032 map to this document as follows:
- **Truth layer** — the existing artifact types (unchanged); see
  `specs/010/data-model.md`. Not redefined here.
- **Meaning layer** — §1 (CST), §2 (semantic graph), §3 (patch/merge).
- **Overlay layer** — §4 (the extensions to history + the new UI-state
  record).

---

## §1 — Concrete Syntax Tree (CST) node model

The CST is the lossless in-memory representation of a parsed artifact file
(FR-012). Every byte of the source file is accounted for by exactly one node
range; unrecognized bytes become `Raw` nodes that are preserved verbatim.
This is what `cst_roundtrip.rs` asserts: `file → cst → file` is the identity.

### `CstNode`

The universal node type. One per syntactic construct (heading, list item,
code fence, table row, paragraph, raw range).

| Field | Type | Notes |
|-------|------|-------|
| `id` | `NodeId` (opaque, stable within a parse) | Used by the semantic graph and patch engine to address a node. Stable across reparses of an unchanged file (deterministic allocation order). |
| `kind` | `CstKind` | Discriminant (see below). |
| `byte_start` | `usize` | Inclusive UTF-8 byte offset into the source file (FR-013 anchor). |
| `byte_end` | `usize` | Exclusive UTF-8 byte offset. `[byte_start, byte_end)` is the node's owned range. |
| `expected_bytes` | `String` | The exact source bytes at parse time (FR-013). Verified before any write; a mismatch means the file changed and the patch is routed to three-way merge (FR-014/016). |
| `revision_hash` | `String` | SHA-256 of the file's full content at parse time (FR-013). Coarse-grained drift detector. |
| `fingerprint` | `String` | Structural fingerprint — `"{kind}/{semantic_id}"`, e.g. `"requirement/FR-016"`, `"user_story/US2"`, `"task/T034"`. Used to pair nodes across three-way merge and to track identity across edits (FR-013). |
| `props` | `CstProps` | Kind-specific extracted properties (see below). Extracted during parse, but never used to re-derive text — the `expected_bytes` always win. |
| `children` | `Vec<NodeId>` | Ordered child node ids. The CST is a tree; sibling ranges are contiguous and non-overlapping. |

**Invariants** (enforced by the parser, asserted by tests):
- The root node's `[byte_start, byte_end)` covers `[0, file_len)`.
- For every node, its children's ranges partition a sub-interval of the
  node's range; any gap becomes a `Raw` child.
- Sibling ranges never overlap.
- `expected_bytes == source[byte_start..byte_end]` at parse time.

### `CstKind`

The exhaustive discriminant for Spec Kit markdown constructs. Every kind in
FR-009's mapping catalog has an entry.

```rust
pub enum CstKind {
    Root,
    Heading { level: u8 },
    Paragraph,
    ListItem,              // a `- ` or `* ` bullet (may carry a semantic pattern)
    CodeFence { lang: Option<String> },
    Table,
    TableRow,
    TableCell,
    BlockQuote,
    ThematicBreak,
    Raw,                   // unrecognized bytes — preserved verbatim (lossless)
    // Semantic-tinged kinds are NOT separate variants; a ListItem stays a
    // ListItem. Its *semantic* classification (Requirement, Task, etc.) is
    // assigned by the meaning layer (§2), not the CST. This keeps the CST
    // a pure syntactic representation and avoids coupling layers.
}
```

### `CstProps`

The kind-specific extracted properties. Stored on the node so the meaning
layer doesn't have to re-parse text, but always reconstructible from
`expected_bytes` (the bytes are authoritative).

```rust
pub enum CstProps {
    None,                                          // Raw, ThematicBreak, Root
    Heading { text: String },                      // raw heading text
    ListItem { marker: char, text: String },       // marker ('-'/'*'), full item text
    CodeFence { content: String },
    TableCell { text: String },
    Paragraph { text: String },
    // … minimal extraction; deep semantic parsing is the meaning layer's job
}
```

### `CstDocument`

The top-level handle returned by the parser.

| Field | Type | Notes |
|-------|------|-------|
| `artifact_path` | `String` | Repo-relative path (`specs/012-…/tasks.md`). |
| `revision_hash` | `String` | SHA-256 of the file content at parse time. |
| `byte_len` | `usize` | Source file length in bytes. |
| `nodes` | `BTreeMap<NodeId, CstNode>` | Ordered by `byte_start`. |
| `root` | `NodeId` | The root node. |

---

## §2 — Semantic graph (Meaning layer)

Derived from the CST by pattern-matching node `kind` + `props` + text
(`meaning/mapping.rs`). This is the projection the widgets render and the
traceability/coverage analysis runs against. It is **in-memory only** and
**invalidated + recomputed** by the semantic cache (FR-040, research.md §4)
on file changes — never a source of truth.

### `SemanticNode`

One per *meaningful* CST node. A CST node with no semantic classification
(e.g. a prose paragraph between requirements) produces no semantic node.

| Field | Type | Notes |
|-------|------|-------|
| `id` | `SemanticId` | Stable within a graph version: `"requirement:FR-016"`, `"user_story:US2"`, `"task:T034"`, `"success_criterion:SC-001"`, `"entity:Feature"`, `"principle:III"`, `"check:CHK007"`, `"phase:3"`, `"checkpoint:phase-3"`. |
| `kind` | `SemanticKind` | See below. |
| `origin` | `CstNodeId` | Back-reference to the CST node (path + NodeId). This is the bridge to byte anchors for editing. |
| `props` | `SemanticProps` | Kind-specific (modality, priority, target value, etc.). |
| `origin_tag` | `OriginTag` | `Source` (read from markdown), `Derived` (computed from graph), `Overlay` (external/private). FR-010 visual distinction. |
| `edges` | `Vec<Edge>` | Traceability + containment edges (see below). |

### `SemanticKind`

```rust
pub enum SemanticKind {
    Principle,          // constitution.md principle row
    UserStory,
    AcceptanceScenario, // Given/When/Then within a story
    Requirement,
    SuccessCriterion,
    KeyEntity,
    EntityRelationship,
    Task,
    Phase,
    Checkpoint,
    Check,              // checklist item
    TechnicalContextField,
    ConstitutionGate,   // a pass/fail row in plan.md
    ComplexityViolation,// a Complexity Tracking table row
    ProjectStructureNode,
    ClarifyMarker,      // [NEEDS CLARIFICATION]
}
```

### `SemanticProps` (per-kind highlights)

- `Requirement { id, modality: Modality (MUST|SHOULD|MAY|MUST_NOT), text }`
- `UserStory { id, priority: Priority (P1|P2|P3), title }`
- `AcceptanceScenario { given, when, then }` — parsed from the GWT pattern.
- `SuccessCriterion { id, target_value: Option<f64>, unit: Option<String>, direction: Option<Direction>, text }` — numeric target extraction per FR-009.
- `Task { id, parallel_eligible: bool, target_files: Vec<String>, user_story_ref: Option<SemanticId>, completed: bool }`
- `KeyEntity { name, fields: Vec<String> }`
- `EntityRelationship { source, verb, target, confidence: Confidence (Explicit|Proposed) }` — proposed edges flagged per FR-011.
- `ClarifyMarker { text, owning_requirement: Option<SemanticId> }`
- `Phase { number, title }`, `Checkpoint { label, blocking: Option<bool> }`
- `ConstitutionGate { principle, result: Pass|Fail|Warn, evidence: String }`
- `ComplexityViolation { rule, why_needed, rejected_alternative }`

### `Edge`

```rust
pub struct Edge {
    pub target: SemanticId,
    pub rel: EdgeKind,
}
pub enum EdgeKind {
    DeliveresValueFor,  // Requirement → UserStory
    Implements,         // Task → Requirement
    Changes,            // Task → ProjectStructureNode (file)
    Verifies,           // Check → Requirement (or Task)
    Governs,            // UserStory/Requirement → Principle
    Contains,           // Phase → Task, UserStory → AcceptanceScenario
    DependsOn,          // Task → Task (from "(depends on T012)" clauses)
    ProposesRelationship,// KeyEntity → KeyEntity (proposed, FR-011)
}
```

### `SemanticGraph`

| Field | Type | Notes |
|-------|------|-------|
| `feature_id` | `String` | The feature this graph is derived for. |
| `revision_hashes` | `HashMap<ArtifactPath, String>` | Per-artifact revision the graph was derived from. Used by the cache to detect staleness. |
| `nodes` | `HashMap<SemanticId, SemanticNode>` | |
| `defects` | `Vec<Defect>` | Precomputed traceability defects (§3). |

---

## §3 — Defects and the patch/merge model

### `Defect` (FR-023)

The four defect classes, each carrying the nodes involved and the one-click
fix affordance (hybrid: deterministic scaffold + optional agent-generated
staged patch — clarification Q3).

| Field | Type | Notes |
|-------|------|-------|
| `id` | `String` | `"defect:orphan:FR-015"`. |
| `class` | `DefectClass` | `OrphanRequirement | RogueTask | Unverified | ConstitutionBreach`. |
| `source_nodes` | `Vec<SemanticId>` | The requirement/task/check/breach at fault. |
| `impact` | `String` | Plain-language summary for the matrix cell. |
| `scaffold` | `Scaffold` | The deterministic structural fix (see below). |
| `generative_followon` | `Option<GenerativeFollowon>` | If the fix benefits from agent generation (a real task body / justification), the scoped run descriptor routed through the existing runner + staging. `None` when the scaffold alone suffices. |

### `Scaffold`

The deterministic, instant, free part of a one-click fix (clarification Q3).

| Field | Type | Notes |
|-------|------|-------|
| `target_artifact` | `ArtifactPath` | e.g. `tasks.md`. |
| `anchor_node` | `SemanticId` | Where the stub is inserted (e.g. the owning phase, or after the orphan requirement). |
| `stub_bytes` | `String` | The exact markdown bytes to insert (a stub task line owning the orphan requirement, a stub checklist item, etc.). |
| `insertion_mode` | `InsertionMode` | `After | Within | Before` the anchor node. |

### `PatchOp`

The atomic unit the patch engine applies. A visual edit compiles to one or
more `PatchOp`s; FR-014 mandates surgical, transactional application.

```rust
pub enum PatchOp {
    Replace { node: NodeId, new_bytes: String },  // rewrite only this node's range
    InsertAfter { anchor: NodeId, new_bytes: String }, // a scaffold insertion
    Delete { node: NodeId },
}
```

### `PatchResult`

```rust
pub enum PatchResult {
    Applied { new_revision_hash: String, undo: Vec<PatchOp> },
    Conflict(ThreeWayMerge),
    AnchorUnresolved { node: NodeId },  // node no longer resolves → read-only + reopen
    ValidationFailed { diagnostics: Vec<String> }, // proposed buffer kept for repair
}
```

### `ThreeWayMerge` (FR-016)

Produced when the revision hash / expected bytes mismatch. Merged at the
semantic-block (CST node) level (research.md §6).

| Field | Type | Notes |
|-------|------|-------|
| `base` | `CstDocument` | The version the developer's edit was based on. |
| `current` | `CstDocument` | The file's current on-disk content. |
| `proposed` | `Vec<PatchOp>` | The developer's proposed change. |
| `conflicts` | `Vec<MergeConflict>` | Nodes whose `expected_bytes` differ on both sides; auto-mergeable nodes resolve silently. |

### `MergeConflict`

| Field | Type | Notes |
|-------|------|-------|
| `node_fingerprint` | `String` | Structural id of the conflicting node. |
| `base_bytes` | `String` | |
| `current_bytes` | `String` | |
| `proposed_bytes` | `String` | |
| `resolution` | `Option<Resolution>` | `None` until the developer chooses; `TakeBase | TakeCurrent | TakeProposed | Edit(String)`. |

---

## §4 — Overlay layer extension

### History JSONL — new record kinds (extends `specs/010`)

The `specs/010` JSONL schema at
`~/.joey/speckit-ui/history/<feature-id>.jsonl` is extended with two new
`record_type` variants. The existing `WorkflowAttempt` record kind is
unchanged (Constitution VII). All records carry `schema_version: 1`.

```rust
pub enum OverlayRecord {
    // Existing, unchanged (specs/010):
    WorkflowAttempt(WorkflowAttemptSummary),
    // NEW — accepted clarify answer (FR-024):
    AcceptedClarify {
        timestamp: String,
        marker_node: SemanticId,    // the [NEEDS CLARIFICATION] node resolved
        question: String,
        answer: String,
        patch_revision: String,     // revision_hash of the reviewed patch applied
    },
    // NEW — anchored comment thread (FR-026 activity center):
    CommentThread {
        thread_id: String,
        anchor_node: SemanticId,
        anchor_fingerprint: String, // structural fingerprint for detach detection
        messages: Vec<CommentMessage>,
    },
}
```

**Detach rule** (research.md §1, FR-032): when a comment thread's
`anchor_fingerprint` no longer resolves in the current CST, the thread is
shown as "detached — structure changed" rather than silently re-anchored.

### UI-state JSON (new file, FR-032)

A small per-repo+branch JSON file at
`~/.joey/speckit-ui/ui-state/<repo-hash>-<branch>.json`. Rewritten atomically
on layout/filter/selection changes (rare). Carries `schema_version: 1`.

| Field | Type | Notes |
|-------|------|-------|
| `schema_version` | `u16` | Versioning gate (Constitution VII). |
| `repo_hash` | `String` | Hash of the repo path (for keying). |
| `branch` | `String` | |
| `selected_feature` | `Option<String>` | Feature id. |
| `open_artifacts` | `Vec<ArtifactPath>` | |
| `active_view` | `Option<String>` | `"atlas" | "spec" | "plan" | "tasks" | "trace" | "review"`. |
| `pane_layout` | `PaneLayout` | Sizes + collapse state. |
| `board_filters` | `BoardFilters` | Phase/story/parallel/completion filters. |
| `scroll_positions` | `HashMap<ArtifactPath, f32>` | |
| `selection` | `Option<SemanticId>` | Cross-view highlight state. |

**Excludes**: unsaved artifact content, secrets, anything not belonging to
this repo+branch. Explicitly never written into the working tree (FR-032).

---

## §5 — State transitions

### CST node (per-node editing state, FR-015/016)

```
Clean ──user edits──▶ Dirty ──save──▶ Validating ──ok──▶ Clean
                                   │                     │
                                   └─validation fail─────▶ Dirty (diagnostics kept)
                          external change on disk
              Clean ───────────────────────────▶ Stale ──reparse──▶ Clean
              Dirty ───────────────────────────▶ Conflict ──merge──▶ Clean
              (anchor no longer resolves) ──▶ ReadOnly (reopen prompt)
```

### Semantic cache (per-feature, FR-040)

```
Current ──watcher event on .md──▶ Stale ──next read──▶ Recomputing ──done──▶ Current
                                                       (≤400 ms budget)
```

### Workflow step (per-step, FR-007, extends `specs/010`)

Extends the existing `specs/010` step-state derivation. The Spec Studio
addition is that "Done" now also requires the output artifact's CST to parse
cleanly and be newer than its inputs (FR-007), where `specs/010` checked
artifact presence + staleness.

```
Locked ──prereqs available──▶ Ready ──start──▶ Running ──success──▶ Done
                              │                │            (output CST parses clean,
                              │                │             newer than inputs)
                              │                ├──cancel────▶ Cancelled (effects preserved)
                              │                ├──fail──────▶ Failed (recovery surface)
                              │                └──ask──────▶ AwaitingInput ──answer──▶ Running
                              └─prereq stale/missing──▶ Blocked (gate card, FR-008)
```

---

## §6 — Validation rules (from requirements)

Mapped to the FRs they enforce, so `tasks.md` (Phase 2) can name them
explicitly as test cases.

| Rule | Source FR | Enforcement point |
|------|-----------|-------------------|
| `cst(file).materialize() == file` for every artifact type incl. malformed/unknown syntax | FR-012 | `cst_roundtrip.rs` |
| A patch changes only `[byte_start, byte_end)` of the edited node; all sibling/parent bytes identical | FR-014, FR-041 | `byte_anchor_patch.rs` |
| Every write verifies `revision_hash` and `expected_bytes` before applying | FR-013, FR-014 | `patch/guard.rs` |
| A revision/expected mismatch routes to `ThreeWayMerge`, never silent write-through | FR-016, SC-006 | `patch/merge.rs` |
| Every accepted visual edit produces an undo `PatchOp` list | FR-014 | `patch/transaction.rs` |
| CST construction for 200-task `tasks.md` ≤ 400 ms p95 | FR-040, SC-010 | `scale_validation.rs` |
| Board initial render for 200 tasks ≤ 400 ms; 60 fps interaction | FR-040, SC-010 | Playwright perf test |
| Semantic cache invalidates + recomputes within 1 s of watcher event | FR-040 | `scale_validation.rs` |
| Orphan/rogue/unverified/breach defects detected at 100% rate on fixture data | FR-023, SC-009 | `meaning_graph.rs` |
| Overlay JSONL/UI-state files never written into the working tree | FR-032 | `ui_state_roundtrip.rs` |
| Existing `specs/001`/`010` parser/writer/API tests pass unchanged | Constitution VII | `contract_api_regression.rs` (preserved) |
| Entity edges inferred from prose are `Proposed` until confirmed | FR-011 | `meaning_graph.rs` |
| Current values for success criteria appear only with a named evidence source | FR-010 | `meaning_graph.rs` |

---

## Entity relationship summary

```
CstDocument 1──1 Artifact (path)
CstDocument 1──∗ CstNode (tree)
SemanticGraph ∗──1 Feature (derived, in-memory, not persisted)
SemanticNode ∗──1 CstNode (origin back-ref, the byte-anchor bridge)
SemanticNode ∗──∗ SemanticNode (edges: traceability spine + coverage)
Defect ∗──∗ SemanticNode (source_nodes)
Defect 1──1 Scaffold (deterministic fix)
Defect 0..1──1 GenerativeFollowon (optional agent-generated staged patch)
PatchOp ∗──1 CstNode (target)
ThreeWayMerge 3──∗ CstDocument (base/current + proposed ops)
OverlayRecord ∗──1 Feature (history JSONL, append-only)
UiState 1──1 (repo, branch) (UI-state JSON, mutable, atomic rewrite)
```
