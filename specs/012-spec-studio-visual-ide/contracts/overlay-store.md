# Contract: Overlay Store (JSONL + UI-state JSON extension over specs/010)

**Feature**: `012-spec-studio-visual-ide` | **Layer**: Overlay (FR-032)
**Source**: `crates/joey-speckit-ui/src/history.rs` (extended) + `src/ui_state.rs` (new)
**Data model**: [data-model.md §4](../data-model.md)

The Overlay Store holds private IDE state that must **never** dirty the
working tree (FR-032). It extends the `specs/010` JSONL convention and adds
one small per-repo+branch JSON file for mutable UI state. Zero new runtime
dependency; no new schema/versioned database format (Constitution VII/VIII,
clarification Q2, research.md §5).

## Two storage files

### 1. Append-only JSONL history (extends `specs/010`)

Path: `~/.joey/speckit-ui/history/<feature-id>.jsonl` (one line per record,
self-contained, `schema_version: 1`).

`specs/010` introduced the `WorkflowAttempt` record. Spec Studio adds two
new `record_type` variants on the same schema (Constitution VII — additive):

| `record_type` | Purpose | FR |
|---------------|---------|-----|
| `workflow_attempt` | (existing, `specs/010`) run attempt summary | FR-018 |
| `accepted_clarify` | (new) a resolved `[NEEDS CLARIFICATION]` marker, with question, answer, and the reviewed patch revision | FR-024 |
| `comment_thread` | (new) an anchored comment thread (with `anchor_node` + `anchor_fingerprint` for detach detection) | FR-026 |

Append is O(1) (single line). 90-day expiry is the existing file-mtime sweep
from `specs/010` (now covers all `record_type`s). Each record carries
`schema_version: 1`; a breaking change requires a MAJOR bump + documented
migration + round-trip tests (Constitution VII, inherited from `specs/010`).

### 2. UI-state JSON (new file)

Path: `~/.joey/speckit-ui/ui-state/<repo-hash>-<branch>.json`.

A small per-repo+branch key/value blob, rewritten atomically (write-temp +
rename) on layout/filter/selection changes. Carries `schema_version: 1`.

Fields per [data-model.md §4](../data-model.md): `selected_feature`,
`open_artifacts`, `active_view`, `pane_layout`, `board_filters`,
`scroll_positions`, `selection`. **Excludes**: unsaved artifact content,
secrets, anything not belonging to this repo+branch.

## Traits

```rust
pub trait OverlayStore {
    /// Append a record to the feature's JSONL history. O(1).
    fn append(&self, feature_id: &str, record: OverlayRecord) -> Result<(), OverlayError>;

    /// Read history records for a feature, oldest-first, optionally filtered.
    /// Streamed (zero-copy line iteration) so a 500-record history does not
    /// blow the SC-010 budget (research.md §5).
    fn iter_history<'a>(&'a self, feature_id: &'a str) -> impl Iterator<Item = OverlayRecord> + 'a;

    /// Sweep records older than the 90-day TTL. O(n) file-mtime check; no
    /// reindex (inherited from specs/010).
    fn sweep_expired(&self, now: DateTime<Utc>) -> Result<usize, OverlayError>;

    /// Load the UI-state blob for a (repo, branch). Missing file → default.
    fn load_ui_state(&self, repo_hash: &str, branch: &str) -> Result<UiState, OverlayError>;

    /// Atomically save the UI-state blob (write-temp + rename).
    fn save_ui_state(&self, state: &UiState) -> Result<(), OverlayError>;
}
```

## Detach detection for comment threads (FR-032, research.md §1)

Each `comment_thread` record stores `anchor_node` (a `SemanticId`) and
`anchor_fingerprint` (the structural fingerprint at attach time). On load,
the UI checks whether the current semantic graph still contains a node with
that `SemanticId` and a matching `fingerprint`:
- match → thread renders anchored.
- `SemanticId` missing OR `fingerprint` differs → thread renders as
  "detached — structure changed" and offers re-anchor or archive.

The store never silently re-anchors (Constitution III — the comment is
private overlay state, but its anchor identity must be honest).

## Write-tree isolation (FR-032)

Both files live under `~/.joey/speckit-ui/`, **never** under the repository
working tree. The store verifies its root is not inside any known feature
directory at init. Shared binding metadata (the explicit repo-sidecar mode
in FR-032) is a *separate* write that the IDE previews before enabling — it
is not part of the Overlay Store.

## Regression bar (Constitution VII)

The existing `specs/010` JSONL schema and `history.rs` API are preserved.
`tests/history_jsonl_roundtrip.rs` is extended (not replaced) to cover the
two new `record_type` variants and the `schema_version` gate. New:
`tests/ui_state_roundtrip.rs` asserts the JSON blob round-trips and that the
store never writes into the working tree.
