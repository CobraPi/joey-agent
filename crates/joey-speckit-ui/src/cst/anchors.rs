//! Per-node byte-anchor helpers (data-model.md §1): `byte_start`/`byte_end`
//! (UTF-8), `expected_bytes`, `revision_hash` (SHA-256 via existing `sha2`),
//! and `fingerprint` (structural id like `"requirement/FR-016"`).
//!
//! STUB: full implementation lands in Phase 2 (T006).

use crate::cst::NodeId;

/// Helper for addressing a node by `(artifact_path, NodeId)`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct NodeRef {
    pub artifact_path: String,
    pub node: NodeId,
}

/// Verify that the source bytes at `[byte_start, byte_end)` still equal
/// `expected_bytes` and that the file's hash is `revision_hash`. Used by the
/// patch engine guard (contracts/patch-engine.md).
pub fn verify_anchor(
    source: &[u8],
    byte_start: usize,
    byte_end: usize,
    expected_bytes: &str,
) -> bool {
    if byte_end > source.len() || byte_start > byte_end {
        return false;
    }
    &source[byte_start..byte_end] == expected_bytes.as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verify_anchor_matches() {
        let source = b"hello world";
        assert!(verify_anchor(source, 0, 5, "hello"));
    }

    #[test]
    fn verify_anchor_mismatch() {
        let source = b"hello world";
        assert!(!verify_anchor(source, 0, 5, "HELLO"));
    }
}
