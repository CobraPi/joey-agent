//! CST parser + materializer traits (contracts/cst-parser.md).
//!
//! The narrow interface between the on-disk Markdown (Truth layer) and the
//! Meaning layer. The implementation lives in `parser.rs`.

use crate::cst::{CstDocument, CstError};

/// The narrow interface between the on-disk Markdown (the Truth layer) and
/// the Meaning layer (contracts/cst-parser.md). Nothing else in the crate
/// reads raw Markdown bytes directly — the existing lossy `parser/` modules
/// from specs/001/010 are preserved for their contract surface but internally
/// route through the CST when byte-exact behavior is needed.
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

/// Reconstruct the exact source bytes from the CST. The identity
/// `parser.parse(p, b)?.materialize() == b` is the round-trip invariant
/// (contracts/cst-parser.md).
pub trait CstMaterialize {
    fn materialize(&self) -> Vec<u8>;
}
