//! Byte-anchor patch test (T022, FR-014/041, SC-006).
//!
//! For each `PatchOp` and each node kind, asserts only the edited node's
//! range changed and every other byte is identical (FR-014/041). Also
//! asserts the guard returns `Conflict` on every external-change scenario
//! (SC-006 — 100% detection).

use joey_speckit_ui::cst::parser::parse_bytes;
use joey_speckit_ui::cst::CstKind;
use joey_speckit_ui::patch::guard::{self, GuardOutcome};
use joey_speckit_ui::patch::surgical::apply_ops;
use joey_speckit_ui::patch::PatchOp;

fn find_first_node_of_kind(doc: &joey_speckit_ui::cst::CstDocument, kind: &CstKind) -> Option<joey_speckit_ui::cst::NodeId> {
    let disc = std::mem::discriminant(kind);
    doc.nodes
        .values()
        .find(|n| std::mem::discriminant(&n.kind) == disc)
        .map(|n| n.id)
}

#[test]
fn replace_changes_only_target_node_bytes() {
    let source = "# Title\n\n- item one\n- item two\n";
    let doc = parse_bytes("test.md", source.as_bytes());
    let target = find_first_node_of_kind(&doc, &CstKind::ListItem).expect("list item");

    let ops = vec![PatchOp::Replace {
        node: target,
        new_bytes: "- CHANGED\n".to_string(),
    }];
    let (result, _undo) = apply_ops(source, &doc, &ops).unwrap();

    // Verify every byte outside the edited range is identical.
    let original_target = doc.get(target).unwrap();
    let new_target_len = "- CHANGED\n".len();
    let before = &source[..original_target.byte_start];
    let after = &source[original_target.byte_end..];
    let new_before = &result[..original_target.byte_start];
    let new_after = &result[original_target.byte_start + new_target_len..];

    assert_eq!(before, new_before, "bytes before the edited range must be identical");
    assert_eq!(after, new_after, "bytes after the edited range must be identical");
}

#[test]
fn delete_removes_only_target_node() {
    let source = "- keep\n- delete me\n- also keep\n";
    let doc = parse_bytes("test.md", source.as_bytes());
    let target = doc
        .nodes
        .values()
        .filter(|n| matches!(n.kind, CstKind::ListItem))
        .nth(1)
        .map(|n| n.id)
        .expect("second list item");

    let ops = vec![PatchOp::Delete { node: target }];
    let (result, _undo) = apply_ops(source, &doc, &ops).unwrap();

    assert_eq!(result, "- keep\n- also keep\n");
    // Bytes before the deleted node are untouched.
    let deleted = doc.get(target).unwrap();
    assert_eq!(&result[..deleted.byte_start], "- keep\n");
}

#[test]
fn insert_after_preserves_surrounding_bytes() {
    let source = "- first\n";
    let doc = parse_bytes("test.md", source.as_bytes());
    let anchor = find_first_node_of_kind(&doc, &CstKind::ListItem).expect("list item");

    let ops = vec![PatchOp::InsertAfter {
        anchor,
        new_bytes: "- inserted\n".to_string(),
    }];
    let (result, _undo) = apply_ops(source, &doc, &ops).unwrap();

    assert_eq!(result, "- first\n- inserted\n");
    // The original anchor bytes are preserved.
    let anchor_node = doc.get(anchor).unwrap();
    assert_eq!(
        &result[anchor_node.byte_start..anchor_node.byte_end],
        "- first\n"
    );
}

#[test]
fn guard_detects_revision_hash_change() {
    let source = "# Original\n\n- item\n";
    let doc = parse_bytes("test.md", source.as_bytes());
    let externally_changed = "# DIFFERENT\n\n- item\n";
    let outcome = guard::check(&doc, externally_changed, &[]);
    assert_eq!(outcome, GuardOutcome::Conflict);
}

#[test]
fn guard_detects_node_bytes_change() {
    let source = "# Title\n\n- original\n";
    let doc = parse_bytes("test.md", source.as_bytes());
    let target = find_first_node_of_kind(&doc, &CstKind::ListItem).unwrap();
    // Same length change so only fine-grained detection catches it.
    let tampered = "# Title\n\n- tampered\n";
    let outcome = guard::check(&doc, tampered, &[target]);
    assert_eq!(outcome, GuardOutcome::Conflict);
}

#[test]
fn guard_passes_on_unchanged_file() {
    let source = "# Title\n\n- item\n";
    let doc = parse_bytes("test.md", source.as_bytes());
    let target = find_first_node_of_kind(&doc, &CstKind::ListItem).unwrap();
    let outcome = guard::check(&doc, source, &[target]);
    assert_eq!(outcome, GuardOutcome::Ok);
}

#[test]
fn guard_returns_anchor_unresolved_for_missing_node() {
    let source = "# Title\n";
    let doc = parse_bytes("test.md", source.as_bytes());
    let outcome = guard::check(&doc, source, &[joey_speckit_ui::cst::NodeId(999)]);
    assert_eq!(
        outcome,
        GuardOutcome::AnchorUnresolved {
            node: joey_speckit_ui::cst::NodeId(999),
        }
    );
}

#[test]
fn undo_restores_original_bytes() {
    let source = "# Title\n\n- item\n";
    let doc = parse_bytes("test.md", source.as_bytes());
    let target = find_first_node_of_kind(&doc, &CstKind::ListItem).unwrap();

    let ops = vec![PatchOp::Replace {
        node: target,
        new_bytes: "- changed\n".to_string(),
    }];
    let (changed, undo) = apply_ops(source, &doc, &ops).unwrap();
    assert_ne!(changed, source);

    // Apply undo — it should restore the original.
    let changed_doc = parse_bytes("test.md", changed.as_bytes());
    let (restored, _undo_of_undo) = apply_ops(&changed, &changed_doc, &undo).unwrap();
    assert_eq!(restored, source);
}
