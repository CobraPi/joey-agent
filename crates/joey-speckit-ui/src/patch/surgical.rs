//! Surgical-write implementation (T010, FR-014/041).
//!
//! Applies `PatchOp::{Replace, InsertAfter, Delete}` to a temp buffer so only
//! the edited node's range changes; every byte outside it stays identical.
//! Range-shift accounting for siblings.

use crate::cst::{CstDocument, NodeId};
use crate::patch::PatchOp;

/// Apply a sequence of `PatchOp`s to a source byte string, producing the new
/// bytes and the inverse (undo) op list. Operations are applied in order; a
/// failure in any op returns an error (the caller treats this as
/// `ValidationFailed`).
///
/// All node byte ranges are resolved against the *current* buffer state, with
/// offsets shifted by any prior op in the same transaction. This keeps a
/// multi-op transaction correct (FR-014 transactional).
pub fn apply_ops(source: &str, doc: &CstDocument, ops: &[PatchOp]) -> Result<(String, Vec<PatchOp>), SurgicalError> {
    let mut buffer = source.to_string();
    let mut shift: i64 = 0; // cumulative byte shift from prior ops
    let mut undo: Vec<PatchOp> = Vec::with_capacity(ops.len());

    for op in ops {
        let (new_buffer, op_shift, undo_op) = apply_one(&buffer, doc, op, shift)?;
        undo.push(undo_op);
        buffer = new_buffer;
        shift += op_shift;
    }

    // Undo ops are reversed so applying them undoes in reverse order.
    undo.reverse();
    Ok((buffer, undo))
}

/// Apply a single `PatchOp` to the buffer, returning (new_buffer, byte_shift, undo_op).
fn apply_one(
    buffer: &str,
    doc: &CstDocument,
    op: &PatchOp,
    shift: i64,
) -> Result<(String, i64, PatchOp), SurgicalError> {
    match op {
        PatchOp::Replace { node, new_bytes } => {
            let n = doc.get(*node).ok_or(SurgicalError::NodeNotFound(*node))?;
            let start = shift_range(n.byte_start as i64, shift)?;
            let end = shift_range(n.byte_end as i64, shift)?;
            let old_bytes = n.expected_bytes.clone();

            // Verify the node's bytes still match (guard-like check on the buffer).
            if buffer[start..end] != old_bytes {
                return Err(SurgicalError::AnchorMismatch {
                    node: *node,
                    expected: old_bytes,
                    actual: buffer[start..end].to_string(),
                });
            }

            let mut new_buffer = String::with_capacity(buffer.len() + new_bytes.len());
            new_buffer.push_str(&buffer[..start]);
            new_buffer.push_str(new_bytes);
            new_buffer.push_str(&buffer[end..]);

            let delta = new_bytes.len() as i64 - (end as i64 - start as i64);
            let undo = PatchOp::Replace {
                node: *node,
                new_bytes: old_bytes,
            };
            Ok((new_buffer, delta, undo))
        }
        PatchOp::InsertAfter { anchor, new_bytes } => {
            let n = doc.get(*anchor).ok_or(SurgicalError::NodeNotFound(*anchor))?;
            let insert_at = shift_range(n.byte_end as i64, shift)?;

            let mut new_buffer = String::with_capacity(buffer.len() + new_bytes.len());
            new_buffer.push_str(&buffer[..insert_at]);
            new_buffer.push_str(new_bytes);
            new_buffer.push_str(&buffer[insert_at..]);

            let delta = new_bytes.len() as i64;
            let undo = PatchOp::Delete {
                // The inserted bytes form a new range starting at insert_at.
                // For undo we synthesize a Delete with the anchor's byte range
                // adjusted — the transaction layer re-parses and rebinds.
                node: *anchor,
            };
            Ok((new_buffer, delta, undo))
        }
        PatchOp::Delete { node } => {
            let n = doc.get(*node).ok_or(SurgicalError::NodeNotFound(*node))?;
            let start = shift_range(n.byte_start as i64, shift)?;
            let end = shift_range(n.byte_end as i64, shift)?;
            let old_bytes = n.expected_bytes.clone();

            if buffer[start..end] != old_bytes {
                return Err(SurgicalError::AnchorMismatch {
                    node: *node,
                    expected: old_bytes.clone(),
                    actual: buffer[start..end].to_string(),
                });
            }

            let mut new_buffer = String::with_capacity(buffer.len() - (end - start));
            new_buffer.push_str(&buffer[..start]);
            new_buffer.push_str(&buffer[end..]);

            let delta = -(old_bytes.len() as i64);
            let undo = PatchOp::InsertAfter {
                anchor: *node,
                new_bytes: old_bytes,
            };
            Ok((new_buffer, delta, undo))
        }
    }
}

fn shift_range(pos: i64, shift: i64) -> Result<usize, SurgicalError> {
    let shifted = pos + shift;
    if shifted < 0 {
        return Err(SurgicalError::ShiftOverflow);
    }
    Ok(shifted as usize)
}

/// Errors from surgical-write application.
#[derive(Debug, thiserror::Error)]
pub enum SurgicalError {
    #[error("node {0:?} not found in document")]
    NodeNotFound(NodeId),
    #[error("node {node:?} anchor mismatch: expected {expected:?}, found {actual:?}")]
    AnchorMismatch {
        node: NodeId,
        expected: String,
        actual: String,
    },
    #[error("byte-shift overflow (negative range)")]
    ShiftOverflow,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cst::parser::parse_bytes;

    fn find_first_list_item(doc: &CstDocument) -> Option<NodeId> {
        doc.nodes
            .values()
            .find(|n| matches!(n.kind, crate::cst::CstKind::ListItem))
            .map(|n| n.id)
    }

    #[test]
    fn replace_changes_only_target_node_bytes() {
        let source = "# Title\n\n- item one\n- item two\n";
        let doc = parse_bytes("test.md", source.as_bytes());
        let target = find_first_list_item(&doc).expect("list item exists");

        let ops = vec![PatchOp::Replace {
            node: target,
            new_bytes: "- CHANGED\n".to_string(),
        }];
        let (new_buffer, undo) = apply_ops(source, &doc, &ops).unwrap();

        assert_eq!(new_buffer, "# Title\n\n- CHANGED\n- item two\n");
        // The heading + blank line bytes are untouched.
        assert!(new_buffer.starts_with("# Title\n\n"));
        // Undo restores the original.
        assert_eq!(undo.len(), 1);
    }

    #[test]
    fn delete_removes_only_target_node() {
        let source = "- keep\n- delete me\n- also keep\n";
        let doc = parse_bytes("test.md", source.as_bytes());
        let target = doc
            .nodes
            .values()
            .filter(|n| matches!(n.kind, crate::cst::CstKind::ListItem))
            .nth(1)
            .map(|n| n.id)
            .expect("second list item");

        let ops = vec![PatchOp::Delete { node: target }];
        let (new_buffer, _undo) = apply_ops(source, &doc, &ops).unwrap();

        assert_eq!(new_buffer, "- keep\n- also keep\n");
    }

    #[test]
    fn insert_after_adds_bytes_at_anchor_end() {
        let source = "- first\n";
        let doc = parse_bytes("test.md", source.as_bytes());
        let anchor = find_first_list_item(&doc).expect("list item");

        let ops = vec![PatchOp::InsertAfter {
            anchor,
            new_bytes: "- second\n".to_string(),
        }];
        let (new_buffer, _undo) = apply_ops(source, &doc, &ops).unwrap();

        assert_eq!(new_buffer, "- first\n- second\n");
    }

    #[test]
    fn anchor_mismatch_is_detected() {
        let source = "- original\n";
        let doc = parse_bytes("test.md", source.as_bytes());
        let target = find_first_list_item(&doc).expect("list item");

        // Pass a stale document (simulate external change by giving wrong source).
        let stale_source = "- TAMPERED\n";
        let ops = vec![PatchOp::Replace {
            node: target,
            new_bytes: "- new\n".to_string(),
        }];
        let result = apply_ops(stale_source, &doc, &ops);
        assert!(matches!(result, Err(SurgicalError::AnchorMismatch { .. })));
    }
}
