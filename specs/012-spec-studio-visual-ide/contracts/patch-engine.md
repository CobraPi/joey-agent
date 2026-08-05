# Contract: Patch Engine (surgical write + three-way merge)

**Feature**: `012-spec-studio-visual-ide` | **Layer**: Meaning (P0 critical foundation)
**Source**: `crates/joey-speckit-ui/src/patch/` | **Data model**: [data-model.md §3](../data-model.md)

The `PatchEngine` trait is the narrow interface for applying visual edits to
the Truth layer. It is the single point that writes to artifact files
(replacing direct use of `writer.rs` for CST-backed edits). Every visual edit
— structured form, inline markdown, raw file — compiles to `PatchOp`s and
flows through this contract. It enforces the six lossless-patch rules in
FR-014/016 and SC-005/006.

## Trait

```rust
#[async_trait]
pub trait PatchEngine {
    /// Apply a patch transactionally. Before applying:
    ///   1. verify each target node's `revision_hash` matches the current
    ///      file and `expected_bytes` still equals the file's bytes at the
    ///      node's range (FR-014 guard);
    ///   2. apply the ops to a temporary buffer;
    ///   3. re-parse the buffer through the CST and run validation;
    ///   4. on success atomically replace the file and return an undo list;
    ///   5. on a guard mismatch return `PatchResult::Conflict(ThreeWayMerge)`;
    ///   6. on validation failure return `PatchResult::ValidationFailed`
    ///      with diagnostics, replacing no file.
    async fn apply(&self, artifact_path: &str, ops: Vec<PatchOp>) -> PatchResult;
}
```

## `PatchOp`

```rust
pub enum PatchOp {
    /// Rewrite only the byte range of `node` with `new_bytes`. The node's
    /// range grows or shrinks; all other nodes keep their relative bytes.
    Replace { node: NodeId, new_bytes: String },
    /// Insert `new_bytes` immediately after `anchor`'s range. Used by the
    /// defect-fix scaffold (FR-023) and inline insertions.
    InsertAfter { anchor: NodeId, new_bytes: String },
    /// Remove the byte range of `node`. The bytes are gone; sibling ranges
    /// compact. Used by delete-node actions and undo.
    Delete { node: NodeId },
}
```

A structured-form edit compiles to exactly one `Replace`. A drag-reorder
within a phase compiles to one `Delete` + one `InsertAfter` (or a single
range move). A scaffold insertion is one `InsertAfter`. The engine applies
ops in order within a transaction; a failure in any op rolls back the whole
transaction (FR-014 — transactional).

## `PatchResult`

```rust
pub enum PatchResult {
    /// The patch applied cleanly. `undo` is the verified inverse `PatchOp`
    /// list — applying it restores the pre-patch bytes exactly (FR-014).
    Applied { new_revision_hash: String, undo: Vec<PatchOp> },
    /// A guard check failed: the file changed on disk since the edit was
    /// based on it. The engine produced a three-way merge at semantic-block
    /// level; the developer must resolve `conflicts` before the patch
    /// proceeds (FR-016, research.md §6).
    Conflict(ThreeWayMerge),
    /// The node's anchor no longer resolves in the current CST (the document
    /// structure changed underneath). The node degrades to read-only with a
    /// "structure changed — reopen" prompt; the engine never guesses a new
    /// range (FR-016, Edge Cases).
    AnchorUnresolved { node: NodeId },
    /// The patched buffer failed CST re-parse or validation. No file is
    /// replaced; the proposed buffer and diagnostics are kept available for
    /// repair or raw review (FR-016, Edge Cases).
    ValidationFailed { proposed_bytes: String, diagnostics: Vec<String> },
}
```

## Guard contract (FR-013/014, SC-006)

Before applying, the engine reads the current file and checks, for every
node targeted by an op:

1. `current_file_bytes[node.byte_start..node.byte_end] == node.expected_bytes`
2. `sha256(current_file_bytes) == node.revision_hash`

Either check failing routes to `Conflict`. **100% of externally changed
artifacts are detected before overwrite** (SC-006). The guard is total: no
write path bypasses it.

## Surgical-write contract (FR-014, FR-041)

After a successful `Replace` or `Delete`:
- every byte outside the edited node's `[byte_start, byte_end)` is
  byte-identical to the pre-patch file;
- the edited node's range reflects `new_bytes`;
- sibling/child ranges are shifted by the delta, but their bytes are
  unchanged.

`tests/byte_anchor_patch.rs` asserts this for every node kind and for the
malformed/unknown-syntax edge cases (where the edited node may be a `Raw`
node).

## Undo contract (FR-014)

Every `Applied` result carries an `undo: Vec<PatchOp>` that, when fed back
through `apply`, restores the pre-patch bytes exactly. The undo is itself
verified under the same guard contract before being recorded, so undo is
always safe to offer as a button.

## Three-way merge (FR-016, research.md §6)

Produced on `Conflict`. The merge is at the **semantic-block (CST node)
level**, not line level:

1. Pair nodes across `base` and `current` by `fingerprint`.
2. For each pair, compare `expected_bytes`:
   - equal on both sides → auto-mergeable.
   - changed only on the `proposed` side → take proposed.
   - changed only on the `current` side → take current (external change wins
     silently for this node; the developer's proposal didn't touch it).
   - changed on **both** sides → a `MergeConflict` surfaced to the UI.
3. The UI renders each conflict as a three-pane card (base / current /
   proposed) at semantic granularity ("FR-016's text conflicts"), never as
   line noise.
4. The developer resolves each `MergeConflict` (`TakeBase | TakeCurrent |
   TakeProposed | Edit(bytes)`); the engine applies the resolved merge as a
   fresh transaction.

## Concurrency with runs (FR-016)

If a node is being edited by the developer while a run is touching the same
file, the edited node **locks**: the agent's output for that node diverts to
the review pane instead of being applied — the developer's intent is never
clobbered mid-thought (FR-016, spec Edge Cases). The lock is per-node, not
per-file, so unrelated nodes in the same file can still receive agent
output into staging.

## Non-goals

- The patch engine does **not** stage agent output. That is the `staging.rs`
  contract from `specs/010` (Git-backed). The patch engine only writes
  developer-accepted edits to the working tree.
- The patch engine does **not** decide *what* bytes to write. It executes
  `PatchOp`s the caller (structured form, inline editor, raw editor,
  scaffold) constructs.
- The patch engine is **not** a merge *policy* — it provides the mechanics;
  the developer chooses resolutions.

## Regression bar (Constitution VII)

The existing `writer.rs` API (`write_if_unchanged`, `replace_line_if_unchanged`,
`read_with_hash`) is preserved unchanged — it is the conflict-checked
foundation the patch engine composes. The `tests/conflict_detection.rs` and
`tests/contract_patch_*.rs` suites continue to pass. New patch tests are
additive: `tests/byte_anchor_patch.rs`, `tests/three_way_merge.rs`.
