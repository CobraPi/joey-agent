//! Patch transaction (T011, FR-014).
//!
//! Temp buffer → CST re-parse → validation → atomic file replace
//! (write-temp + rename) → return verified inverse `undo: Vec<PatchOp>`.
//! On validation failure return `PatchResult::ValidationFailed` with
//! diagnostics, replacing no file.

use std::path::Path;

use crate::cst::parser::{parse_bytes, verify_partition};
use crate::cst::parser_trait::CstMaterialize;
use crate::patch::guard::{self, GuardOutcome};
use crate::patch::merge;
use crate::patch::surgical::{self, SurgicalError};
use crate::patch::{PatchOp, ThreeWayMerge};

/// Outcome of a transaction attempt. The caller (PatchEngine impl) maps this
/// to a `PatchResult`.
#[derive(Debug)]
pub enum TransactionOutcome {
    /// The patch applied cleanly. `new_bytes` is the verified result; `undo`
    /// restores the pre-patch bytes.
    Applied {
        new_bytes: String,
        new_revision_hash: String,
        undo: Vec<PatchOp>,
    },
    /// External change detected — three-way merge produced.
    Conflict(ThreeWayMerge),
    /// Node anchor no longer resolves.
    AnchorUnresolved { node: crate::cst::NodeId },
    /// The patched buffer failed validation.
    ValidationFailed {
        proposed_bytes: String,
        diagnostics: Vec<String>,
    },
}

/// Execute a patch transaction against a source string (in-memory, no file
/// I/O). The file-write layer wraps this.
///
/// Steps (contracts/patch-engine.md):
///   1. guard check (revision_hash + expected_bytes)
///   2. apply ops to a temp buffer
///   3. re-parse the buffer through the CST and validate
///   4. on success return Applied with undo; on failure return ValidationFailed
pub fn execute(
    doc: &crate::cst::CstDocument,
    source: &str,
    ops: &[PatchOp],
) -> TransactionOutcome {
    let target_nodes: Vec<crate::cst::NodeId> = ops.iter().filter_map(|op| op.target_node()).collect();

    // 1. Guard check.
    match guard::check(doc, source, &target_nodes) {
        GuardOutcome::Ok => {}
        GuardOutcome::Conflict => {
            // Build a three-way merge.
            let current = parse_bytes(&doc.artifact_path, source.as_bytes());
            let conflicts = merge::find_conflicts(doc, &current, ops);
            return TransactionOutcome::Conflict(ThreeWayMerge {
                base: doc.clone(),
                current,
                proposed: ops.to_vec(),
                conflicts,
            });
        }
        GuardOutcome::AnchorUnresolved { node } => {
            return TransactionOutcome::AnchorUnresolved { node };
        }
    }

    // 2. Apply ops to a temp buffer.
    let (proposed_bytes, undo) = match surgical::apply_ops(source, doc, ops) {
        Ok((b, u)) => (b, u),
        Err(SurgicalError::NodeNotFound(node)) => {
            return TransactionOutcome::AnchorUnresolved { node };
        }
        Err(SurgicalError::AnchorMismatch { .. }) | Err(SurgicalError::ShiftOverflow) => {
            // The buffer doesn't match expected — treat as validation failure.
            return TransactionOutcome::ValidationFailed {
                proposed_bytes: source.to_string(),
                diagnostics: vec!["surgical apply failed: anchor mismatch".to_string()],
            };
        }
    };

    // 3. Re-parse + validate (lossless partition check).
    let reparsed = parse_bytes(&doc.artifact_path, proposed_bytes.as_bytes());
    if !verify_partition(&reparsed) {
        return TransactionOutcome::ValidationFailed {
            proposed_bytes,
            diagnostics: vec!["re-parsed CST does not partition [0, byte_len)".to_string()],
        };
    }

    // 4. Round-trip identity check.
    if reparsed.materialize().as_slice() != proposed_bytes.as_bytes() {
        return TransactionOutcome::ValidationFailed {
            proposed_bytes,
            diagnostics: vec!["materialize() != proposed bytes (round-trip broken)".to_string()],
        };
    }

    let new_revision_hash = reparsed.revision_hash.clone();
    TransactionOutcome::Applied {
        new_bytes: proposed_bytes,
        new_revision_hash,
        undo,
    }
}

/// Atomically write `new_bytes` to `path` (write-temp + rename).
pub fn atomic_write(path: &Path, new_bytes: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("md.tmp");
    std::fs::write(&tmp, new_bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

impl PatchOp {
    /// Extract the target/anchor node id from a patch op.
    pub fn target_node(&self) -> Option<crate::cst::NodeId> {
        match self {
            PatchOp::Replace { node, .. } => Some(*node),
            PatchOp::InsertAfter { anchor, .. } => Some(*anchor),
            PatchOp::Delete { node } => Some(*node),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cst::parser::parse_bytes;

    #[test]
    fn transaction_applies_clean_replace() {
        let source = "# Title\n\n- item one\n";
        let doc = parse_bytes("t.md", source.as_bytes());
        let target = doc
            .nodes
            .values()
            .find(|n| matches!(n.kind, crate::cst::CstKind::ListItem))
            .map(|n| n.id)
            .unwrap();

        let ops = vec![PatchOp::Replace {
            node: target,
            new_bytes: "- replaced\n".to_string(),
        }];

        match execute(&doc, source, &ops) {
            TransactionOutcome::Applied { new_bytes, undo, .. } => {
                assert!(new_bytes.contains("- replaced"));
                assert!(!new_bytes.contains("- item one"));
                assert_eq!(undo.len(), 1);
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn transaction_conflict_on_external_change() {
        let base_source = "- item\n";
        let doc = parse_bytes("t.md", base_source.as_bytes());
        let target = doc
            .nodes
            .values()
            .find(|n| matches!(n.kind, crate::cst::CstKind::ListItem))
            .map(|n| n.id)
            .unwrap();

        // The file changed on disk.
        let current_source = "- CHANGED EXTERNALLY\n";
        let ops = vec![PatchOp::Replace {
            node: target,
            new_bytes: "- developer edit\n".to_string(),
        }];

        match execute(&doc, current_source, &ops) {
            TransactionOutcome::Conflict(merge) => {
                assert!(merge.conflicts.len() >= 1);
            }
            other => panic!("expected Conflict, got {other:?}"),
        }
    }

    #[test]
    fn transaction_anchor_unresolved_for_missing_node() {
        let source = "- item\n";
        let doc = parse_bytes("t.md", source.as_bytes());
        let ops = vec![PatchOp::Replace {
            node: crate::cst::NodeId(999),
            new_bytes: "x".to_string(),
        }];
        match execute(&doc, source, &ops) {
            TransactionOutcome::AnchorUnresolved { node } => assert_eq!(node, crate::cst::NodeId(999)),
            other => panic!("expected AnchorUnresolved, got {other:?}"),
        }
    }
}
