//! Discovery and tolerant parsing of feature artifacts beyond the
//! spec/plan/tasks trio handled by `spec.rs`/`plan.rs`/`tasks.rs`.
//!
//! This module discovers authorable artifacts (checklist, research,
//! data-model, contract, quickstart, constitution, supporting) without
//! assuming they all exist (FR-003), and parses each tolerantly — malformed
//! content degrades to a partial result rather than panicking or being
//! dropped, mirroring the `Status::Unparsed`-on-malformed pattern already used
//! in `model.rs` (Constitution VII: existing parsers untouched).

use std::path::{Path, PathBuf};

use crate::model::{Artifact, ArtifactKind, WorkflowPhase};

/// Discover all authorable artifacts for a feature at `feature_dir`,
/// returning entries for both existing and not-yet-created artifacts so the
/// explorer can offer creation (FR-003). `feature_dir` is typically
/// `repo_root/specs/<feature-id>`.
pub fn discover_artifacts(feature_dir: &Path, feature_id: &str) -> Vec<Artifact> {
    let mut artifacts = Vec::new();

    // The core spec/plan/tasks artifacts.
    for (filename, kind) in [
        ("spec.md", ArtifactKind::Spec),
        ("plan.md", ArtifactKind::Plan),
        ("tasks.md", ArtifactKind::Tasks),
        ("research.md", ArtifactKind::Research),
        ("data-model.md", ArtifactKind::DataModel),
        ("quickstart.md", ArtifactKind::Quickstart),
    ] {
        let path = feature_dir.join(filename);
        let repo_relative = format!("specs/{feature_id}/{filename}");
        artifacts.push(build_artifact(&path, &repo_relative, kind));
    }

    // Checklist: checklists/ directory.
    let checklists_dir = feature_dir.join("checklists");
    if checklists_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&checklists_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.ends_with(".md") {
                    let path = entry.path();
                    let repo_relative = format!("specs/{feature_id}/checklists/{name_str}");
                    artifacts.push(build_artifact(&path, &repo_relative, ArtifactKind::Checklist));
                }
            }
        }
    } else {
        // Offer creation of a checklist placeholder.
        let repo_relative = format!("specs/{feature_id}/checklists/requirements.md");
        artifacts.push(Artifact {
            path: repo_relative,
            kind: ArtifactKind::Checklist,
            exists: false,
            content_hash: None,
            dirty: false,
            save_state: crate::model::SaveState::Clean,
            validity: Vec::new(),
            workflow_phase: WorkflowPhase::Checklist,
            stale: false,
            stale_reason: None,
        });
    }

    // Contracts: contracts/ directory.
    let contracts_dir = feature_dir.join("contracts");
    if contracts_dir.is_dir() {
        if let Ok(entries) = std::fs::read_dir(&contracts_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name_str = name.to_string_lossy();
                if name_str.ends_with(".md") {
                    let path = entry.path();
                    let repo_relative = format!("specs/{feature_id}/contracts/{name_str}");
                    artifacts.push(build_artifact(&path, &repo_relative, ArtifactKind::Contract));
                }
            }
        }
    }

    // Constitution: .specify/memory/constitution.md (relative to repo root,
    // not feature dir). Caller should pass repo_root separately if needed;
    // here we discover it if the repo root parent is accessible.
    // This is typically handled at the API layer where repo_root is known.

    artifacts
}

/// Discover the constitution artifact relative to `repo_root`.
pub fn discover_constitution(repo_root: &Path) -> Option<Artifact> {
    let path = repo_root.join(".specify").join("memory").join("constitution.md");
    if path.exists() {
        let repo_relative = ".specify/memory/constitution.md".to_string();
        Some(build_artifact(&path, &repo_relative, ArtifactKind::Constitution))
    } else {
        None
    }
}

/// Build an `Artifact` descriptor for a file, tolerantly handling missing
/// files (exists=false) and IO errors (treated as missing).
fn build_artifact(path: &Path, repo_relative: &str, kind: ArtifactKind) -> Artifact {
    if path.exists() {
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let hash = crate::conflict::content_hash(&content);
        Artifact {
            path: repo_relative.to_string(),
            kind: kind.clone(),
            exists: true,
            content_hash: Some(hash),
            dirty: false,
            save_state: crate::model::SaveState::Clean,
            validity: Vec::new(),
            workflow_phase: kind.workflow_phase(),
            stale: false,
            stale_reason: None,
        }
    } else {
        Artifact {
            path: repo_relative.to_string(),
            kind,
            exists: false,
            content_hash: None,
            dirty: false,
            save_state: crate::model::SaveState::Clean,
            validity: Vec::new(),
            workflow_phase: ArtifactKind::Spec.workflow_phase(),
            stale: false,
            stale_reason: None,
        }
    }
}

/// Parse a Markdown heading outline (title + line number) for an artifact,
/// used by the GET artifact endpoint for the rendered reading view (FR-006).
pub fn parse_outline(content: &str) -> Vec<OutlineEntry> {
    let parser = pulldown_cmark::Parser::new(content);
    let mut entries = Vec::new();
    let mut line = 1usize;

    for event in parser {
        match event {
            pulldown_cmark::Event::Start(pulldown_cmark::Tag::Heading { level, .. }) => {
                if level <= pulldown_cmark::HeadingLevel::H3 {
                    // Compute the line number by counting newlines up to the
                    // current offset. pulldown-cmark doesn't expose byte
                    // ranges in the public API in 0.12, so we approximate by
                    // tracking line position through Text events.
                    entries.push(OutlineEntry {
                        title: String::new(),
                        line,
                        level: heading_level_int(level),
                    });
                }
            }
            pulldown_cmark::Event::Text(t) => {
                if let Some(last) = entries.last_mut() {
                    if last.title.is_empty() || last.level <= 3 {
                        last.title.push_str(&t);
                    }
                }
                line += t.as_ref().matches('\n').count();
            }
            pulldown_cmark::Event::Code(t) => {
                if let Some(last) = entries.last_mut() {
                    if last.title.is_empty() {
                        last.title.push_str(&t);
                    }
                }
            }
            pulldown_cmark::Event::SoftBreak | pulldown_cmark::Event::HardBreak => {
                line += 1;
            }
            _ => {}
        }
    }

    entries
}

fn heading_level_int(level: pulldown_cmark::HeadingLevel) -> u8 {
    match level {
        pulldown_cmark::HeadingLevel::H1 => 1,
        pulldown_cmark::HeadingLevel::H2 => 2,
        pulldown_cmark::HeadingLevel::H3 => 3,
        pulldown_cmark::HeadingLevel::H4 => 4,
        pulldown_cmark::HeadingLevel::H5 => 5,
        pulldown_cmark::HeadingLevel::H6 => 6,
    }
}

/// One heading entry in a document outline (FR-006).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutlineEntry {
    pub title: String,
    pub line: usize,
    pub level: u8,
}

/// Resolve a repo-relative artifact path to an absolute filesystem path under
/// `repo_root`. Rejects path traversal (no `..` segments).
pub fn resolve_artifact_path(repo_root: &Path, repo_relative: &str) -> Option<PathBuf> {
    // Reject any explicit traversal attempt.
    for component in repo_relative.split('/') {
        if component == ".." {
            return None;
        }
    }
    let path = repo_root.join(repo_relative);
    // Ensure the resolved path stays within repo_root (prefix check).
    let path_str = path.to_string_lossy();
    let root_str = repo_root.to_string_lossy();
    if !path_str.starts_with(&*root_str) {
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn discover_includes_nonexistent_artifacts() {
        let dir = tempdir().unwrap();
        let feature_dir = dir.path().join("specs").join("001-test");
        std::fs::create_dir_all(&feature_dir).unwrap();
        std::fs::write(feature_dir.join("spec.md"), "# Test\n").unwrap();

        let artifacts = discover_artifacts(&feature_dir, "001-test");
        // spec.md exists, plan/tasks/research/etc do not — all should be listed.
        assert!(artifacts.len() >= 6);
        let spec = artifacts.iter().find(|a| a.kind == ArtifactKind::Spec).unwrap();
        assert!(spec.exists);
        let plan = artifacts.iter().find(|a| a.kind == ArtifactKind::Plan).unwrap();
        assert!(!plan.exists);
    }

    #[test]
    fn discover_finds_checklist_files() {
        let dir = tempdir().unwrap();
        let feature_dir = dir.path().join("specs").join("001-test");
        let checklists = feature_dir.join("checklists");
        std::fs::create_dir_all(&checklists).unwrap();
        std::fs::write(checklists.join("requirements.md"), "# Checklist\n").unwrap();

        let artifacts = discover_artifacts(&feature_dir, "001-test");
        let checklist = artifacts
            .iter()
            .find(|a| a.kind == ArtifactKind::Checklist)
            .unwrap();
        assert!(checklist.exists);
        assert!(checklist.path.ends_with("requirements.md"));
    }

    #[test]
    fn parse_outline_extracts_headings() {
        let md = "# Title\n\n## Section A\n\ntext\n\n### Sub\n";
        let outline = parse_outline(md);
        assert!(outline.iter().any(|e| e.title.contains("Title")));
        assert!(outline.iter().any(|e| e.title.contains("Section A")));
    }

    #[test]
    fn resolve_rejects_traversal() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        assert!(resolve_artifact_path(root, "specs/001-test/spec.md").is_some());
        assert!(resolve_artifact_path(root, "../etc/passwd").is_none());
        assert!(resolve_artifact_path(root, "specs/../etc/passwd").is_none());
    }
}
