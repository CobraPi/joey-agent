# Contract: CST Parser (lossless)

**Feature**: `012-spec-studio-visual-ide` | **Layer**: Meaning (P0 critical foundation)
**Source**: `crates/joey-speckit-ui/src/cst/` | **Data model**: [data-model.md §1](../data-model.md)

The `CstParser` trait is the narrow interface between the on-disk Markdown
(the Truth layer) and the Meaning layer. It is the single entry point for
constructing the lossless concrete syntax tree defined in
[data-model.md §1](../data-model.md). Nothing else in the crate reads raw
Markdown bytes directly — the existing lossy `parser/` modules from
`specs/001`/`010` are preserved for their contract surface but internally
route through the CST when byte-exact behavior is needed.

## Trait

```rust
pub trait CstParser {
    /// Parse the UTF-8 bytes of an artifact file into a lossless CST.
    ///
    /// Guarantees (FR-012, enforced by `cst_roundtrip.rs`):
    ///   * every input byte is covered by exactly one node range;
    ///   * `document.materialize() == input` (the identity round-trip);
    ///   * whitespace, comments, unknown extensions, and untouched ranges
    ///     are preserved as `Raw` nodes, never dropped or reformatted.
    ///
    /// Performance (FR-040, SC-010): construction completes within the
    /// ≤400 ms p95 budget for a 200-task `tasks.md`.
    fn parse(&self, artifact_path: &str, bytes: &[u8]) -> Result<CstDocument, CstError>;
}

pub trait CstMaterialize {
    /// Reconstruct the exact source bytes from the CST. The identity
    /// `parser.parse(p, b)?.materialize() == b` is the round-trip invariant.
    fn materialize(&self) -> Vec<u8>;
}
```

## Node addressing

Every node is addressable by `(artifact_path, NodeId)`. `NodeId` is an opaque
`Copy` id allocated deterministically by parse order, stable across reparses
of byte-identical content (so a UI holding a `NodeId` does not need to
re-fetch after an unrelated reparse). After *any* byte change the document is
reparsed and `NodeId`s are re-validated against `fingerprint`; UIs re-bind via
fingerprint, not raw `NodeId`, across edits.

## Anchor contract (FR-013)

Each node carries four anchor fields, all set at parse time and verified
before any write (see [patch-engine.md](./patch-engine.md)):

| Field | Purpose |
|-------|---------|
| `byte_start`, `byte_end` | UTF-8 range the node owns |
| `expected_bytes` | the exact source bytes at parse time |
| `revision_hash` | SHA-256 of the whole file at parse time (drift detector) |
| `fingerprint` | structural id (`"requirement/FR-016"`); identity across edits and merge pairing |

## Behavior with malformed input (FR-012, Edge Cases)

The parser is **always total**: it never panics, never drops bytes, and never
returns `Err` for syntactically odd Markdown. Malformed or unsupported
constructs become `Raw` nodes that preserve their bytes verbatim. The UI
renders parseable nodes as widgets and unsupported ranges as raw text — the
view never blanks out (spec Edge Cases).

The only `Err` path is I/O failure reading the file (which the caller maps to
the existing `WriteError::Io` family).

## Non-goals

- The CST does **not** classify semantic kind (Requirement vs Task vs
  UserStory). That is the meaning layer's job
  ([semantic-graph.md](./semantic-graph.md)). A list item is always a
  `CstKind::ListItem`; its semantic classification is derived.
- The CST does **not** validate Spec Kit structure (required sections,
  unresolved markers). That is `validation.rs`'s job.
- The CST is **not** persisted. It is an in-memory derivation rebuilt from
  the Truth layer on demand (Constitution III).

## Regression bar (Constitution VII)

The existing `parser/spec.rs`, `parser/plan.rs`, `parser/tasks.rs`,
`parser/discovery.rs` modules and the `tests/parser_roundtrip.rs` and
`tests/contract_api_regression.rs` suites are preserved unchanged. The CST
sits *behind* them. New CST tests are additive:
`tests/cst_roundtrip.rs` asserts the identity round-trip across all artifact
types and the malformed-input edge cases.
