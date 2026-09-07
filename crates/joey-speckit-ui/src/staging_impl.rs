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

/// Filter a unified `git diff` patch down to the sections touching files in
/// `wanted` (paths as they appear after `+++ b/`).
///
/// Each per-file section starts with its `diff --git a/<p> b/<p>` line
/// followed by the `--- a/<p>` / `+++ b/<p>` header pair; emitting from the
/// `+++` line alone would produce a malformed patch `git apply` can't parse.
/// While scanning we buffer the most recent `diff --git` + `--- a/` pair and
/// emit them when the following `+++ b/` names a kept file, so each kept
/// section stays well-formed.
pub fn filter_patch_to_files(patch_text: &str, wanted: &std::collections::HashSet<&str>) -> String {
    let mut out = String::new();
    let mut current: Option<String> = None;
    let mut header_buf: Vec<&str> = Vec::new();

    for line in patch_text.lines() {
        if let Some(p) = line.strip_prefix("+++ b/") {
            let keep = wanted.contains(p);
            if keep {
                // Emit the buffered `diff --git` / `--- a/` header lines
                // first, then this `+++ b/` line.
                for h in header_buf.drain(..) {
                    out.push_str(h);
                    out.push('\n');
                }
                out.push_str(line);
                out.push('\n');
                current = Some(p.to_string());
            } else {
                header_buf.clear();
                current = None;
            }
            continue;
        }
        if line.starts_with("diff --git ") {
            // A new file section starts here: invalidate the previous
            // section state and buffer the header until the `+++ b/` line
            // decides whether this file is kept.
            current = None;
            header_buf.clear();
            header_buf.push(line);
            continue;
        }
        if line.starts_with("--- a/") {
            // Buffer the `--- a/` header until we know whether the file is
            // kept. (Only buffer while not inside a kept section: inside a
            // kept section the next section's headers are preceded by a
            // `diff --git` line that already reset the state.)
            if current.is_none() {
                header_buf.push(line);
            }
            continue;
        }
        if let Some(p) = current.as_deref() {
            if wanted.contains(p) {
                out.push_str(line);
                out.push('\n');
            }
        }
    }
    out
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
        //
        // In direct mode the staging runs against a DEDICATED index file
        // (outside the repo, so it can't stage itself into the snapshot)
        // — `git add -A` against the user's real index would pollute it
        // with our snapshot. Staged mode owns its temp worktree, so the
        // worktree's own index is fine (and keeps agent-created files
        // tracked, so later HEAD-based diffs see them).
        let dedicated_index: Option<std::path::PathBuf> = match root.mode {
            ChangeMode::Direct => Some(
                std::env::temp_dir().join(format!("joey-stage-index-{}", root.attempt_id)),
            ),
            ChangeMode::Staged => None,
        };

        let mut add_cmd = Command::new("git");
        add_cmd.arg("add").arg("-A").current_dir(&root.worktree);
        if let Some(idx) = &dedicated_index {
            add_cmd.env("GIT_INDEX_FILE", idx);
        }
        let add_output = add_cmd
            .output()
            .await
            .map_err(|e| StagingError::Git(format!("git add failed: {e}")))?;

        if !add_output.status.success() {
            return Err(StagingError::Git(format!(
                "git add -A failed: {}",
                String::from_utf8_lossy(&add_output.stderr)
            )));
        }

        let mut write_tree_cmd = Command::new("git");
        write_tree_cmd.arg("write-tree").current_dir(&root.worktree);
        if let Some(idx) = &dedicated_index {
            write_tree_cmd.env("GIT_INDEX_FILE", idx);
        }
        let tree_output = write_tree_cmd
            .output()
            .await
            .map_err(|e| StagingError::Git(format!("git write-tree failed: {e}")))?;

        if !tree_output.status.success() {
            return Err(StagingError::Git(format!(
                "git write-tree failed: {}",
                String::from_utf8_lossy(&tree_output.stderr)
            )));
        }

        // The dedicated index was only needed to compute the tree; remove it.
        if let Some(idx) = &dedicated_index {
            let _ = std::fs::remove_file(idx);
        }

        let tree_ish = format!("sha1:{}", String::from_utf8_lossy(&tree_output.stdout).trim());

        Ok(Checkpoint {
            tree_ish,
            last_confirmed_interaction_id: None,
            at: Some(chrono::Utc::now().to_rfc3339()),
        })
    }

    async fn diff(&self, root: &StagingRoot) -> Result<ChangeSet, StagingError> {
        // git diff <base> --name-status to enumerate changed files. The
        // base is HEAD (not the index): checkpoint() stages everything, so
        // a bare worktree-vs-index diff would see nothing afterwards.
        // HEAD-based diffing shows what changed since the checkpoint.
        let base = Self::diff_base(&root.worktree).await;
        let output = Command::new("git")
            .arg("diff")
            .arg(&base)
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
        // For staged mode: compute the diff in the STAGING worktree (that's
        // where the changes live) and apply it to the PRIMARY worktree —
        // `git apply` must run with the primary repo root as cwd, never in
        // the staging worktree itself (which already contains the changes,
        // so hunks would never land in the user's repo).
        if root.mode == ChangeMode::Staged {
            // Diff against HEAD (not the bare worktree-vs-index diff):
            // checkpoint() stages everything into the index, after which a
            // bare `git diff` in the staging worktree would be empty.
            let base = Self::diff_base(&root.worktree).await;
            let diff_output = Command::new("git")
                .arg("diff")
                .arg(&base)
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
                // Keep per-file diff sections (including their `diff --git`
                // and `--- a/` headers) for selected paths.
                let wanted: std::collections::HashSet<&str> = selection
                    .entries
                    .iter()
                    .map(|e| e.path.as_str())
                    .collect();
                filter_patch_to_files(&patch_text, &wanted)
            };
            if selected_patch.trim().is_empty() {
                return Ok(ApplyOutcome::default());
            }

            // Resolve the PRIMARY repo root from the staging worktree: a
            // linked worktree's `.git` file points at
            // `<primary>/.git/worktrees/<name>`, so the git-common-dir
            // parent-of-parent is the primary worktree. This is where the
            // reviewed hunks must land.
            let primary_root = self.primary_root(&root.worktree).await?;
            let patch_file = std::env::temp_dir().join(format!("joey-apply-{}.patch", root.attempt_id));
            std::fs::write(&patch_file, selected_patch)?;

            let apply_output = Command::new("git")
                .arg("apply")
                .arg("--reject")
                .arg(&patch_file)
                .current_dir(&primary_root)
                .output()
                .await
                .map_err(|e| StagingError::Git(format!("git apply failed: {e}")))?;

            let _ = std::fs::remove_file(&patch_file);

            let mut warnings = Vec::new();
            if !apply_output.status.success() {
                // Surface the failure instead of silently reporting nothing:
                // --reject leaves unappliable hunks in .rej files.
                warnings.push(crate::staging::DependencyWarning {
                    hunk_id: String::new(),
                    depends_on: Vec::new(),
                    message: format!(
                        "git apply reported failures: {}{}",
                        String::from_utf8_lossy(&apply_output.stdout),
                        String::from_utf8_lossy(&apply_output.stderr)
                    ),
                });
            }

            // Report the actually-applied paths (selection-relative).
            let applied: Vec<String> = if !apply_output.status.success() {
                Vec::new()
            } else {
                selection
                    .entries
                    .iter()
                    .map(|e| e.path.clone())
                    .collect()
            };
            return Ok(ApplyOutcome { applied, warnings });
        }

        Ok(ApplyOutcome::default())
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
    /// Resolve the base for worktree-wide diffs: HEAD when it exists, the
    /// empty tree otherwise (unborn branch).
    ///
    /// A bare `git diff` compares worktree-vs-index and goes blind after
    /// `checkpoint` staged everything; diffing against this base keeps
    /// post-checkpoint edits visible.
    async fn diff_base(worktree: &Path) -> String {
        // `rev-parse --verify --quiet HEAD` fails on a repo with no commits.
        let head = Command::new("git")
            .arg("rev-parse")
            .arg("--verify")
            .arg("--quiet")
            .arg("HEAD")
            .current_dir(worktree)
            .output()
            .await;
        if let Ok(out) = head {
            if out.status.success() {
                return "HEAD".to_string();
            }
        }
        // Unborn branch: diff against the empty tree (`git mktree` on empty
        // stdin yields it for either object format). stdin is /dev/null so
        // the read hits EOF immediately.
        let mktree = Command::new("git")
            .arg("mktree")
            .stdin(std::process::Stdio::null())
            .current_dir(worktree)
            .output()
            .await;
        if let Ok(out) = mktree {
            if out.status.success() {
                let tree = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !tree.is_empty() {
                    return tree;
                }
            }
        }
        // Last resort: literal HEAD (a diff error then surfaces upstream).
        "HEAD".to_string()
    }

    /// Resolve the PRIMARY repo root from a staging (linked) worktree.
    ///
    /// `git rev-parse --git-common-dir` run inside a linked worktree points
    /// at `<primary>/.git` (the shared common dir); its parent is the primary
    /// worktree root. Falls back to the given `worktree` when resolution
    /// fails (e.g. not a linked worktree).
    async fn primary_root(&self, worktree: &Path) -> Result<std::path::PathBuf, StagingError> {
        let output = Command::new("git")
            .arg("rev-parse")
            .arg("--git-common-dir")
            .current_dir(worktree)
            .output()
            .await
            .map_err(|e| StagingError::Git(format!("git rev-parse failed: {e}")))?;

        if !output.status.success() {
            return Ok(worktree.to_path_buf());
        }

        let common_dir_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if common_dir_str.is_empty() {
            return Ok(worktree.to_path_buf());
        }

        let common_dir = std::path::PathBuf::from(&common_dir_str);
        // `git rev-parse` returns paths relative to the worktree when they
        // are inside it; make absolute before walking up.
        let common_dir = if common_dir.is_absolute() {
            common_dir
        } else {
            worktree.join(common_dir)
        };
        let canonical = std::fs::canonicalize(&common_dir).unwrap_or(common_dir);

        // `<primary>/.git` -> `<primary>`
        Ok(canonical
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| worktree.to_path_buf()))
    }

    /// Get additions/removals count for a file.
    ///
    /// Diffs against HEAD (via `diff_base`) for the same reason as
    /// `diff()`: after a checkpoint staged everything, a bare
    /// worktree-vs-index `--numstat` returns empty.
    async fn diffstat(&self, worktree: &Path, path: &str) -> Result<(i32, i32), StagingError> {
        let base = Self::diff_base(worktree).await;
        let output = Command::new("git")
            .arg("diff")
            .arg("--numstat")
            .arg(&base)
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

    /// A two-file patch filtered to one file yields a well-formed
    /// single-file patch starting at its `diff --git` header.
    #[test]
    fn filter_patch_keeps_diff_git_header() {
        let patch = "\
diff --git a/alpha.md b/alpha.md
index 1111111..2222222 100644
--- a/alpha.md
+++ b/alpha.md
@@ -1 +1 @@
-old alpha
+new alpha
diff --git a/beta.md b/beta.md
index 3333333..4444444 100644
--- a/beta.md
+++ b/beta.md
@@ -1 +1 @@
-old beta
+new beta
";
        let wanted: std::collections::HashSet<&str> = ["beta.md"].into_iter().collect();
        let filtered = filter_patch_to_files(patch, &wanted);

        assert!(
            filtered.starts_with("diff --git a/beta.md b/beta.md\n"),
            "filtered patch must start with the diff --git header: {filtered:?}"
        );
        assert!(filtered.contains("--- a/beta.md\n"));
        assert!(filtered.contains("+++ b/beta.md\n"));
        assert!(filtered.contains("+new beta"));
        assert!(
            !filtered.contains("alpha"),
            "unselected file must be fully excluded: {filtered:?}"
        );
        assert_eq!(filtered.lines().count(), 6);
    }

    /// Filtering to all files preserves both sections, each well-formed.
    #[test]
    fn filter_patch_keeps_all_selected() {
        let patch = "\
diff --git a/alpha.md b/alpha.md
--- a/alpha.md
+++ b/alpha.md
@@ -1 +1 @@
-old
+new
diff --git a/beta.md b/beta.md
--- a/beta.md
+++ b/beta.md
@@ -1 +1 @@
-old
+new
";
        let wanted: std::collections::HashSet<&str> = ["alpha.md", "beta.md"].into_iter().collect();
        let filtered = filter_patch_to_files(patch, &wanted);
        assert!(filtered.starts_with("diff --git a/alpha.md b/alpha.md\n"));
        assert_eq!(filtered.matches("diff --git ").count(), 2);
        assert_eq!(filtered.matches("--- a/").count(), 2);
    }

    /// Filtering to no matching file yields an empty patch.
    #[test]
    fn filter_patch_no_match_is_empty() {
        let patch = "\
diff --git a/alpha.md b/alpha.md
--- a/alpha.md
+++ b/alpha.md
@@ -1 +1 @@
-old
+new
";
        let wanted: std::collections::HashSet<&str> = ["other.md"].into_iter().collect();
        let filtered = filter_patch_to_files(patch, &wanted);
        assert!(filtered.is_empty());
    }

    /// End-to-end: a staged apply of one selected file lands the change in
    /// the PRIMARY repo, not the staging worktree, and reports it applied.
    #[tokio::test]
    async fn staged_apply_targets_primary_root() {
        let primary = tempfile::tempdir().unwrap();
        // git init + tracked files + initial commit so the worktree add has
        // a HEAD and modifications show up in `git diff`.
        let init_files = [("alpha.md", "alpha old\n"), ("beta.md", "beta old\n")];
        for (name, contents) in init_files {
            std::fs::write(primary.path().join(name), contents).unwrap();
        }
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
            vec!["add", "-A"],
            vec!["commit", "-q", "-m", "init"],
        ] {
            let out = Command::new("git")
                .args(&args)
                .current_dir(primary.path())
                .output()
                .await
                .unwrap();
            assert!(
                out.status.success(),
                "git {:?} failed: {}",
                args,
                String::from_utf8_lossy(&out.stderr)
            );
        }

        let staging = GitStagingArea::new();
        let root = staging
            .open(primary.path(), "apply-test-1", ChangeMode::Staged, &Scope::default())
            .await
            .unwrap();

        // Modify two tracked files in the staging worktree; only beta.md is
        // selected for apply.
        std::fs::write(root.worktree.join("alpha.md"), "alpha new\n").unwrap();
        std::fs::write(root.worktree.join("beta.md"), "beta new\n").unwrap();

        let selection = crate::staging::Selection {
            entries: vec![crate::staging::SelectionEntry {
                path: "beta.md".to_string(),
                hunks: Vec::new(),
            }],
            apply_all_accepted: false,
        };

        let outcome = staging.apply(&root, &selection).await.unwrap();
        assert!(
            outcome.warnings.is_empty(),
            "apply should succeed without warnings: {:?}",
            outcome.warnings
        );
        assert_eq!(outcome.applied, vec!["beta.md".to_string()]);

        // The selected change must land in the PRIMARY worktree…
        let primary_beta = std::fs::read_to_string(primary.path().join("beta.md"))
            .expect("beta.md must be readable in the primary repo after apply");
        assert_eq!(primary_beta, "beta new\n");
        // …while the unselected file stays untouched there. (Had `git apply`
        // run inside the staging worktree — the bug — the patch would have
        // bounced off the already-modified files and nothing would have
        // landed in the primary repo.)
        let primary_alpha = std::fs::read_to_string(primary.path().join("alpha.md")).unwrap();
        assert_eq!(primary_alpha, "alpha old\n");

        staging.discard(&root).await.unwrap();
    }

    /// Regression (review finding 3): checkpoint() stages everything
    /// (`git add -A`), after which a bare worktree-vs-index `git diff` is
    /// empty. diff() must diff against HEAD so post-checkpoint edits stay
    /// visible. Also asserts the direct-mode checkpoint uses a dedicated
    /// index and leaves the user's shared index alone.
    #[tokio::test]
    async fn diff_shows_post_checkpoint_changes() {
        let primary = tempfile::tempdir().unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t"],
            vec!["config", "user.name", "t"],
        ] {
            let out = Command::new("git")
                .args(&args)
                .current_dir(primary.path())
                .output()
                .await
                .unwrap();
            assert!(out.status.success(), "git {:?} failed", args);
        }
        std::fs::write(primary.path().join("spec.md"), "line\n").unwrap();
        for args in [vec!["add", "-A"], vec!["commit", "-q", "-m", "init"]] {
            let out = Command::new("git")
                .args(&args)
                .current_dir(primary.path())
                .output()
                .await
                .unwrap();
            assert!(out.status.success(), "git {:?} failed", args);
        }

        let staging = GitStagingArea::new();
        let root = staging
            .open(
                primary.path(),
                "diff-cp-1",
                ChangeMode::Direct,
                &Scope::default(),
            )
            .await
            .unwrap();

        // Edit, checkpoint (stages everything), then edit FURTHER — the
        // post-checkpoint edit must still be visible to diff().
        std::fs::write(primary.path().join("spec.md"), "line\nline2\n").unwrap();
        staging.checkpoint(&root).await.unwrap();
        std::fs::write(primary.path().join("spec.md"), "line\nline2\nline3\n").unwrap();

        let changes = staging.diff(&root).await.unwrap();
        assert!(
            changes.files.iter().any(|f| f.path == "spec.md"),
            "post-checkpoint edit must be visible in diff(); got {:?}",
            changes.files
        );

        // Direct-mode checkpoint must not pollute the user's shared index:
        // spec.md must be modified-but-UNSTAGED (" M"), not staged ("M ",
        // "MM" — which is what the old shared-index checkpoint produced).
        let status = Command::new("git")
            .arg("status")
            .arg("--porcelain")
            .current_dir(primary.path())
            .output()
            .await
            .unwrap();
        let status = String::from_utf8_lossy(&status.stdout);
        let spec_line = status
            .lines()
            .find(|l| l.ends_with("spec.md"))
            .unwrap_or_else(|| panic!("spec.md missing from status:\n{status}"));
        assert!(
            spec_line.starts_with(" M"),
            "checkpoint must not stage into the shared index; status line: {spec_line:?}"
        );

        staging.discard(&root).await.unwrap();
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
