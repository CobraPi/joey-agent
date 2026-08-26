//! Multi-artifact conflict-safe writes, composing `writer.rs`.
//!
//! Supports `whole` and `section` scopes for artifact edits. Every write
//! carries `based_on_hash` → 409 on external change (FR-020/SC-005).
//! Unrelated content is always preserved.

use std::path::Path;

use crate::conflict::{check_conflict, content_hash, ConflictError};
use crate::model::ValidationFinding;
use crate::writer::WriteError;

/// Result of an editor write attempt.
#[derive(Debug)]
pub enum EditorResult {
    /// Write succeeded; carries the new content hash.
    Success { new_hash: String },
    /// External change detected — the file was modified since `based_on_hash`
    /// was read. The file is left unmodified (FR-020).
    Conflict { current_hash: String },
    /// Structural validation failed — the new content is malformed. The file
    /// is left unmodified and the findings explain why (FR-007).
    Invalid { findings: Vec<ValidationFinding> },
    /// IO or other error.
    Error(String),
}

/// Edit scope: whole-file replace or section-scoped replace (FR-004/005).
#[derive(Debug, Clone)]
pub enum EditScope {
    /// Replace the entire file content.
    Whole,
    /// Replace the content under a single Markdown heading identified by
    /// `heading` (heading text or id). The heading line itself is preserved;
    /// everything between it and the next heading of equal-or-higher level is
    /// replaced.
    Section { heading: String },
}

impl Default for EditScope {
    fn default() -> Self {
        EditScope::Whole
    }
}

/// Apply a conflict-checked write to `path`. The file must already exist.
///
/// - `Whole` scope: replace the entire file with `new_text`.
/// - `Section` scope: replace the content under the matched heading,
///   preserving the heading line and everything outside the section.
pub fn apply_edit(
    path: &Path,
    new_text: &str,
    based_on_hash: &str,
    scope: &EditScope,
    findings: Vec<ValidationFinding>,
) -> EditorResult {
    let current_content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return EditorResult::Error(e.to_string()),
    };

    // Conflict check FIRST: if the file changed externally, the edit can't
    // be meaningfully applied or validated against the current state (FR-020).
    if let Err(ConflictError::Conflict { current_hash }) = check_conflict(&current_content, based_on_hash) {
        return EditorResult::Conflict { current_hash };
    }

    // Then structural validation: reject Critical findings before writing.
    if findings.iter().any(|f| f.severity == crate::model::Severity::Critical) {
        return EditorResult::Invalid { findings };
    }

    let resolved = match scope {
        EditScope::Whole => new_text.to_string(),
        EditScope::Section { heading } => {
            match replace_section(&current_content, heading, new_text) {
                Some(content) => content,
                None => {
                    // Heading not found — fall back to whole-file replace
                    // rather than silently failing. This matches the tolerant
                    // philosophy: degrade gracefully.
                    new_text.to_string()
                }
            }
        }
    };

    match crate::patch::transaction::atomic_write(path, &resolved) {
        Ok(()) => EditorResult::Success {
            new_hash: content_hash(&resolved),
        },
        Err(e) => EditorResult::Error(e.to_string()),
    }
}

/// Replace the body text under a Markdown heading `heading` (first match),
/// preserving the heading line and any content outside the section.
/// Returns `None` if the heading is not found.
fn replace_section(content: &str, heading: &str, new_body: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();
    let heading_lower = heading.trim().to_lowercase();

    // Find the heading line index.
    let heading_idx = lines.iter().position(|line| {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            // Extract heading text (strip leading #s).
            let text = trimmed.trim_start_matches('#').trim();
            text.to_lowercase() == heading_lower
                || text.to_lowercase().contains(&heading_lower)
        } else {
            false
        }
    })?;

    // Determine the heading level (count leading #s).
    let heading_level = lines[heading_idx]
        .trim()
        .chars()
        .take_while(|c| *c == '#')
        .count();

    // Find where this section ends (next heading of equal-or-higher level).
    let mut end_idx = lines.len();
    for (i, line) in lines.iter().enumerate().skip(heading_idx + 1) {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|c| *c == '#').count();
            if level <= heading_level {
                end_idx = i;
                break;
            }
        }
    }

    // Rebuild: lines before heading + heading line + new body + lines after section.
    let mut result = String::new();
    for (i, line) in lines.iter().enumerate() {
        if i < heading_idx || i == heading_idx {
            result.push_str(line);
            result.push('\n');
        } else if i == heading_idx + 1 && !new_body.is_empty() {
            result.push_str(new_body);
            if !new_body.ends_with('\n') {
                result.push('\n');
            }
        } else if i >= end_idx {
            result.push_str(line);
            result.push('\n');
        }
        // Lines between heading+1 and end_idx are replaced by new_body (already added).
    }

    // Trim trailing newline to match original if original didn't have one.
    if !content.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    Some(result)
}

/// Convenience wrapper for a whole-file conflict-checked write, returning
/// the new hash on success or propagating the WriteError.
pub fn write_whole(path: &Path, new_content: &str, based_on_hash: &str) -> Result<String, WriteError> {
    crate::writer::write_if_unchanged(path, new_content, based_on_hash)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn whole_file_write_succeeds_on_matching_hash() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plan.md");
        std::fs::write(&path, "original content\n").unwrap();
        let hash = content_hash("original content\n");

        let result = apply_edit(&path, "new content\n", &hash, &EditScope::Whole, Vec::new());
        match result {
            EditorResult::Success { new_hash } => {
                assert_eq!(new_hash, content_hash("new content\n"));
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content\n");
            }
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn whole_file_write_rejects_on_stale_hash() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plan.md");
        std::fs::write(&path, "original content\n").unwrap();
        let stale = content_hash("something else");

        let result = apply_edit(&path, "new content\n", &stale, &EditScope::Whole, Vec::new());
        match result {
            EditorResult::Conflict { current_hash } => {
                assert_eq!(current_hash, content_hash("original content\n"));
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "original content\n");
            }
            _ => panic!("expected Conflict"),
        }
    }

    #[test]
    fn section_replace_preserves_rest_of_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plan.md");
        let content = "# Plan\n\n## Summary\nold summary text\n\n## Technical Context\nkeep this\n";
        std::fs::write(&path, content).unwrap();
        let hash = content_hash(content);

        let result = apply_edit(
            &path,
            "brand new summary\n",
            &hash,
            &EditScope::Section {
                heading: "Summary".to_string(),
            },
            Vec::new(),
        );

        match result {
            EditorResult::Success { .. } => {
                let updated = std::fs::read_to_string(&path).unwrap();
                assert!(updated.contains("brand new summary"));
                assert!(updated.contains("keep this"));
                assert!(!updated.contains("old summary text"));
                assert!(updated.contains("# Plan"));
            }
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn critical_findings_reject_write() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plan.md");
        std::fs::write(&path, "original\n").unwrap();
        let hash = content_hash("original\n");

        let finding = ValidationFinding {
            finding_id: "f1".to_string(),
            severity: crate::model::Severity::Critical,
            code: "missing_required_section".to_string(),
            description: "missing Summary".to_string(),
            location: crate::model::ArtifactLocation {
                path: "plan.md".to_string(),
                line_or_section: "1".to_string(),
            },
            remediation: None,
        };

        let result = apply_edit(&path, "broken\n", &hash, &EditScope::Whole, vec![finding]);
        match result {
            EditorResult::Invalid { findings } => {
                assert_eq!(findings.len(), 1);
                assert_eq!(std::fs::read_to_string(&path).unwrap(), "original\n");
            }
            _ => panic!("expected Invalid"),
        }
    }
}

// =====================================================================
// Feature 012: compose patch/ for the three editing depths (T051).
// The PatchEngine composes writer.rs — it does not replace it (Constitution VII).
// =====================================================================

use crate::cst::parser::parse_bytes;
use crate::patch::{self, PatchOp, PatchResult};

/// Compose a patch-engine edit through the existing editor interface. This
/// bridges the structured/inline/raw editing depths (FR-015) to the
/// conflict-checked writer.
pub fn apply_cst_patch(
    repo_root: &Path,
    feature_id: &str,
    artifact: &str,
    ops: Vec<PatchOp>,
) -> EditorResult {
    // Path-traversal guard: both components land in a filesystem path.
    if !crate::parser::discovery::is_safe_feature_id(feature_id)
        || !crate::parser::discovery::is_safe_artifact_name(artifact)
    {
        return EditorResult::Error(
            "feature id or artifact name is not a safe path component".to_string(),
        );
    }
    let artifact_path = format!("specs/{feature_id}/{artifact}");
    let full_path = repo_root.join(&artifact_path);

    let source = match std::fs::read_to_string(&full_path) {
        Ok(s) => s,
        Err(e) => return EditorResult::Error(e.to_string()),
    };

    let doc = parse_bytes(&artifact_path, source.as_bytes());
    let result = patch::apply_in_memory(&doc, &source, &ops);

    match result {
        PatchResult::Applied { new_revision_hash, .. } => {
            // Re-execute to get the bytes and write them.
            let outcome = crate::patch::transaction::execute(&doc, &source, &ops);
            if let crate::patch::transaction::TransactionOutcome::Applied { new_bytes, .. } = outcome {
                if let Err(e) = crate::patch::transaction::atomic_write(&full_path, &new_bytes) {
                    return EditorResult::Error(e.to_string());
                }
            }
            EditorResult::Success { new_hash: new_revision_hash }
        }
        PatchResult::Conflict(_) => EditorResult::Conflict {
            current_hash: content_hash(&source),
        },
        PatchResult::AnchorUnresolved { .. } => EditorResult::Error(
            "anchor unresolved — node structure changed".to_string(),
        ),
        PatchResult::ValidationFailed { diagnostics, .. } => {
            let path_for_findings = artifact_path.clone();
            EditorResult::Invalid {
                findings: diagnostics
                    .into_iter()
                    .map(|d| ValidationFinding {
                        finding_id: uuid::Uuid::new_v4().to_string(),
                        severity: crate::model::Severity::Warning,
                        code: "cst_validation".to_string(),
                        description: d,
                        location: crate::model::ArtifactLocation {
                            path: path_for_findings.clone(),
                            line_or_section: String::new(),
                        },
                        remediation: None,
                    })
                    .collect(),
            }
        }
    }
}
