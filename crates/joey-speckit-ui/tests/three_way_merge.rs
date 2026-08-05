//! Three-way merge at semantic-block level (T023, FR-016).
//!
//! Asserts semantic-block merge produces `MergeConflict` labelled by
//! `fingerprint` (not line number); auto-mergeable nodes resolve silently;
//! resolutions apply cleanly.

use joey_speckit_ui::cst::parser::parse_bytes;
use joey_speckit_ui::cst::CstKind;
use joey_speckit_ui::patch::merge::{apply_resolutions, find_conflicts};
use joey_speckit_ui::patch::{PatchOp, Resolution};

fn find_first_list_item(doc: &joey_speckit_ui::cst::CstDocument) -> joey_speckit_ui::cst::NodeId {
    doc.nodes
        .values()
        .find(|n| matches!(n.kind, CstKind::ListItem))
        .map(|n| n.id)
        .unwrap()
}

#[test]
fn conflict_labelled_by_fingerprint_not_line() {
    let base_source = "- **FR-016**: original text\n";
    let current_source = "- **FR-016**: changed externally\n";
    let base = parse_bytes("spec.md", base_source.as_bytes());
    let current = parse_bytes("spec.md", current_source.as_bytes());

    let target = find_first_list_item(&base);
    let proposed = vec![PatchOp::Replace {
        node: target,
        new_bytes: "- **FR-016**: developer edit\n".to_string(),
    }];

    let conflicts = find_conflicts(&base, &current, &proposed);
    assert_eq!(conflicts.len(), 1);
    // The conflict must be labelled by fingerprint (containing the semantic
    // id), not a line number.
    assert!(
        conflicts[0].node_fingerprint.contains("FR-016"),
        "conflict fingerprint must contain the semantic id, got: {}",
        conflicts[0].node_fingerprint
    );
}

#[test]
fn auto_merges_when_only_one_side_changed() {
    let base_source = "- **FR-001**: original\n";
    let current_source = "- **FR-001**: original\n"; // unchanged externally
    let base = parse_bytes("spec.md", base_source.as_bytes());
    let current = parse_bytes("spec.md", current_source.as_bytes());

    let target = find_first_list_item(&base);
    let proposed = vec![PatchOp::Replace {
        node: target,
        new_bytes: "- **FR-001**: developer edit\n".to_string(),
    }];

    let conflicts = find_conflicts(&base, &current, &proposed);
    assert!(conflicts.is_empty(), "auto-merge when only proposed changed");
}

#[test]
fn take_current_resolution_applies() {
    let base_source = "- **FR-001**: original\n";
    let current_source = "- **FR-001**: external\n";
    let base = parse_bytes("spec.md", base_source.as_bytes());
    let current = parse_bytes("spec.md", current_source.as_bytes());

    let target = find_first_list_item(&base);
    let proposed = vec![PatchOp::Replace {
        node: target,
        new_bytes: "- **FR-001**: developer\n".to_string(),
    }];

    let mut conflicts = find_conflicts(&base, &current, &proposed);
    assert_eq!(conflicts.len(), 1);
    conflicts[0].resolution = Some(Resolution::TakeCurrent);

    let merge = joey_speckit_ui::patch::ThreeWayMerge {
        base: base.clone(),
        current: current.clone(),
        proposed,
        conflicts,
    };
    let result = apply_resolutions(base_source, current_source, &merge);
    assert!(
        result.contains("external"),
        "TakeCurrent should keep the external version"
    );
}

#[test]
fn take_proposed_resolution_applies() {
    let base_source = "- **FR-001**: original\n";
    let current_source = "- **FR-001**: external\n";
    let base = parse_bytes("spec.md", base_source.as_bytes());
    let current = parse_bytes("spec.md", current_source.as_bytes());

    let target = find_first_list_item(&base);
    let proposed = vec![PatchOp::Replace {
        node: target,
        new_bytes: "- **FR-001**: developer wins\n".to_string(),
    }];

    let mut conflicts = find_conflicts(&base, &current, &proposed);
    conflicts[0].resolution = Some(Resolution::TakeProposed);

    let merge = joey_speckit_ui::patch::ThreeWayMerge {
        base: base.clone(),
        current: current.clone(),
        proposed,
        conflicts,
    };
    let result = apply_resolutions(base_source, current_source, &merge);
    assert!(result.contains("developer wins"));
}

#[test]
fn edit_resolution_uses_custom_bytes() {
    let base_source = "- **FR-001**: original\n";
    let current_source = "- **FR-001**: external\n";
    let base = parse_bytes("spec.md", base_source.as_bytes());
    let current = parse_bytes("spec.md", current_source.as_bytes());

    let target = find_first_list_item(&base);
    let proposed = vec![PatchOp::Replace {
        node: target,
        new_bytes: "- **FR-001**: developer\n".to_string(),
    }];

    let mut conflicts = find_conflicts(&base, &current, &proposed);
    conflicts[0].resolution = Some(Resolution::Edit("- **FR-001**: manual merge\n".to_string()));

    let merge = joey_speckit_ui::patch::ThreeWayMerge {
        base: base.clone(),
        current: current.clone(),
        proposed,
        conflicts,
    };
    let result = apply_resolutions(base_source, current_source, &merge);
    assert!(result.contains("manual merge"));
}
