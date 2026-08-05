//! Markdown-construct → semantic-kind mapping (T014, FR-009 catalog).
//!
//! Classifies each CST node into at most one `SemanticKind` per the exhaustive
//! FR-009 catalog. A node matching no pattern produces no semantic node. Pure
//! function, no I/O.

use crate::cst::{CstKind, CstNode, CstProps};
use crate::meaning::{
    Direction, Modality, NodeOrigin, OriginTag, Priority, SemanticId, SemanticKind, SemanticNode,
    SemanticProps,
};

/// Classify a single CST node. Returns `Some(SemanticNode)` if the node
/// matches a known Spec Kit pattern, `None` otherwise.
///
/// This is the pure mapping function (contracts/semantic-graph.md). The graph
/// builder (graph.rs) calls this for every node and wires up edges afterward.
pub fn classify(_feature_id: &str, artifact: &str, node: &CstNode) -> Option<SemanticNode> {
    let origin = NodeOrigin {
        artifact: artifact.to_string(),
        node: node.id,
        byte_start: node.byte_start,
        byte_end: node.byte_end,
    };

    let (kind, props, id) = match (&node.kind, &node.props) {
        // Heading patterns.
        (CstKind::Heading { level: 2..=3 }, CstProps::Heading { text }) => {
            classify_heading(text).map(|(k, p, i)| (k, p, i))?
        }
        // List-item patterns (requirements, tasks, success criteria, checks,
        // key entities, checkpoints).
        (CstKind::ListItem, CstProps::ListItem { text, .. }) => {
            classify_list_item(text)?
        }
        // Paragraphs: clarify markers, GWT scenarios, technical-context fields,
        // checkpoint lines.
        (CstKind::Paragraph, CstProps::Paragraph { text }) => {
            classify_paragraph(text)?
        }
        // Table rows in plan.md: Constitution Check rows and Complexity Tracking
        // rows are classified at the row level (graph.rs handles the table
        // structure; here we handle the row when it arrives with enough text).
        (CstKind::TableRow, _) => classify_table_row(node)?,
        // Code fences: project structure trees (plan.md).
        (CstKind::CodeFence { .. }, CstProps::CodeFence { content }) => {
            classify_code_fence(content, node)?
        }
        // Table cells in isolation are not classified (handled at row level).
        // Inline tags, raw ranges: not classified.
        _ => return None,
    };

    Some(SemanticNode {
        id,
        kind,
        origin,
        props,
        origin_tag: OriginTag::Source,
        edges: Vec::new(),
    })
}

/// Build a stable `SemanticId` for a kind + bare id.
fn sid(kind: &str, id: &str) -> SemanticId {
    format!("{kind}:{id}")
}

/// Classify a heading text. Returns (kind, props, semantic_id).
fn classify_heading(text: &str) -> Option<(SemanticKind, SemanticProps, SemanticId)> {
    let trimmed = text.trim();

    // `### User Story N (Priority: Px)` or `### User Story N — Title`.
    if let Some(rest) = trimmed.strip_prefix("User Story ").or_else(|| trimmed.strip_prefix("User Story ")) {
        if let Some(num_end) = rest.find(|c: char| !c.is_ascii_digit() && c != '-') {
            let num = &rest[..num_end];
            if !num.is_empty() {
                let id = format!("US{num}");
                let priority = extract_priority(rest);
                let title = extract_heading_title(rest);
                return Some((
                    SemanticKind::UserStory,
                    SemanticProps::UserStory { id: id.clone(), priority, title },
                    sid("user_story", &id),
                ));
            }
        }
    }

    // `## Phase N: Title`
    if let Some(rest) = trimmed.strip_prefix("Phase ") {
        if let Some(num_end) = rest.find(|c: char| !c.is_ascii_digit()) {
            let num_str = &rest[..num_end];
            if let Ok(num) = num_str.parse::<u32>() {
                let title = rest.strip_prefix(num_str).unwrap_or(rest).trim_start_matches(':').trim().to_string();
                return Some((
                    SemanticKind::Phase,
                    SemanticProps::Phase { number: num, title },
                    sid("phase", num_str),
                ));
            }
        }
    }

    None
}

/// Classify a list-item text. Returns (kind, props, semantic_id).
fn classify_list_item(text: &str) -> Option<(SemanticKind, SemanticProps, SemanticId)> {
    let trimmed = text.trim_start();

    // `- **FR-NNN**: ...` (requirement)
    if let Some(id) = extract_bracketed_id(trimmed, "FR-") {
        let (modality, req_text) = extract_modality_and_text(trimmed);
        return Some((
            SemanticKind::Requirement,
            SemanticProps::Requirement { id: id.clone(), modality, text: req_text },
            sid("requirement", &id),
        ));
    }

    // `- **SC-NNN**: ... <number> <unit>` (success criterion)
    if let Some(id) = extract_bracketed_id(trimmed, "SC-") {
        let (target, unit, direction, sc_text) = extract_success_criterion(trimmed);
        return Some((
            SemanticKind::SuccessCriterion,
            SemanticProps::SuccessCriterion {
                id: id.clone(),
                target_value: target,
                unit,
                direction,
                text: sc_text,
            },
            sid("success_criterion", &id),
        ));
    }

    // `- [ ] TNNN ...` / `- [x] TNNN ...` (task)
    if let Some(id) = extract_task_id_from_checkbox(trimmed) {
        let (parallel, target_files, completed, description) = parse_task_body(trimmed);
        // Extract FR-NNN references from the full description for Implements edges.
        let implements_refs = extract_requirement_refs_from_text(&description);
        return Some((
            SemanticKind::Task,
            SemanticProps::Task {
                id: id.clone(),
                parallel_eligible: parallel,
                target_files,
                user_story_ref: extract_story_ref(&description),
                completed,
                implements_refs,
            },
            sid("task", &id),
        ));
    }

    // `- [ ] CHKNNN ...` (check)
    if let Some(id) = extract_bracketed_id_after_checkbox(trimmed, "CHK") {
        return Some((
            SemanticKind::Check,
            SemanticProps::None,
            sid("check", &id),
        ));
    }

    // `- **EntityName**: ...` under a `### Key Entities` heading (key entity).
    // Matches the spec.md Key Entities pattern: `- **Feature**: A Spec Kit feature directory; ...`
    if let Some((name, fields_text)) = extract_key_entity(trimmed) {
        let fields = parse_entity_fields(&fields_text);
        return Some((
            SemanticKind::KeyEntity,
            SemanticProps::KeyEntity { name: name.clone(), fields },
            sid("entity", &name),
        ));
    }

    // `- **Given** ... **When** ... **Then** ...` (acceptance scenario)
    if let Some((given, when, then)) = extract_gwt(trimmed) {
        return Some((
            SemanticKind::AcceptanceScenario,
            SemanticProps::AcceptanceScenario { given, when, then },
            sid("acceptance_scenario", &auto_number()),
        ));
    }

    None
}

/// Classify a paragraph text (clarify markers, technical-context fields,
/// checkpoint lines, entity prose).
fn classify_paragraph(text: &str) -> Option<(SemanticKind, SemanticProps, SemanticId)> {
    // `[NEEDS CLARIFICATION: ...]`
    if let Some(marker_text) = extract_clarify_marker(text) {
        return Some((
            SemanticKind::ClarifyMarker,
            SemanticProps::ClarifyMarker {
                text: marker_text,
                owning_requirement: None,
            },
            sid("clarify", &auto_number()),
        ));
    }

    // `**Checkpoint**: ...` (tasks.md checkpoint line)
    if let Some(label) = extract_bold_label(text, "Checkpoint") {
        return Some((
            SemanticKind::Checkpoint,
            SemanticProps::Checkpoint {
                label,
                blocking: Some(true),
            },
            sid("checkpoint", &auto_number()),
        ));
    }

    // `**Label**: value` (plan.md Technical Context field) — e.g.
    // `**Language/Version**: Rust (edition 2021).`
    if let Some((label, value)) = extract_technical_context_field(text) {
        return Some((
            SemanticKind::TechnicalContextField,
            SemanticProps::None,
            sid("tech_ctx", &label),
        ))
        .map(|(k, _p, id)| {
            // TechnicalContextField carries its label in the SemanticId; the
            // value is recoverable from the origin node's expected_bytes.
            let _ = value;
            (k, SemanticProps::None, id)
        });
    }

    None
}

// ----- helpers -----

fn extract_priority(rest: &str) -> Priority {
    if rest.contains("P1") {
        Priority::P1
    } else if rest.contains("P2") {
        Priority::P2
    } else if rest.contains("P3") {
        Priority::P3
    } else {
        Priority::Unparsed
    }
}

fn extract_heading_title(rest: &str) -> String {
    // After "User Story N", skip digits and take the rest as title.
    let after_digits = rest
        .char_indices()
        .skip_while(|(_, c)| c.is_ascii_digit() || *c == '-')
        .map(|(i, _)| &rest[i..])
        .next()
        .unwrap_or("");
    after_digits.trim_start_matches(|c: char| c == ':' || c.is_whitespace())
        .trim_end()
        .to_string()
}

fn extract_bracketed_id(text: &str, prefix: &str) -> Option<String> {
    if text.starts_with("**") {
        let inner = &text[2..];
        if let Some(end) = inner.find("**") {
            let candidate = &inner[..end];
            if candidate.starts_with(prefix) {
                return Some(strip_trailing(candidate).to_string());
            }
        }
    }
    None
}

fn strip_trailing(s: &str) -> &str {
    s.trim_end_matches(|c: char| c == ':' || c == '.')
}

fn extract_modality_and_text(text: &str) -> (Modality, String) {
    let upper = text.to_uppercase();
    let modality = if upper.contains("MUST NOT") {
        Modality::MustNot
    } else if upper.contains("MUST") {
        Modality::Must
    } else if upper.contains("SHOULD") {
        Modality::Should
    } else if upper.contains("MAY") {
        Modality::May
    } else {
        Modality::Unparsed
    };
    // Text after the `**FR-NNN**:` marker.
    let req_text = text
        .find("**:")
        .and_then(|i| text.get(i + 3..))
        .unwrap_or(text)
        .trim()
        .to_string();
    (modality, req_text)
}

fn extract_success_criterion(text: &str) -> (Option<f64>, Option<String>, Option<Direction>, String) {
    let body = text
        .find("**:")
        .and_then(|i| text.get(i + 3..))
        .unwrap_or(text)
        .trim();

    // Look for a number followed by a unit.
    let mut target = None;
    let mut unit = None;
    let mut direction = None;

    for token in body.split_whitespace() {
        if let Ok(n) = token.trim_end_matches(',').parse::<f64>() {
            target = Some(n);
        } else if target.is_some() && token.chars().all(|c| c.is_alphabetic() || c == '/' || c == '%') {
            unit = Some(token.trim_end_matches(',').to_string());
            break;
        }
    }

    let lower = body.to_lowercase();
    if lower.contains("higher is better") || lower.contains("exceeds") || lower.contains("at least") {
        direction = Some(Direction::HigherIsBetter);
    } else if lower.contains("lower is better") || lower.contains("under") || lower.contains("at most") {
        direction = Some(Direction::LowerIsBetter);
    }

    (target, unit, direction, body.to_string())
}

fn extract_task_id_from_checkbox(text: &str) -> Option<String> {
    // `- [ ] T001 ...` → extract T001
    if !text.starts_with('[') {
        return None;
    }
    let after_bracket = text
        .char_indices()
        .nth(2)
        .and_then(|(i, c)| if c == ']' { Some(&text[i + 1..]) } else { None })?;
    let candidate = after_bracket.trim_start();
    if candidate.starts_with('T') {
        let id_end = candidate
            .find(|c: char| c.is_whitespace() || c == ':')
            .unwrap_or(candidate.len());
        let id = &candidate[..id_end];
        if id.len() > 1 && id[1..].chars().all(|c| c.is_ascii_digit()) {
            return Some(id.to_string());
        }
    }
    None
}

fn extract_bracketed_id_after_checkbox(text: &str, prefix: &str) -> Option<String> {
    if !text.starts_with('[') {
        return None;
    }
    let after_bracket = text
        .char_indices()
        .nth(2)
        .and_then(|(i, c)| if c == ']' { Some(&text[i + 1..]) } else { None })?;
    let candidate = after_bracket.trim_start();
    if candidate.starts_with(prefix) {
        let id_end = candidate
            .find(|c: char| c.is_whitespace() || c == ':')
            .unwrap_or(candidate.len());
        return Some(strip_trailing(&candidate[..id_end]).to_string());
    }
    None
}

fn parse_task_body(text: &str) -> (bool, Vec<String>, bool, String) {
    let completed = text.starts_with("[x]") || text.starts_with("[X]");
    let after_checkbox = if text.starts_with('[') {
        text.char_indices()
            .nth(2)
            .and_then(|(i, c)| if c == ']' { Some(&text[i + 1..]) } else { None })
            .unwrap_or(text)
            .trim_start()
    } else {
        text
    };

    let parallel = after_checkbox.contains("[P]");
    let description = after_checkbox.to_string();

    // Target files: look for "in path" or backtick-quoted paths.
    let target_files = Vec::new(); // simplified — graph builder enriches

    (parallel, target_files, completed, description)
}

fn extract_story_ref(description: &str) -> Option<SemanticId> {
    // `[US2]` or `[US1]` in the description.
    if let Some(start) = description.find("[US") {
        if let Some(end) = description[start..].find(']') {
            let tag = &description[start + 1..start + end];
            return Some(sid("user_story", tag));
        }
    }
    None
}

fn extract_gwt(text: &str) -> Option<(String, String, String)> {
    let given = extract_bold_section(text, "Given")?;
    let when = extract_bold_section(text, "When")?;
    let then = extract_bold_section(text, "Then")?;
    Some((given, when, then))
}

fn extract_bold_section(text: &str, keyword: &str) -> Option<String> {
    let pattern = format!("**{keyword}**");
    let start = text.find(&pattern)?;
    let after = &text[start + pattern.len()..];
    // Take until the next `**When**`, `**Then**`, or end.
    let end = after
        .find("**When**")
        .or_else(|| after.find("**Then**"))
        .unwrap_or(after.len());
    Some(after[..end].trim().trim_end_matches('.').trim().to_string())
}

fn extract_clarify_marker(text: &str) -> Option<String> {
    if let Some(start) = text.find("[NEEDS CLARIFICATION") {
        if let Some(end) = text[start..].find(']') {
            return Some(text[start..start + end + 1].to_string());
        }
        // Unclosed — take the whole marker text.
        return Some(text[start..].to_string());
    }
    None
}

/// Extract a `**BoldLabel**: rest` pattern, returning (label, rest). Used for
/// both Key Entity bullets and Technical Context paragraph fields.
fn extract_bold_label(text: &str, expected_label: &str) -> Option<String> {
    let pattern = format!("**{expected_label}**:");
    if let Some(start) = text.find(&pattern) {
        let rest = text[start + pattern.len()..].trim().to_string();
        return Some(rest);
    }
    // Also accept `**BoldLabel**:` with no space before colon (already handled
    // by the pattern above) and `**BoldLabel** :` with a space.
    let pattern_spaced = format!("**{expected_label}** :");
    if let Some(start) = text.find(&pattern_spaced) {
        let rest = text[start + pattern_spaced.len()..].trim().to_string();
        return Some(rest);
    }
    None
}

/// Extract a Key Entity bullet: `- **Name**: description with fields`.
/// Returns (name, fields_text) where fields_text is the descriptive prose
/// after the colon. The caller parses field hints from it.
fn extract_key_entity(text: &str) -> Option<(String, String)> {
    // Must start with `**` and contain `**:` separator.
    if !text.starts_with("**") {
        return None;
    }
    let inner = &text[2..];
    let end = inner.find("**")?;
    let name = &inner[..end];
    // Name must not look like an FR-/SC-/T-prefixed id (those are handled
    // earlier in the classifier chain) and must not be a GWT keyword.
    if name.starts_with("FR-") || name.starts_with("SC-") {
        return None;
    }
    if matches!(name, "Given" | "When" | "Then" | "Checkpoint") {
        return None;
    }
    // Name should be a capitalised noun phrase (no lowercase-only words).
    if name.is_empty() || name.chars().next().map(|c| c.is_lowercase()).unwrap_or(true) {
        return None;
    }
    // After `**` there should be a `:` then the description.
    let after_name = &inner[end + 2..];
    let after_colon = after_name.trim_start().strip_prefix(':')?.trim();
    Some((name.to_string(), after_colon.to_string()))
}

/// Parse field hints from an entity description. The spec.md Key Entities
/// prose typically lists key attributes inline ("with path, type, validity,
/// dirty state"). We extract comma-separated nouns as best-effort fields.
fn parse_entity_fields(description: &str) -> Vec<String> {
    // Best effort: split on commas/semicolons, take short noun phrases.
    let paren_content = description
        .find('(')
        .and_then(|s| description[s..].find(')').map(|e| &description[s + 1..s + e]))
        .unwrap_or(description);
    paren_content
        .split([',', ';'])
        .map(|s| s.trim().trim_end_matches('.').to_string())
        .filter(|s| !s.is_empty() && s.len() < 80)
        .collect()
}

/// Extract a Technical Context field from a paragraph: `**Label**: value`.
/// Returns (label, value). Only matches when the paragraph starts with a bold
/// label followed by a colon — the plan.md Technical Context convention.
fn extract_technical_context_field(text: &str) -> Option<(String, String)> {
    let trimmed = text.trim();
    if !trimmed.starts_with("**") {
        return None;
    }
    let inner = &trimmed[2..];
    let end = inner.find("**")?;
    let label = &inner[..end];
    if label.is_empty() {
        return None;
    }
    // Skip ids already handled by earlier classifiers.
    if label.starts_with("FR-") || label.starts_with("SC-") {
        return None;
    }
    if matches!(label, "Given" | "When" | "Then" | "Checkpoint") {
        return None;
    }
    let after = &inner[end + 2..];
    let value = after.trim_start().strip_prefix(':')?.trim().to_string();
    if value.is_empty() {
        return None;
    }
    Some((label.to_string(), value))
}

/// Classify a Constitution Check or Complexity Tracking table row. Reads the
/// row's expected_bytes (the raw markdown) because pulldown-cmark gives us
/// `TableRow` nodes whose cell text is embedded in the bytes.
fn classify_table_row(node: &CstNode) -> Option<(SemanticKind, SemanticProps, SemanticId)> {
    let bytes = &node.expected_bytes;
    // Split on `|` to get the cells. Skip leading/trailing empty cells
    // produced by leading/trailing `|`.
    let cells: Vec<&str> = bytes
        .split('|')
        .map(|c| c.trim())
        .filter(|c| !c.is_empty())
        .collect();

    // Complexity Tracking row: 3 cells `(Violation | Why Needed | Rejected)`.
    // Detected by heuristics: it's a row (not a separator `|---|`) with 3
    // non-empty cells and no leading roman-numeral principle id. We rely on
    // the graph builder's section-context to distinguish Constitution Check
    // from Complexity Tracking; here we attempt both patterns and let the
    // graph builder's section-aware pass override.
    //
    // Constitution Check row: first cell is a roman numeral (I..VIII), third
    // cell is PASS/FAIL/WARN.
    if cells.len() >= 3 {
        if let Some(principle) = roman_numeral(cells[0]) {
            let result = parse_gate_result(cells.get(2).unwrap_or(&""));
            let evidence = cells.get(3).unwrap_or(&"").to_string();
            return Some((
                SemanticKind::ConstitutionGate,
                SemanticProps::ConstitutionGate {
                    principle: principle.clone(),
                    result,
                    evidence,
                },
                sid("gate", &principle),
            ));
        }
        // Complexity Tracking row: 3 cells, no roman-numeral first cell, and
        // not a separator. We classify it as a ComplexityViolation.
        if !is_separator_row(&cells) && cells.len() == 3 {
            let rule = cells[0].to_string();
            return Some((
                SemanticKind::ComplexityViolation,
                SemanticProps::ComplexityViolation {
                    rule: rule.clone(),
                    why_needed: cells[1].to_string(),
                    rejected_alternative: cells[2].to_string(),
                },
                sid("violation", &auto_id_from_text(&rule)),
            ));
        }
    }
    None
}

/// Classify a Project Structure code fence. The fence content is a tree of
/// paths (`crates/foo/src/lib.rs`, `└── src/lib.rs`). Each path line becomes
/// one ProjectStructureNode. We return the first one here; the graph builder
/// iterates the rest. For the single-node `classify()` contract, we emit a
/// single ProjectStructureNode whose props carry the full tree (the frontend
/// tree-diff widget renders it).
fn classify_code_fence(content: &str, node: &CstNode) -> Option<(SemanticKind, SemanticProps, SemanticId)> {
    // Only treat `text` fences (or unlabeled fences) as project structures.
    // A `rust`/`json` fence is content, not a structure tree.
    let lang = match &node.kind {
        CstKind::CodeFence { lang } => lang.as_deref().unwrap_or("text"),
        _ => "text",
    };
    if !matches!(lang, "text" | "") {
        return None;
    }
    // Heuristic: project-structure trees contain `└──`, `├──`, or path-like
    // lines starting with `crates/` / `src/` / `tests/`.
    if !looks_like_project_tree(content) {
        return None;
    }
    let id = sid("proj_structure", &auto_id_from_text(&node.expected_bytes));
    Some((SemanticKind::ProjectStructureNode, SemanticProps::None, id))
}

/// Heuristic: does a code-fence body look like a project-structure tree?
fn looks_like_project_tree(content: &str) -> bool {
    content.contains("└──")
        || content.contains("├──")
        || content.lines().any(|l| {
            let t = l.trim();
            t.starts_with("crates/") || t.starts_with("src/") || t.starts_with("tests/")
        })
}

/// Parse a roman numeral I..XII into its string form (uppercased). Returns
/// None if not a recognised principle id.
fn roman_numeral(s: &str) -> Option<String> {
    let upper = s.trim().trim_end_matches('.').to_uppercase();
    match upper.as_str() {
        "I" | "II" | "III" | "IV" | "V" | "VI" | "VII" | "VIII" | "IX" | "X" | "XI" | "XII" => {
            Some(upper)
        }
        _ => None,
    }
}

/// Parse a PASS/FAIL/WARN cell value into a GateResultKind.
fn parse_gate_result(s: &str) -> crate::meaning::GateResultKind {
    let upper = s.trim().to_uppercase();
    // Strip surrounding markdown bold `**PASS**`.
    let stripped = upper.trim_matches('*');
    match stripped {
        "PASS" => crate::meaning::GateResultKind::Pass,
        "FAIL" => crate::meaning::GateResultKind::Fail,
        "WARN" | "WARNING" => crate::meaning::GateResultKind::Warn,
        _ => crate::meaning::GateResultKind::Pass, // default to pass; evidence carries nuance
    }
}

/// Is this a markdown table separator row (`---|---|---`)?
fn is_separator_row(cells: &[&str]) -> bool {
    cells.iter().all(|c| c.chars().all(|ch| ch == '-' || ch == ':' || ch.is_whitespace()) && !c.is_empty())
}

/// Derive a short, stable id from free-form text (used for violations and
/// structure nodes that don't carry an explicit id).
fn auto_id_from_text(text: &str) -> String {
    let lowered = text.to_lowercase();
    let slug: String = lowered
        .chars()
        .take(32)
        .map(|c| if c.is_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        auto_number()
    } else {
        trimmed
    }
}

/// Extract FR-NNN requirement references from a text body (used to populate
/// `Task.implements_refs` during classification, where the full description
/// is available).
pub(crate) fn extract_requirement_refs_from_text(text: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("FR-") {
        let after = &rest[start + 3..];
        let end = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
        if end > 0 {
            refs.push(format!("FR-{}", &after[..end]));
        }
        rest = if start + 3 + end < rest.len() {
            &rest[start + 3 + end..]
        } else {
            break;
        };
    }
    refs
}

fn auto_number() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("auto-{n}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cst::{parser::parse_bytes, CstDocument};

    #[allow(dead_code)]
    fn classify_first(doc: &CstDocument, kind: &CstKind) -> Option<SemanticNode> {
        let node = doc.nodes.values().find(|n| std::mem::discriminant(&n.kind) == std::mem::discriminant(kind))?;
        classify("test-feature", "test.md", node)
    }

    #[test]
    fn classifies_requirement() {
        let doc = parse_bytes("t.md", b"- **FR-001**: The system MUST do X.\n");
        let node = doc.nodes.values().find(|n| matches!(n.kind, CstKind::ListItem)).unwrap();
        let sem = classify("test", "t.md", node).unwrap();
        assert_eq!(sem.kind, SemanticKind::Requirement);
        assert_eq!(sem.id, "requirement:FR-001");
        match sem.props {
            SemanticProps::Requirement { modality, .. } => assert_eq!(modality, Modality::Must),
            _ => panic!("wrong props"),
        }
    }

    #[test]
    fn classifies_user_story_heading() {
        let doc = parse_bytes("t.md", b"### User Story 2: Visual Board (Priority: P2)\n");
        let node = doc.nodes.values().find(|n| matches!(n.kind, CstKind::Heading { .. })).unwrap();
        let sem = classify("test", "t.md", node).unwrap();
        assert_eq!(sem.kind, SemanticKind::UserStory);
        assert_eq!(sem.id, "user_story:US2");
    }

    #[test]
    fn classifies_task_checkbox() {
        let doc = parse_bytes("t.md", b"- [ ] T015 [P] [US2] Implement the thing in src/foo.rs\n");
        let node = doc.nodes.values().find(|n| matches!(n.kind, CstKind::ListItem)).unwrap();
        let sem = classify("test", "t.md", node).unwrap();
        assert_eq!(sem.kind, SemanticKind::Task);
        assert_eq!(sem.id, "task:T015");
    }

    #[test]
    fn classifies_clarify_marker() {
        let doc = parse_bytes("t.md", b"This is [NEEDS CLARIFICATION: what about X?] text.\n\n");
        let node = doc.nodes.values().find(|n| matches!(n.kind, CstKind::Paragraph)).unwrap();
        let sem = classify("test", "t.md", node).unwrap();
        assert_eq!(sem.kind, SemanticKind::ClarifyMarker);
    }

    #[test]
    fn classifies_phase_heading() {
        let doc = parse_bytes("t.md", b"## Phase 2: Core Implementation\n");
        let node = doc.nodes.values().find(|n| matches!(n.kind, CstKind::Heading { .. })).unwrap();
        let sem = classify("test", "t.md", node).unwrap();
        assert_eq!(sem.kind, SemanticKind::Phase);
    }

    #[test]
    fn returns_none_for_unclassifiable_node() {
        let doc = parse_bytes("t.md", b"Random prose paragraph.\n\n");
        let node = doc.nodes.values().find(|n| matches!(n.kind, CstKind::Paragraph)).unwrap();
        assert!(classify("test", "t.md", node).is_none());
    }

    #[test]
    fn classifies_key_entity_bullet() {
        // Pattern from spec.md Key Entities: `- **Feature**: A Spec Kit feature directory; ...`
        let doc = parse_bytes("spec.md", b"- **Feature**: A Spec Kit feature directory; its repository context, open artifacts, layout.\n");
        let node = doc.nodes.values().find(|n| matches!(n.kind, CstKind::ListItem)).unwrap();
        let sem = classify("test", "spec.md", node).unwrap();
        assert_eq!(sem.kind, SemanticKind::KeyEntity);
        assert!(sem.id.starts_with("entity:Feature"), "got {}", sem.id);
        match &sem.props {
            SemanticProps::KeyEntity { name, fields } => {
                assert_eq!(name, "Feature");
                assert!(!fields.is_empty(), "should extract at least one field from prose");
            }
            _ => panic!("expected KeyEntity props, got {:?}", sem.props),
        }
    }

    #[test]
    fn key_entity_classifier_rejects_requirement_ids() {
        // A requirement bullet must NOT be misclassified as a key entity even
        // though both start with `**`.
        let doc = parse_bytes("t.md", b"- **FR-001**: A requirement.\n");
        let node = doc.nodes.values().find(|n| matches!(n.kind, CstKind::ListItem)).unwrap();
        let sem = classify("test", "t.md", node).unwrap();
        assert_eq!(sem.kind, SemanticKind::Requirement);
    }

    #[test]
    fn classifies_technical_context_field() {
        // Pattern from plan.md Technical Context: `**Language/Version**: Rust (edition 2021).`
        let doc = parse_bytes("plan.md", b"**Language/Version**: Rust (edition 2021).\n\n");
        let node = doc.nodes.values().find(|n| matches!(n.kind, CstKind::Paragraph)).unwrap();
        let sem = classify("test", "plan.md", node).unwrap();
        assert_eq!(sem.kind, SemanticKind::TechnicalContextField);
        assert!(sem.id.starts_with("tech_ctx:Language/Version"), "got {}", sem.id);
    }

    #[test]
    fn classifies_checkpoint_paragraph() {
        // Pattern from tasks.md: `**Checkpoint**: P0 foundation ready.`
        let doc = parse_bytes("tasks.md", b"**Checkpoint**: P0 foundation ready.\n\n");
        let node = doc.nodes.values().find(|n| matches!(n.kind, CstKind::Paragraph)).unwrap();
        let sem = classify("test", "tasks.md", node).unwrap();
        assert_eq!(sem.kind, SemanticKind::Checkpoint);
        match sem.props {
            SemanticProps::Checkpoint { blocking, .. } => assert_eq!(blocking, Some(true)),
            _ => panic!("expected Checkpoint props"),
        }
    }

    #[test]
    fn classifies_constitution_gate_row() {
        // Pattern from plan.md Constitution Check table:
        // `| I | Workspace-First Rust | PASS | All code in crates. |`
        let markdown = "| # | Principle | Result | Notes |\n|---|-----------|--------|-------|\n| I | Workspace-First Rust | PASS | All code in crates. |\n";
        let doc = parse_bytes("plan.md", markdown.as_bytes());
        let row = doc.nodes.values().find(|n| matches!(n.kind, CstKind::TableRow)).unwrap();
        let sem = classify("test", "plan.md", row).unwrap();
        assert_eq!(sem.kind, SemanticKind::ConstitutionGate);
        assert_eq!(sem.id, "gate:I");
        match sem.props {
            SemanticProps::ConstitutionGate { principle, result, .. } => {
                assert_eq!(principle, "I");
                assert_eq!(result, crate::meaning::GateResultKind::Pass);
            }
            _ => panic!("expected ConstitutionGate props"),
        }
    }

    #[test]
    fn classifies_constitution_gate_fail_row() {
        // A FAIL row must be detected as a ConstitutionGate with Fail result.
        let markdown = "| # | Principle | Result | Notes |\n|---|-----------|--------|-------|\n| III | Filesystem Source of Truth | FAIL | Cache persisted. |\n";
        let doc = parse_bytes("plan.md", markdown.as_bytes());
        let row = doc.nodes.values().find(|n| matches!(n.kind, CstKind::TableRow)).unwrap();
        let sem = classify("test", "plan.md", row).unwrap();
        match sem.props {
            SemanticProps::ConstitutionGate { principle, result, .. } => {
                assert_eq!(principle, "III");
                assert_eq!(result, crate::meaning::GateResultKind::Fail);
            }
            _ => panic!("expected ConstitutionGate props"),
        }
    }

    #[test]
    fn classifies_complexity_violation_row() {
        // Pattern from plan.md Complexity Tracking:
        // `| Extra cache layer | 3x budget overrun without it | Parse-on-demand too slow. |`
        let markdown = "| Violation | Why Needed | Rejected Because |\n|-----------|------------|------------------|\n| Extra cache layer | 3x budget overrun without it | Parse-on-demand too slow. |\n";
        let doc = parse_bytes("plan.md", markdown.as_bytes());
        let row = doc.nodes.values().find(|n| matches!(n.kind, CstKind::TableRow)).unwrap();
        let sem = classify("test", "plan.md", row).unwrap();
        assert_eq!(sem.kind, SemanticKind::ComplexityViolation);
        match sem.props {
            SemanticProps::ComplexityViolation { rule, why_needed, rejected_alternative } => {
                assert!(rule.contains("Extra cache layer"));
                assert!(why_needed.contains("3x budget overrun"));
                assert!(rejected_alternative.contains("Parse-on-demand"));
            }
            _ => panic!("expected ComplexityViolation props"),
        }
    }

    #[test]
    fn classifies_project_structure_code_fence() {
        // Pattern from plan.md Project Structure (a `text` fence with a tree).
        let markdown = "```text\ncrates/foo/\n└── src/lib.rs\n```\n";
        let doc = parse_bytes("plan.md", markdown.as_bytes());
        let fence = doc.nodes.values().find(|n| matches!(n.kind, CstKind::CodeFence { .. })).unwrap();
        let sem = classify("test", "plan.md", fence).unwrap();
        assert_eq!(sem.kind, SemanticKind::ProjectStructureNode);
    }

    #[test]
    fn code_fence_classifier_rejects_non_text_fences() {
        // A `rust` code fence must NOT be classified as a project structure.
        let markdown = "```rust\nfn main() {}\n```\n";
        let doc = parse_bytes("plan.md", markdown.as_bytes());
        let fence = doc.nodes.values().find(|n| matches!(n.kind, CstKind::CodeFence { .. })).unwrap();
        assert!(classify("test", "plan.md", fence).is_none(), "rust fence should not classify");
    }
}
