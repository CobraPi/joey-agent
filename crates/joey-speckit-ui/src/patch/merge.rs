//! Three-way merge at semantic-block (CST node) level (T012, FR-016).
//!
//! Pair nodes by `fingerprint` across base/current/proposed; auto-merge
//! non-conflicting nodes; surface `MergeConflict` for both-sides-changed
//! nodes with `TakeBase|TakeCurrent|TakeProposed|Edit(bytes)` resolution.
//! <500 ms budget for a 200-task file.

use std::collections::HashMap;

use crate::cst::CstDocument;
use crate::cst::NodeId;
use crate::patch::{MergeConflict, PatchOp, Resolution, ThreeWayMerge};

/// How a proposed op touches a node, for conflict detection/application.
#[derive(Debug, Clone)]
enum ProposedChange {
    /// Node's bytes are replaced with these.
    Replace(String),
    /// Node's byte range is removed entirely.
    Delete,
    /// `new_bytes` are inserted immediately after the anchor node's range.
    InsertAfter(String),
}

/// Index proposed ops by target node. Replace/Delete target the node itself;
/// InsertAfter targets its anchor node.
fn proposed_by_node(proposed: &[PatchOp]) -> HashMap<NodeId, ProposedChange> {
    let mut map = HashMap::new();
    for op in proposed {
        match op {
            PatchOp::Replace { node, new_bytes } => {
                map.insert(*node, ProposedChange::Replace(new_bytes.clone()));
            }
            PatchOp::Delete { node } => {
                map.insert(*node, ProposedChange::Delete);
            }
            PatchOp::InsertAfter { anchor, new_bytes } => {
                map.entry(*anchor)
                    .or_insert(ProposedChange::InsertAfter(new_bytes.clone()));
            }
        }
    }
    map
}

/// Pair each base node id with the current node id that has the same
/// fingerprint. Within an equal-fingerprint group the pairing is positional
/// (k-th base node of the group ↔ k-th current node of the group), so
/// duplicate fingerprints (`list_item/_`) distribute across the group
/// instead of collapsing onto the first match.
fn pair_by_fingerprint(base: &CstDocument, current: &CstDocument) -> HashMap<NodeId, NodeId> {
    let mut by_fp: HashMap<&str, (Vec<NodeId>, Vec<NodeId>)> = HashMap::new();
    for n in base.iter_in_order() {
        by_fp.entry(n.fingerprint.as_str()).or_default().0.push(n.id);
    }
    for n in current.iter_in_order() {
        by_fp.entry(n.fingerprint.as_str()).or_default().1.push(n.id);
    }
    let mut pairs = HashMap::new();
    for (_fp, (base_ids, current_ids)) in by_fp {
        for (b, c) in base_ids.into_iter().zip(current_ids.into_iter()) {
            pairs.insert(b, c);
        }
    }
    pairs
}

/// Pair nodes across base and current by fingerprint, then for each pair
/// decide: auto-merge (one side unchanged), or surface a `MergeConflict`
/// (both sides changed). The `proposed` ops are projected onto the base to
/// derive the "proposed bytes" for each conflicting node.
///
/// Returns the list of conflicts (empty if everything auto-merges). The
/// caller (PatchEngine) wraps this into `PatchResult::Conflict(ThreeWayMerge)`.
pub fn find_conflicts(base: &CstDocument, current: &CstDocument, proposed: &[PatchOp]) -> Vec<MergeConflict> {
    let mut conflicts = Vec::new();

    let proposed_map = proposed_by_node(proposed);
    let pairs = pair_by_fingerprint(base, current);

    // Walk base nodes; for each, compare against its paired current node.
    for base_node in base.iter_in_order() {
        let fp = &base_node.fingerprint;
        let current_node = pairs.get(&base_node.id).and_then(|cid| current.nodes.get(cid));

        let proposed_change = proposed_map.get(&base_node.id);

        let current_changed = match &current_node {
            Some(c) => c.expected_bytes != base_node.expected_bytes,
            None => true, // missing in current = structural change
        };
        let proposed_changed = proposed_change.is_some();

        if current_changed && proposed_changed {
            // Both sides changed → conflict.
            let current_bytes = current_node
                .map(|c| c.expected_bytes.clone())
                // Missing in current: empty bytes. apply_resolutions must
                // NOT fall back to `result.find("")` (== Some(0)) for these
                // — that would prepend the chosen bytes at offset 0.
                .unwrap_or_default();
            let proposed_bytes = match proposed_change {
                Some(ProposedChange::Replace(b)) => b.clone(),
                Some(ProposedChange::Delete) => String::new(),
                Some(ProposedChange::InsertAfter(b)) => b.clone(),
                None => String::new(),
            };
            conflicts.push(MergeConflict {
                node_fingerprint: fp.clone(),
                base_bytes: base_node.expected_bytes.clone(),
                current_bytes,
                proposed_bytes,
                resolution: None,
            });
        }
        // If only one side changed, it auto-merges silently.
    }

    conflicts
}

/// One splice planned against the current document.
enum PlannedSplice {
    /// Replace the byte range of the paired current node with `bytes`.
    OverRange { start: usize, end: usize, bytes: String },
    /// Insert `bytes` at `at` (used by InsertAfter resolutions).
    At { at: usize, bytes: String },
}

/// Apply resolved conflicts to produce a final byte string. For each conflict,
/// the chosen resolution determines the bytes; unconflicted nodes take
/// current's version (external change wins for untouched nodes).
///
/// Node positions are re-derived from `merge.base`/`merge.current` (paired
/// positionally by fingerprint, exactly as `find_conflicts` did) — the
/// `MergeConflict` itself carries only bytes. Splices are planned first and
/// then applied bottom-up (descending offset) so an earlier splice cannot
/// shift a later one's offsets.
pub fn apply_resolutions(
    base_source: &str,
    current_source: &str,
    merge: &ThreeWayMerge,
) -> String {
    let _ = base_source; // base is informational for the UI
    let mut result = current_source.to_string();

    let proposed_map = proposed_by_node(&merge.proposed);
    let pairs = pair_by_fingerprint(&merge.base, &merge.current);

    // Re-derive which base nodes produced which conflict: find_conflicts
    // pushed one conflict per base node (in iteration order) that had a
    // proposed change AND a changed/missing current pair. Replaying that
    // iteration and zipping against `merge.conflicts` recovers the mapping
    // deterministically.
    let mut qualifying: Vec<(NodeId, &ProposedChange)> = Vec::new();
    for base_node in merge.base.iter_in_order() {
        if let Some(change) = proposed_map.get(&base_node.id) {
            let current_node = pairs
                .get(&base_node.id)
                .and_then(|cid| merge.current.nodes.get(cid));
            let current_changed = match &current_node {
                Some(c) => c.expected_bytes != base_node.expected_bytes,
                None => true,
            };
            if current_changed {
                qualifying.push((base_node.id, change));
            }
        }
    }

    let mut splices: Vec<PlannedSplice> = Vec::new();
    for (conflict, (base_id, change)) in merge.conflicts.iter().zip(qualifying.iter()) {
        let current_node = pairs
            .get(base_id)
            .and_then(|cid| merge.current.nodes.get(cid));
        let chosen = match &conflict.resolution {
            Some(Resolution::TakeBase) => conflict.base_bytes.clone(),
            Some(Resolution::TakeCurrent) | None => conflict.current_bytes.clone(),
            Some(Resolution::TakeProposed) => conflict.proposed_bytes.clone(),
            Some(Resolution::Edit(bytes)) => bytes.clone(),
        };
        match change {
            ProposedChange::Replace(_) | ProposedChange::Delete => {
                match current_node {
                    Some(c) => splices.push(PlannedSplice::OverRange {
                        start: c.byte_start,
                        end: c.byte_end,
                        bytes: chosen,
                    }),
                    // Node missing in current: a delete is already satisfied;
                    // a replace has no reliable anchor (base offsets don't
                    // map into the current document). Skip — never splice at
                    // offset 0 (the old `find("")` behavior corrupted the
                    // document by prepending).
                    None => continue,
                }
            }
            ProposedChange::InsertAfter(_) => {
                match current_node {
                    Some(c) => match &conflict.resolution {
                        // Keeping current (or unresolved) means dropping the
                        // insertion; TakeBase reverts the anchor to its base
                        // bytes (base predates the insertion).
                        Some(Resolution::TakeCurrent) | None => continue,
                        Some(Resolution::TakeBase) => splices.push(PlannedSplice::OverRange {
                            start: c.byte_start,
                            end: c.byte_end,
                            bytes: chosen,
                        }),
                        // TakeProposed / Edit: insert the chosen bytes right
                        // after the anchor's current range.
                        Some(Resolution::TakeProposed) | Some(Resolution::Edit(_)) => splices
                            .push(PlannedSplice::At { at: c.byte_end, bytes: chosen }),
                    },
                    // Anchor gone from current: nowhere safe to insert.
                    None => continue,
                }
            }
        }
    }

    // Apply bottom-up so offsets stay valid.
    splices.sort_by(|a, b| {
        let ka = match a {
            PlannedSplice::OverRange { start, .. } => *start,
            PlannedSplice::At { at, .. } => *at,
        };
        let kb = match b {
            PlannedSplice::OverRange { start, .. } => *start,
            PlannedSplice::At { at, .. } => *at,
        };
        kb.cmp(&ka)
    });
    for splice in splices {
        match splice {
            PlannedSplice::OverRange { start, end, bytes } => {
                if end <= result.len()
                    && result.is_char_boundary(start)
                    && result.is_char_boundary(end)
                {
                    result.replace_range(start..end, &bytes);
                }
                // Else: range no longer valid — skip rather than corrupt.
            }
            PlannedSplice::At { at, bytes } => {
                if at <= result.len() && result.is_char_boundary(at) {
                    result.replace_range(at..at, &bytes);
                }
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cst::parser::parse_bytes;

    fn first_list_item(base: &CstDocument) -> NodeId {
        base.nodes
            .values()
            .find(|n| matches!(n.kind, crate::cst::CstKind::ListItem))
            .map(|n| n.id)
            .unwrap()
    }

    fn list_item_containing(base: &CstDocument, needle: &str) -> NodeId {
        base.nodes
            .values()
            .find(|n| {
                matches!(n.kind, crate::cst::CstKind::ListItem) && n.expected_bytes.contains(needle)
            })
            .map(|n| n.id)
            .unwrap_or_else(|| panic!("no list item contains {needle:?}"))
    }

    #[test]
    fn no_conflict_when_only_proposed_changed() {
        let base_source = "- **FR-001**: original\n";
        let current_source = "- **FR-001**: original\n"; // unchanged externally
        let base = parse_bytes("t.md", base_source.as_bytes());
        let current = parse_bytes("t.md", current_source.as_bytes());

        let proposed = vec![PatchOp::Replace {
            node: first_list_item(&base),
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

        let proposed = vec![PatchOp::Replace {
            node: first_list_item(&base),
            new_bytes: "- **FR-001**: changed by developer\n".to_string(),
        }];

        let conflicts = find_conflicts(&base, &current, &proposed);
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].node_fingerprint.contains("FR-001"));
    }

    #[test]
    fn missing_in_current_conflict_does_not_prepend_at_zero() {
        // Base has a uniquely-fingerprinted node (requirement FR-001) that the
        // current document dropped entirely; the developer proposed a
        // replacement for it. The conflict has empty current_bytes, and
        // apply_resolutions must NOT fall into `result.find("")` == Some(0)
        // semantics that prepend the chosen bytes at offset 0.
        let base_source = "# T\n\n- **FR-001**: original requirement\n- plain item\n";
        let current_source = "# T\n\n- plain item\n"; // FR-001 deleted externally
        let base = parse_bytes("t.md", base_source.as_bytes());
        let current = parse_bytes("t.md", current_source.as_bytes());

        let target = list_item_containing(&base, "FR-001");
        let proposed = vec![PatchOp::Replace {
            node: target,
            new_bytes: "- **FR-001**: developer version\n".to_string(),
        }];

        let mut conflicts = find_conflicts(&base, &current, &proposed);
        assert_eq!(conflicts.len(), 1, "node missing in current + proposed change = conflict");
        assert!(
            conflicts[0].current_bytes.is_empty(),
            "node missing in current must carry empty current_bytes"
        );
        conflicts[0].resolution = Some(Resolution::TakeProposed);

        let merge = ThreeWayMerge {
            base: base.clone(),
            current: current.clone(),
            proposed,
            conflicts,
        };
        let result = apply_resolutions(base_source, current_source, &merge);
        assert_eq!(
            result, current_source,
            "missing-in-current node has no reliable anchor — document must be preserved, got {result:?}"
        );
        assert!(
            result.starts_with("# T"),
            "document must not be corrupted by a prepend at offset 0"
        );
    }

    #[test]
    fn duplicate_fingerprints_pair_positionally() {
        // Several plain list items share the `list_item/_` fingerprint. With
        // first-match pairing, EVERY base item pairs to the FIRST current
        // item, mispairing and flagging spurious conflicts. With positional
        // pairing, item k pairs with item k.
        let base_source = "- alpha\n- beta\n- gamma\n";
        let current_source = "- alpha\n- BETA-CHANGED\n- gamma\n";
        let base = parse_bytes("t.md", base_source.as_bytes());
        let current = parse_bytes("t.md", current_source.as_bytes());

        // No proposed ops → no conflicts (only current changed; one-sided
        // changes auto-merge).
        let conflicts = find_conflicts(&base, &current, &[]);
        assert!(
            conflicts.is_empty(),
            "positional pairing must not flag conflicts when only one side changed, got {conflicts:?}"
        );

        // Propose a change to gamma; current changed beta. Positional
        // pairing keeps those on DIFFERENT nodes → no conflict.
        let gamma = list_item_containing(&base, "gamma");
        let proposed = vec![PatchOp::Replace {
            node: gamma,
            new_bytes: "- gamma-dev\n".to_string(),
        }];
        let conflicts = find_conflicts(&base, &current, &proposed);
        assert!(
            conflicts.is_empty(),
            "beta(external) and gamma(developer) are different nodes — no conflict, got {conflicts:?}"
        );

        // But a proposed change to beta itself conflicts with the external
        // beta change (same node, both sides).
        let beta = list_item_containing(&base, "beta");
        let proposed = vec![PatchOp::Replace {
            node: beta,
            new_bytes: "- beta-dev\n".to_string(),
        }];
        let conflicts = find_conflicts(&base, &current, &proposed);
        assert_eq!(conflicts.len(), 1, "same-node both-sides change must conflict");
    }

    #[test]
    fn proposed_delete_conflicts_and_removes_range() {
        let base_source = "- keep\n- drop-me\n";
        let current_source = "- keep\n- drop-me EXTERNAL\n";
        let base = parse_bytes("t.md", base_source.as_bytes());
        let current = parse_bytes("t.md", current_source.as_bytes());

        let target = list_item_containing(&base, "drop-me");
        let proposed = vec![PatchOp::Delete { node: target }];

        let mut conflicts = find_conflicts(&base, &current, &proposed);
        assert_eq!(
            conflicts.len(),
            1,
            "proposed delete on an externally-changed node must surface a conflict"
        );
        conflicts[0].resolution = Some(Resolution::TakeProposed);

        let merge = ThreeWayMerge {
            base: base.clone(),
            current: current.clone(),
            proposed,
            conflicts,
        };
        let result = apply_resolutions(base_source, current_source, &merge);
        assert!(
            !result.contains("drop-me"),
            "TakeProposed on a delete conflict must remove the node, got: {result:?}"
        );
        assert!(result.contains("- keep"));
    }

    #[test]
    fn proposed_insert_after_conflicts_and_splices_at_anchor() {
        let base_source = "- anchor item\n";
        let current_source = "- anchor item EDITED EXTERNALLY\n";
        let base = parse_bytes("t.md", base_source.as_bytes());
        let current = parse_bytes("t.md", current_source.as_bytes());

        let anchor = list_item_containing(&base, "anchor");
        let proposed = vec![PatchOp::InsertAfter {
            anchor,
            new_bytes: "- inserted by developer\n".to_string(),
        }];

        let mut conflicts = find_conflicts(&base, &current, &proposed);
        assert_eq!(
            conflicts.len(),
            1,
            "proposed insert-after an externally-changed anchor must surface a conflict"
        );
        conflicts[0].resolution = Some(Resolution::TakeProposed);

        let merge = ThreeWayMerge {
            base: base.clone(),
            current: current.clone(),
            proposed,
            conflicts,
        };
        let result = apply_resolutions(base_source, current_source, &merge);
        assert!(
            result.contains("- inserted by developer"),
            "TakeProposed on an insert_after conflict must splice in the bytes, got: {result:?}"
        );
        assert!(
            result.contains("anchor item EDITED EXTERNALLY"),
            "current anchor content must survive, got: {result:?}"
        );
    }
}
