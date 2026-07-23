//! Parser for `plan.md` into the `Plan` model, including the Constitution
//! Check gate table.

use crate::model::{ConstitutionGate, GateResult, Plan};

pub fn parse_plan(content: &str) -> Plan {
    Plan {
        summary: extract_summary(content),
        technical_context: extract_section(content, "Technical Context"),
        constitution_gates: parse_constitution_gates(content),
    }
}

fn extract_summary(content: &str) -> String {
    // "## Summary" section's first paragraph, tolerant of absence.
    extract_section(content, "Summary").unwrap_or_default()
}

/// Extract the text under the first heading whose title contains `name`,
/// stopping at the next heading of equal-or-higher level.
fn extract_section(content: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let trimmed = lines[i].trim();
        if trimmed.starts_with('#') && trimmed.to_lowercase().contains(&name.to_lowercase()) {
            let mut buf = String::new();
            let mut j = i + 1;
            while j < lines.len() && !lines[j].trim_start().starts_with('#') {
                if !lines[j].trim().is_empty() {
                    if !buf.is_empty() {
                        buf.push(' ');
                    }
                    buf.push_str(lines[j].trim());
                }
                j += 1;
            }
            return Some(buf);
        }
        i += 1;
    }
    None
}

/// Parse a Markdown table of the form:
/// | Principle | Status | Notes |
/// |---|---|---|
/// | I. Foo | PASS | ... |
fn parse_constitution_gates(content: &str) -> Vec<ConstitutionGate> {
    let mut gates = Vec::new();
    let lines: Vec<&str> = content.lines().collect();
    let mut in_table = false;
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.to_lowercase().contains("constitution check") {
            in_table = true;
            continue;
        }
        if in_table {
            if trimmed.starts_with('#') {
                // reached a new section; stop scanning for this table
                break;
            }
            if trimmed.starts_with('|') {
                // Skip header/separator rows.
                if idx > 0 && trimmed.chars().all(|c| "|-: ".contains(c)) {
                    continue;
                }
                let cells: Vec<&str> = trimmed
                    .trim_matches('|')
                    .split('|')
                    .map(|c| c.trim())
                    .collect();
                if cells.len() >= 2 {
                    // Header row detection: skip if this looks like column titles.
                    let lower0 = cells[0].to_lowercase();
                    if lower0.contains("principle") || lower0.contains("gate") {
                        continue;
                    }
                    let result = parse_gate_result(cells.get(1).copied().unwrap_or(""));
                    gates.push(ConstitutionGate {
                        principle: cells[0].to_string(),
                        result,
                        notes: cells.get(2).map(|s| s.to_string()),
                    });
                }
            }
        }
    }
    gates
}

fn parse_gate_result(text: &str) -> GateResult {
    let lower = text.to_lowercase();
    if lower.contains("pass") {
        GateResult::Pass
    } else if lower.contains("fail") {
        GateResult::Fail
    } else {
        GateResult::Unparsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_constitution_table() {
        let md = "## Constitution Check\n| Principle | Status | Notes |\n|---|---|---|\n| I. Workspace-First Rust | PASS | New crate |\n| II. CLI/TUI Parity | FAIL | needs work |\n";
        let plan = parse_plan(md);
        assert_eq!(plan.constitution_gates.len(), 2);
        assert_eq!(plan.constitution_gates[0].result, GateResult::Pass);
        assert_eq!(plan.constitution_gates[1].result, GateResult::Fail);
    }

    #[test]
    fn malformed_result_is_unparsed() {
        let md = "## Constitution Check\n| Principle | Status |\n|---|---|\n| X | ??? |\n";
        let plan = parse_plan(md);
        assert_eq!(plan.constitution_gates[0].result, GateResult::Unparsed);
    }
}
