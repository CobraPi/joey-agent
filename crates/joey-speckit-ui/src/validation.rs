//! Required-structure + unresolved-marker validation per `ArtifactKind`
//! (FR-007).
//!
//! Each kind has a tolerant structural check (mirroring the
//! `Status::Unparsed`-on-malformed pattern from `model.rs`). Findings are
//! anchored to `ArtifactLocation` for navigation (FR-023).

use crate::model::{ArtifactKind, ArtifactLocation, Severity, ValidationFinding};

/// Marker strings that indicate an unresolved workflow placeholder.
const UNRESOLVED_MARKERS: &[&str] = &[
    "NEEDS CLARIFICATION",
    "TBD",
    "[REMOVE IF UNUSED]",
    "TODO",
    "FIXME",
    "XXX",
];

/// Validate `content` for an artifact of `kind`, returning findings.
/// Malformed or incomplete content produces findings rather than errors —
/// validation is always tolerant.
pub fn validate(kind: &ArtifactKind, content: &str, path: &str) -> Vec<ValidationFinding> {
    let mut findings = Vec::new();
    let mut next_id = 1u32;

    // Structure checks per kind.
    match kind {
        ArtifactKind::Spec => {
            if !has_heading(content, "User Story") {
                findings.push(critical(
                    &mut next_id,
                    path,
                    "missing_user_stories",
                    "spec.md must contain at least one User Story section",
                ));
            }
            if !has_heading(content, "Requirement") && !has_line_prefix(content, "- **FR-") {
                findings.push(critical(
                    &mut next_id,
                    path,
                    "missing_requirements",
                    "spec.md must contain at least one requirement (FR-NNN)",
                ));
            }
            if !has_heading(content, "Success Criteria") {
                findings.push(warning(
                    &mut next_id,
                    path,
                    "missing_success_criteria",
                    "spec.md should contain a Success Criteria section",
                ));
            }
        }
        ArtifactKind::Plan => {
            if !has_heading(content, "Summary") {
                findings.push(critical(
                    &mut next_id,
                    path,
                    "missing_summary",
                    "plan.md must contain a Summary section",
                ));
            }
            if !has_heading(content, "Technical Context") {
                findings.push(critical(
                    &mut next_id,
                    path,
                    "missing_technical_context",
                    "plan.md must contain a Technical Context section",
                ));
            }
            if !has_heading(content, "Constitution Check") {
                findings.push(critical(
                    &mut next_id,
                    path,
                    "missing_constitution_check",
                    "plan.md must contain a Constitution Check section",
                ));
            }
        }
        ArtifactKind::Tasks => {
            // Tasks can legitimately be empty, but having no checkbox lines at all
            // is a warning (the file may be a stub).
            if !has_checkbox(content) {
                findings.push(warning(
                    &mut next_id,
                    path,
                    "no_tasks",
                    "tasks.md has no task checkbox lines",
                ));
            }
        }
        ArtifactKind::Checklist => {
            // Checklists with incomplete items block dependent steps.
            for (line_num, line) in content.lines().enumerate() {
                if line.trim().starts_with("- [ ]") {
                    findings.push(ValidationFinding {
                        finding_id: format!("v{next_id}"),
                        severity: Severity::Warning,
                        code: "incomplete_checklist_item".to_string(),
                        description: format!(
                            "Incomplete checklist item at line {}: {}",
                            line_num + 1,
                            line.trim()
                        ),
                        location: ArtifactLocation {
                            path: path.to_string(),
                            line_or_section: (line_num + 1).to_string(),
                        },
                        remediation: Some(
                            "Complete this item before the dependent step can run.".to_string(),
                        ),
                    });
                    next_id += 1;
                }
            }
        }
        ArtifactKind::Constitution => {
            if !content.contains("Version") && !content.contains("version") {
                findings.push(warning(
                    &mut next_id,
                    path,
                    "missing_version",
                    "constitution.md should declare a Version",
                ));
            }
        }
        _ => {}
    }

    // Unresolved marker scan (applies to all kinds).
    for (line_num, line) in content.lines().enumerate() {
        for marker in UNRESOLVED_MARKERS {
            if line.contains(marker) {
                findings.push(ValidationFinding {
                    finding_id: format!("v{next_id}"),
                    severity: Severity::Warning,
                    code: "unresolved_marker".to_string(),
                    description: format!("{} at line {}", marker, line_num + 1),
                    location: ArtifactLocation {
                        path: path.to_string(),
                        line_or_section: (line_num + 1).to_string(),
                    },
                    remediation: Some("Resolve this placeholder before dependent steps can run.".to_string()),
                });
                next_id += 1;
                break; // one finding per line is enough
            }
        }
    }

    findings
}

fn critical(id: &mut u32, path: &str, code: &str, desc: &str) -> ValidationFinding {
    let f = ValidationFinding {
        finding_id: format!("v{id}"),
        severity: Severity::Critical,
        code: code.to_string(),
        description: desc.to_string(),
        location: ArtifactLocation {
            path: path.to_string(),
            line_or_section: "1".to_string(),
        },
        remediation: None,
    };
    *id += 1;
    f
}

fn warning(id: &mut u32, path: &str, code: &str, desc: &str) -> ValidationFinding {
    let f = ValidationFinding {
        finding_id: format!("v{id}"),
        severity: Severity::Warning,
        code: code.to_string(),
        description: desc.to_string(),
        location: ArtifactLocation {
            path: path.to_string(),
            line_or_section: "1".to_string(),
        },
        remediation: None,
    };
    *id += 1;
    f
}

fn has_heading(content: &str, heading_fragment: &str) -> bool {
    let lower = heading_fragment.to_lowercase();
    content
        .lines()
        .any(|line| line.trim().starts_with('#') && line.to_lowercase().contains(&lower))
}

fn has_line_prefix(content: &str, prefix: &str) -> bool {
    content.lines().any(|line| line.trim_start().starts_with(prefix))
}

fn has_checkbox(content: &str) -> bool {
    content
        .lines()
        .any(|line| line.trim_start().starts_with("- ["))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_spec_has_no_critical_findings() {
        let md = "# Spec\n## User Story 1\n- **FR-001**: Do a thing\n## Success Criteria\n- It works\n";
        let findings = validate(&ArtifactKind::Spec, md, "spec.md");
        assert!(!findings.iter().any(|f| f.severity == Severity::Critical));
    }

    #[test]
    fn missing_plan_sections_produce_criticals() {
        let md = "# Plan\nThis has no required sections.\n";
        let findings = validate(&ArtifactKind::Plan, md, "plan.md");
        assert!(findings.iter().any(|f| f.code == "missing_summary"));
        assert!(findings.iter().any(|f| f.code == "missing_technical_context"));
        assert!(findings.iter().any(|f| f.code == "missing_constitution_check"));
    }

    #[test]
    fn unresolved_markers_are_flagged() {
        let md = "# Plan\n## Summary\nThis needs NEEDS CLARIFICATION before proceeding.\n";
        let findings = validate(&ArtifactKind::Plan, md, "plan.md");
        assert!(findings.iter().any(|f| f.code == "unresolved_marker"));
    }

    #[test]
    fn incomplete_checklist_items_are_flagged() {
        let md = "- [ ] Do something\n- [x] Done thing\n";
        let findings = validate(&ArtifactKind::Checklist, md, "checklist.md");
        assert_eq!(
            findings.iter().filter(|f| f.code == "incomplete_checklist_item").count(),
            1
        );
    }
}
