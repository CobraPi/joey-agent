//! Patch-engine guard (T009, FR-013/014, SC-006).
//!
//! Before-write verification of `revision_hash` + `expected_bytes` for every
//! targeted node; 100% external-change detection (SC-006). Returns `Ok` or
//! routes to `PatchResult::Conflict`.

use crate::cst::{CstDocument, NodeId};

/// Outcome of a guard check on a single node.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardOutcome {
    /// The node's bytes and the file's revision both match — safe to patch.
    Ok,
    /// The file changed on disk since the edit was based on it — route to
    /// three-way merge (FR-016, SC-006).
    Conflict,
    /// The node's anchor no longer resolves in the current CST — degrade to
    /// read-only with a reopen prompt (FR-016 Edge Cases).
    AnchorUnresolved { node: NodeId },
}

/// Check whether the on-disk file state still matches the document's anchors.
///
/// 1. `sha256(current_file_bytes) == doc.revision_hash` — coarse drift check.
/// 2. For the specific target node, `current_bytes[byte_start..byte_end] ==
///    node.expected_bytes` — fine-grained anchor check.
///
/// Either failing routes to `Conflict` (SC-006 — 100% detection). If the
/// revision matches but the node range can't be resolved (structure changed),
/// returns `AnchorUnresolved`.
pub fn check(
    doc: &CstDocument,
    current_file_bytes: &str,
    target_nodes: &[NodeId],
) -> GuardOutcome {
    // 1. Revision-hash drift check.
    let current_hash = crate::conflict::content_hash(current_file_bytes);
    if current_hash != doc.revision_hash {
        return GuardOutcome::Conflict;
    }

    // 2. Per-node expected_bytes check.
    for node_id in target_nodes {
        let node = match doc.get(*node_id) {
            Some(n) => n,
            None => return GuardOutcome::AnchorUnresolved { node: *node_id },
        };
        if node.byte_end > current_file_bytes.len() {
            return GuardOutcome::AnchorUnresolved { node: *node_id };
        }
        let actual = &current_file_bytes[node.byte_start..node.byte_end];
        if actual != node.expected_bytes {
            return GuardOutcome::Conflict;
        }
    }

    GuardOutcome::Ok
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cst::parser::parse_bytes;

    #[test]
    fn ok_when_file_unchanged() {
        let source = "# Title\n\n- item\n";
        let doc = parse_bytes("test.md", source.as_bytes());
        let target = doc
            .nodes
            .values()
            .find(|n| matches!(n.kind, crate::cst::CstKind::ListItem))
            .map(|n| n.id)
            .unwrap();
        let outcome = check(&doc, source, &[target]);
        assert_eq!(outcome, GuardOutcome::Ok);
    }

    #[test]
    fn conflict_when_revision_hash_differs() {
        let source = "# Title\n\n- item\n";
        let doc = parse_bytes("test.md", source.as_bytes());
        let tampered = "# Different\n\n- item\n";
        let outcome = check(&doc, tampered, &[]);
        assert_eq!(outcome, GuardOutcome::Conflict);
    }

    #[test]
    fn conflict_when_node_bytes_differ() {
        let source = "# Title\n\n- item\n";
        let doc = parse_bytes("test.md", source.as_bytes());
        let target = doc
            .nodes
            .values()
            .find(|n| matches!(n.kind, crate::cst::CstKind::ListItem))
            .map(|n| n.id)
            .unwrap();
        // Tamper just the list-item bytes (same length, so revision differs
        // by content). This is the external-change scenario.
        let tampered = "# Title\n\n- TAMPER\n";
        let outcome = check(&doc, tampered, &[target]);
        assert_eq!(outcome, GuardOutcome::Conflict);
    }

    #[test]
    fn anchor_unresolved_when_node_missing() {
        let source = "# Title\n";
        let doc = parse_bytes("test.md", source.as_bytes());
        let outcome = check(&doc, source, &[NodeId(999)]);
        assert_eq!(outcome, GuardOutcome::AnchorUnresolved { node: NodeId(999) });
    }
}
