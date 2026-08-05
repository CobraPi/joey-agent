//! Structural fingerprint derivation per `CstKind` + extracted semantic id
//! (data-model.md §1). Used by three-way merge pairing and UI re-binding
//! across edits.
//!
//! STUB: full implementation lands in Phase 2 (T007).

use crate::cst::{CstKind, CstProps};

/// Derive a structural fingerprint `"{kind}/{semantic_id}"` for a node. A node
/// matching no semantic pattern yields `"{kind}/_"` so it still has a stable
/// identity for merge pairing.
pub fn fingerprint(kind: &CstKind, props: &CstProps) -> String {
    let kind_str = kind_str(kind);
    match semantic_id(kind, props) {
        Some(id) => format!("{kind_str}/{id}"),
        None => format!("{kind_str}/_"),
    }
}

fn kind_str(kind: &CstKind) -> &'static str {
    match kind {
        CstKind::Root => "root",
        CstKind::Heading { .. } => "heading",
        CstKind::Paragraph => "paragraph",
        CstKind::ListItem => "list_item",
        CstKind::CodeFence { .. } => "code_fence",
        CstKind::Table => "table",
        CstKind::TableRow => "table_row",
        CstKind::TableCell => "table_cell",
        CstKind::BlockQuote => "block_quote",
        CstKind::ThematicBreak => "thematic_break",
        CstKind::Raw => "raw",
    }
}

/// Best-effort semantic id extraction. Returns `Some("FR-016")` for a list
/// item matching `^\s*-\s*\*\*FR-\d+\*\*`, etc. Full patterns land in T007.
fn semantic_id(kind: &CstKind, props: &CstProps) -> Option<String> {
    let text = match (kind, props) {
        (CstKind::ListItem, CstProps::ListItem { text, .. }) => text,
        (CstKind::Heading { .. }, CstProps::Heading { text }) => text,
        _ => return None,
    };
    extract_id_from_text(text)
}

/// Extract a Spec Kit semantic id (`FR-NNN`, `SC-NNN`, `TNNN`, `USN`) from
/// the leading portion of an item's text. Returns the bare id (e.g. `FR-016`).
pub fn extract_id_from_text(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    // Strip leading `- `, `* `, or `[ ]` checkbox prefix.
    let after_marker = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .unwrap_or(trimmed)
        .trim_start();
    // Checkbox form: `- [ ] T001 ...` → after_marker = `[ ] T001 ...`
    let after_checkbox = if after_marker.starts_with('[') {
        after_marker
            .char_indices()
            .nth(2)
            .and_then(|(i, c)| if c == ']' { Some(&after_marker[i + 1..]) } else { None })
            .map(|s| s.trim_start())
            .unwrap_or(after_marker)
    } else {
        after_marker
    };

    // Match `**FR-NNN**` / `**SC-NNN**` / `TNNN` / `USN` patterns.
    let patterns: &[&str] = &["FR-", "SC-", "T", "US"];
    for p in patterns {
        if let Some(id) = extract_bracketed_or_plain(after_checkbox, p) {
            return Some(id);
        }
    }
    None
}

fn extract_bracketed_or_plain(s: &str, prefix: &str) -> Option<String> {
    // `**FR-016**` form.
    if s.starts_with("**") {
        let inner = &s[2..];
        if let Some(end) = inner.find("**") {
            let candidate = &inner[..end];
            if candidate.starts_with(prefix) {
                return Some(strip_trailing_punct(candidate).to_string());
            }
        }
    }
    // `FR-016:` plain form (no bold).
    if s.starts_with(prefix) {
        let id_end = s
            .find(|c: char| c.is_whitespace() || c == ':' || c == ']')
            .unwrap_or(s.len());
        return Some(strip_trailing_punct(&s[..id_end]).to_string());
    }
    None
}

fn strip_trailing_punct(s: &str) -> &str {
    s.trim_end_matches(|c: char| c == ':' || c == '.' || c == ',')
}
