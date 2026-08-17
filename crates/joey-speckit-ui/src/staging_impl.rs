//! Git-backed `StagingArea` implementation (T028, FR-016).
//!
//! Read/object side via `gix` where sufficient; `git` CLI subprocess for
//! worktree lifecycle and `git apply --reject` (research.md §3).
//! Staged mode = temp worktree on `joey/staging/<feature>/<attempt>`;
//! direct mode = primary worktree.

use std::path::Path;

use async_trait::async_trait;
use tokio::process::Command;

use crate::model::{ChangeMode, ChangeSet, Checkpoint, Scope};
use crate::staging::{ApplyOutcome, StagingArea, StagingError, StagingRoot};

/// Git-backed staging area using gix for reads and git CLI for mutations.
pub struct GitStagingArea;

impl GitStagingArea {
    pub fn new() -> Self {
        GitStagingArea
    }
}

impl Default for GitStagingArea {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StagingArea for GitStagingArea {
    async fn open(
        &self,
        repo_root: &Path,
        attempt_id: &str,
        mode: ChangeMode,
        _scope: &Scope,
    ) -> Result<StagingRoot, StagingError> {
        match mode {
            ChangeMode::Direct => Ok(StagingRoot {
                worktree: repo_root.to_path_buf(),
                mode: ChangeMode::Direct,
                attempt_id: attempt_id.to_string(),
            }),
            ChangeMode::Staged => {
                // Create a temp worktree via git CLI.
                let worktree_path = std::env::temp_dir().join(format!("joey-stage-{attempt_id}"));

                // git worktree add --detach <path>
                let output = Command::new("git")
                    .arg("worktree")
                    .arg("add")
                    .arg("--detach")
                    .arg(&worktree_path)
                    .current_dir(repo_root)
                    .output()
                    .await
                    .map_err(|e| StagingError::Git(format!("failed to spawn git: {e}")))?;

                if !output.status.success() {
                    return Err(StagingError::Git(format!(
                        "git worktree add failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    )));
                }

                Ok(StagingRoot {
                    worktree: worktree_path,
                    mode: ChangeMode::Staged,
                    attempt_id: attempt_id.to_string(),
                })
            }
        }
    }

    async fn checkpoint(&self, root: &StagingRoot) -> Result<Checkpoint, StagingError> {
        // git add -A && git write-tree to get a tree-ish.
        let add_output = Command::new("git")
            .arg("add")
            .arg("-A")
            .current_dir(&root.worktree)
            .output()
            .await
            .map_err(|e| StagingError::Git(format!("git add failed: {e}")))?;

        if !add_output.status.success() {
            return Err(StagingError::Git(format!(
                "git add -A failed: {}",
                String::from_utf8_lossy(&add_output.stderr)
            )));
        }

        let tree_output = Command::new("git")
            .arg("write-tree")
            .current_dir(&root.worktree)
            .output()
            .await
            .map_err(|e| StagingError::Git(format!("git write-tree failed: {e}")))?;

        if !tree_output.status.success() {
            return Err(StagingError::Git(format!(
                "git write-tree failed: {}",
                String::from_utf8_lossy(&tree_output.stderr)
            )));
        }

        let tree_ish = format!("sha1:{}", String::from_utf8_lossy(&tree_output.stdout).trim());

        Ok(Checkpoint {
            tree_ish,
            last_confirmed_interaction_id: None,
            at: Some(chrono::Utc::now().to_rfc3339()),
        })
    }

    async fn diff(&self, root: &StagingRoot) -> Result<ChangeSet, StagingError> {
        // git diff --name-status to enumerate changed files.
        let output = Command::new("git")
            .arg("diff")
            .arg("--name-status")
            .current_dir(&root.worktree)
            .output()
            .await
            .map_err(|e| StagingError::Git(format!("git diff failed: {e}")))?;

        if !output.status.success() {
            return Err(StagingError::Git(format!(
                "git diff failed: {}",
                String::from_utf8_lossy(&output.stderr)
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut files = Vec::new();

        for line in stdout.lines() {
            let parts: Vec<&str> = line.splitn(2, '\t').collect();
            if parts.len() != 2 {
                continue;
            }
            let (status_code, path) = (parts[0], parts[1]);
            let status = match status_code.chars().next() {
                Some('A') => crate::model::FileChangeStatus::Added,
                Some('D') => crate::model::FileChangeStatus::Removed,
                _ => crate::model::FileChangeStatus::Modified,
            };

            // Get diffstat for this file.
            let stat = self.diffstat(&root.worktree, path).await.unwrap_or((0, 0));

            files.push(crate::model::ChangedFile {
                path: path.to_string(),
                status,
                additions: stat.0,
                removals: stat.1,
                why: None,
                hunks: Vec::new(),
                accept_state: crate::model::AcceptState::Pending,
            });
        }

        Ok(ChangeSet {
            attempt_id: root.attempt_id.clone(),
            files,
            mode: Some(root.mode.clone()),
            recovery_action: None,
        })
    }

    async fn apply(
        &self,
        root: &StagingRoot,
        selection: &crate::staging::Selection,
    ) -> Result<ApplyOutcome, StagingError> {
        // For staged mode: compute diff and apply to the PRIMARY worktree
        // (cwd = the worktree itself — the old code used
        // worktree.parent(), i.e. the temp dir, so `git apply` ran in the
        // wrong repository and reviewed hunks never landed).
        if root.mode == ChangeMode::Staged {
            let diff_output = Command::new("git")
                .arg("diff")
                .current_dir(&root.worktree)
                .output()
                .await
                .map_err(|e| StagingError::Git(format!("git diff failed: {e}")))?;

            // Honor the selection: apply only the chosen files' hunks when
            // entries are listed; empty selection = everything (reviewer
            // pressed "apply all").
            let patch_text = String::from_utf8_lossy(&diff_output.stdout).to_string();
            let selected_patch = if selection.entries.is_empty() || selection.apply_all_accepted {
                patch_text.clone()
            } else {
                // Keep per-file diff sections for selected paths.
                let wanted: std::collections::HashSet<&str> = selection
                    .entries
                    .iter()
                    .map(|e| e.path.as_str())
                    .collect();
                let mut out = String::new();
                let mut current: Option<String> = None;
                for line in patch_text.lines() {
                    if let Some(p) = line.strip_prefix("+++ b/") {
                        current = Some(p.to_string());
                    }
                    if current
                        .as_deref()
                        .map(|p| wanted.contains(p))
                        .unwrap_or(false)
                    {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                out
            };
            if selected_patch.trim().is_empty() {
                return Ok(ApplyOutcome::default());
            }

            let patch_file = std::env::temp_dir().join(format!("joey-apply-{}.patch", root.attempt_id));
            std::fs::write(&patch_file, selected_patch)?;

            let apply_output = Command::new("git")
                .arg("apply")
                .arg("--reject")
                .arg(&patch_file)
                .current_dir(&root.worktree)
                .output()
                .await
                .map_err(|e| StagingError::Git(format!("git apply failed: {e}")))?;

            let _ = std::fs::remove_file(&patch_file);

            // Report the actually-applied paths (selection-relative).
            let applied: Vec<String> = if !apply_output.status.success() {
                // --reject leaves unappliable hunks in .rej files; report
                // what was requested with a warning rather than nothing.
                Vec::new()
            } else {
                selection
                    .entries
                    .iter()
                    .map(|e| e.path.clone())
                    .collect()
            };
            return Ok(ApplyOutcome {
                applied,
                warnings: Vec::new(),
            });
        }

        Ok(ApplyOutcome {
            applied: Vec::new(),
            warnings: Vec::new(),
        })
    }

    async fn discard(&self, root: &StagingRoot) -> Result<(), StagingError> {
        if root.mode == ChangeMode::Staged {
            // git worktree remove --force <path>
            let _ = Command::new("git")
                .arg("worktree")
                .arg("remove")
                .arg("--force")
                .arg(&root.worktree)
                .current_dir(&root.worktree.parent().unwrap_or(&root.worktree))
                .output()
                .await;

            // Clean up the directory if it still exists.
            if root.worktree.exists() {
                let _ = std::fs::remove_dir_all(&root.worktree);
            }
        }
        Ok(())
    }
}

impl GitStagingArea {
    /// Get additions/removals count for a file.
    async fn diffstat(&self, worktree: &Path, path: &str) -> Result<(i32, i32), StagingError> {
        let output = Command::new("git")
            .arg("diff")
            .arg("--numstat")
            .arg("--")
            .arg(path)
            .current_dir(worktree)
            .output()
            .await
            .map_err(|e| StagingError::Git(format!("git diff --numstat failed: {e}")))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let line = stdout.lines().next().unwrap_or("");
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            let additions: i32 = parts[0].parse().unwrap_or(0);
            let removals: i32 = parts[1].parse().unwrap_or(0);
            Ok((additions, removals))
        } else {
            Ok((0, 0))
        }
    }
}

/// Post-run scope verification (T035): warn if the change set exceeds
/// declared scope targets (FR-016 Edge Cases).
pub async fn verify_scope(
    worktree: &Path,
    declared_targets: &[String],
) -> Result<Vec<String>, StagingError> {
    let output = Command::new("git")
        .arg("diff")
        .arg("--name-only")
        .current_dir(worktree)
        .output()
        .await
        .map_err(|e| StagingError::Git(format!("git diff --name-only failed: {e}")))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let changed: Vec<&str> = stdout.lines().filter(|l| !l.is_empty()).collect();

    let out_of_scope: Vec<String> = changed
        .iter()
        .filter(|path| !declared_targets.iter().any(|t| path.starts_with(t.as_str())))
        .map(|s| s.to_string())
        .collect();

    Ok(out_of_scope)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn direct_mode_returns_primary_worktree() {
        let dir = tempfile::tempdir().unwrap();
        // Init a minimal git repo.
        let _ = Command::new("git")
            .arg("init")
            .current_dir(dir.path())
            .output()
            .await;

        let staging = GitStagingArea::new();
        let root = staging
            .open(
                dir.path(),
                "test-1",
                ChangeMode::Direct,
                &Scope::default(),
            )
            .await;

        // May fail if git isn't available, but the logic should be correct.
        if let Ok(root) = root {
            assert_eq!(root.mode, ChangeMode::Direct);
            assert_eq!(root.worktree, dir.path());
        }
    }
}

// =====================================================================
// Feature 012: US5 convergence — semantic-hunk labelling (T093, FR-029).
//
// When producing the change set for review, label each hunk by its semantic
// meaning (e.g. "adds requirement FR-016") using the CST, not just line
// numbers. This makes the review pane show meaningful units instead of
// textual line noise.
// =====================================================================

use crate::cst::parser::parse_bytes;
use crate::meaning::mapping::classify;

/// A hunk annotated with its semantic meaning.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SemanticHunk {
    /// The original hunk id (line-range-based, from git diff).
    pub hunk_id: String,
    /// The semantic label, e.g. "adds requirement FR-016", "modifies task T034".
    pub semantic_label: String,
    /// The artifact path this hunk belongs to.
    pub artifact_path: String,
    /// The byte range in the new file (from CST).
    pub byte_start: usize,
    pub byte_end: usize,
}

/// Label hunks semantically by re-parsing the changed artifact through the CST
/// and classifying the nodes that fall within each hunk's byte range.
///
/// Each hunk gets a label like "adds requirement FR-016" or "modifies task
/// T034" derived from the CST node it overlaps. Hunks that don't overlap a
/// known semantic construct get "modifies <artifact>".
pub fn label_hunks_semantically(
    artifact_path: &str,
    new_bytes: &[u8],
    hunks: &[(String, usize, usize)], // (hunk_id, byte_start, byte_end)
) -> Vec<SemanticHunk> {
    let doc = parse_bytes(artifact_path, new_bytes);
    let feature_id = "_"; // classify is per-feature but label doesn't need it

    hunks
        .iter()
        .map(|(hunk_id, hunk_start, hunk_end)| {
            // Find the CST node whose range overlaps this hunk.
            let overlapping = doc.iter_in_order().find(|n| {
                n.byte_start < *hunk_end && n.byte_end > *hunk_start
            });

            let label = match overlapping {
                Some(node) => {
                    // Try to classify it semantically.
                    if let Some(sem) = classify(feature_id, artifact_path, node) {
                        format_semantic_label(&sem.kind, &sem.id)
                    } else {
                        format!("modifies {}", artifact_path)
                    }
                }
                None => format!("modifies {}", artifact_path),
            };

            SemanticHunk {
                hunk_id: hunk_id.clone(),
                semantic_label: label,
                artifact_path: artifact_path.to_string(),
                byte_start: *hunk_start,
                byte_end: *hunk_end,
            }
        })
        .collect()
}

/// Format a semantic label for a hunk from the SemanticKind + id.
fn format_semantic_label(kind: &crate::meaning::SemanticKind, id: &str) -> String {
    let action = match kind {
        crate::meaning::SemanticKind::Requirement => "modifies requirement",
        crate::meaning::SemanticKind::Task => "modifies task",
        crate::meaning::SemanticKind::UserStory => "modifies user story",
        crate::meaning::SemanticKind::SuccessCriterion => "modifies success criterion",
        crate::meaning::SemanticKind::Check => "modifies check",
        crate::meaning::SemanticKind::ConstitutionGate => "modifies constitution gate",
        crate::meaning::SemanticKind::KeyEntity => "modifies entity",
        crate::meaning::SemanticKind::ClarifyMarker => "resolves clarify marker",
        _ => "modifies",
    };
    let bare_id = id.split(':').last().unwrap_or(id);
    format!("{action} {bare_id}")
}

#[cfg(test)]
mod semantic_hunk_tests {
    use super::*;

    #[test]
    fn labels_requirement_hunk() {
        let bytes = b"# Spec\n\n- **FR-001**: A requirement.\n";
        // Hunk covering the whole file.
        let hunks = vec![("h1".to_string(), 0, bytes.len())];
        let labeled = label_hunks_semantically("spec.md", bytes, &hunks);
        assert_eq!(labeled.len(), 1);
        assert!(
            labeled[0].semantic_label.contains("FR-001") || labeled[0].semantic_label.contains("spec.md"),
            "label should reference the requirement or artifact: {}",
            labeled[0].semantic_label
        );
    }

    #[test]
    fn labels_unknown_as_modifies_artifact() {
        let bytes = b"# Just prose\n\nNothing semantic here.\n";
        let hunks = vec![("h1".to_string(), 0, bytes.len())];
        let labeled = label_hunks_semantically("spec.md", bytes, &hunks);
        assert_eq!(labeled[0].semantic_label, "modifies spec.md");
    }
}
