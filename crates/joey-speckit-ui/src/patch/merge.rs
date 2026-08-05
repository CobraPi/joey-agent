//! Three-way merge at semantic-block (CST node) level (T012, FR-016).
//!
//! Pair nodes by `fingerprint` across base/current/proposed; auto-merge
//! non-conflicting nodes; surface `MergeConflict` for both-sides-changed
//! nodes with `TakeBase|TakeCurrent|TakeProposed|Edit(bytes)` resolution.
//! <500 ms budget for a 200-task file.

use crate::cst::CstDocument;
use crate::patch::{MergeConflict, PatchOp, ThreeWayMerge};

/// Pair nodes across base and current by fingerprint, then for each pair
/// decide: auto-merge (one side unchanged), or surface a `MergeConflict`
/// (both sides changed). The `proposed` ops are projected onto the base to
/// derive the "proposed bytes" for each conflicting node.
///
/// Returns the list of conflicts (empty if everything auto-merges). The
/// caller (PatchEngine) wraps this into `PatchResult::Conflict(ThreeWayMerge)`.
pub fn find_conflicts(base: &CstDocument, current: &CstDocument, proposed: &[PatchOp]) -> Vec<MergeConflict> {
    let mut conflicts = Vec::new();

    // Index proposed ops by target node for quick lookup.
    let proposed_by_node: std::collections::HashMap<_, _> = proposed
        .iter()
        .filter_map(|op| match op {
            PatchOp::Replace { node, new_bytes } => Some((node, new_bytes.clone())),
            _ => None,
        })
        .collect();

    // Walk base nodes; for each, compare against current by fingerprint.
    for base_node in base.iter_in_order() {
        let fp = &base_node.fingerprint;
        // Find the current node with the same fingerprint.
        let current_node = current
            .iter_in_order()
            .find(|c| &c.fingerprint == fp);

        let proposed_bytes = proposed_by_node.get(&base_node.id);

        let current_changed = match &current_node {
            Some(c) => c.expected_bytes != base_node.expected_bytes,
            None => true, // missing in current = structural change
        };
        let proposed_changed = proposed_bytes.is_some();

        if current_changed && proposed_changed {
            // Both sides changed → conflict.
            let current_bytes = current_node
                .map(|c| c.expected_bytes.clone())
                .unwrap_or_default();
            conflicts.push(MergeConflict {
                node_fingerprint: fp.clone(),
                base_bytes: base_node.expected_bytes.clone(),
                current_bytes,
                proposed_bytes: proposed_bytes.cloned().unwrap_or_default(),
                resolution: None,
            });
        }
        // If only one side changed, it auto-merges silently.
    }

    conflicts
}

/// Apply resolved conflicts to produce a final byte string. For each conflict,
/// the chosen resolution determines the bytes; unconflicted nodes take
/// current's version (external change wins for untouched nodes).
pub fn apply_resolutions(
    base_source: &str,
    current_source: &str,
    merge: &ThreeWayMerge,
) -> String {
    // Simple strategy: start from current (external-change wins for
    // untouched nodes), then apply each resolved conflict's chosen bytes.
    // For unresolved conflicts, take current (safe default).
    let mut result = current_source.to_string();

    for conflict in &merge.conflicts {
        let chosen = match &conflict.resolution {
            Some(crate::patch::Resolution::TakeBase) => conflict.base_bytes.clone(),
            Some(crate::patch::Resolution::TakeCurrent) | None => conflict.current_bytes.clone(),
            Some(crate::patch::Resolution::TakeProposed) => conflict.proposed_bytes.clone(),
            Some(crate::patch::Resolution::Edit(bytes)) => bytes.clone(),
        };
        // Replace the first occurrence of current_bytes with chosen (best effort).
        if let Some(pos) = result.find(&conflict.current_bytes) {
            result.replace_range(pos..pos + conflict.current_bytes.len(), &chosen);
        }
    }

    let _ = base_source; // base is informational for the UI
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cst::parser::parse_bytes;

    #[test]
    fn no_conflict_when_only_proposed_changed() {
        let base_source = "- **FR-001**: original\n";
        let current_source = "- **FR-001**: original\n"; // unchanged externally
        let base = parse_bytes("t.md", base_source.as_bytes());
        let current = parse_bytes("t.md", current_source.as_bytes());

        let target = base
            .nodes
            .values()
            .find(|n| matches!(n.kind, crate::cst::CstKind::ListItem))
            .map(|n| n.id)
            .unwrap();
        let proposed = vec![PatchOp::Replace {
            node: target,
            new_bytes: "- **FR-001**: changed by developer\n".to_string(),
        }];

        let conflicts = find_conflicts(&base, &current, &proposed);
        assert!(conflicts.is_empty(), "auto-merge when only proposed changed");
    }

    #[test]
    fn conflict_when_both_sides_changed() {
        let base_source = "- **FR-001**: original\n";
        let current_source = "- **FR-001**: changed externally\n";
        let base = parse_bytes("t.md", base_source.as_bytes());
        let current = parse_bytes("t.md", current_source.as_bytes());

        let target = base
            .nodes
            .values()
            .find(|n| matches!(n.kind, crate::cst::CstKind::ListItem))
            .map(|n| n.id)
            .unwrap();
        let proposed = vec![PatchOp::Replace {
            node: target,
            new_bytes: "- **FR-001**: changed by developer\n".to_string(),
        }];

        let conflicts = find_conflicts(&base, &current, &proposed);
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].node_fingerprint.contains("FR-001"));
    }
}
