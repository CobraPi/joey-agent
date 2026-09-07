//! Git-worktree workspace isolation (spec 023, T018 / US4).
//!
//! FR-015: tasks whose write-sets must not collide run against isolated
//! copies of the project — a linked git worktree when the project is a git
//! repository, or a plain recursive copy otherwise. Readers share the main
//! checkout; they never reach [`WorkspaceIsolation::prepare`] in the CLI
//! wiring.
//!
//! Worktrees live at `<run_dir>/worktree/<task-id>` (spec 023, Phase 6).
//! System git is driven via `std::process::Command` (no libgit2, per
//! plan.md); the argument style mirrors the staging precedent in
//! `joey-speckit-ui/src/staging_impl.rs`.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::task_graph::TaskNode;

/// How an [`IsolatedWorkspace`] was materialized. Named `WorktreeMode`
/// (not `IsolationMode`) to avoid clashing with
/// [`crate::task_graph::IsolationMode`], which describes the *plan's*
/// intent rather than the concrete mechanism used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum WorktreeMode {
    /// A linked git worktree created via `git worktree add --detach`.
    GitWorktree,
    /// A recursive filesystem copy (used when git is unavailable or the
    /// project is not a git repository).
    FullCopy,
}

/// A prepared isolated workspace for one task (FR-015).
#[derive(Debug, Clone)]
pub struct IsolatedWorkspace {
    /// The task this workspace belongs to.
    pub task_id: String,
    /// Filesystem path of the isolated checkout/copy.
    pub path: PathBuf,
    /// Git revision the workspace was based on (empty when unknown).
    pub baseline_revision: String,
    /// Mechanism used to create the workspace.
    pub mode: WorktreeMode,
}

impl IsolatedWorkspace {
    /// The isolated workspace's filesystem path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Marker file written at the root of a full-copy workspace. Its presence
/// records that the workspace is a copy (not a linked worktree); its
/// contents are empty (a full copy excludes `.git`, so there is no usable
/// baseline — `Joiner::collect` rejects such workspaces rather than
/// emitting a silently-empty patch).
const FULL_COPY_MARKER: &str = ".joey-worktree-copy";

/// Directory names excluded from the full-copy fallback.
const FULL_COPY_EXCLUDE_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    ".venv",
    "dist",
    "build",
];

/// Returns `git rev-parse HEAD` in `project_root`, trimmed; `None` on any
/// failure (not a git repo, git missing, non-zero exit).
pub fn baseline_revision(project_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(project_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let rev = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if rev.is_empty() {
        None
    } else {
        Some(rev)
    }
}

/// Creates isolated copies of a project for parallel-writing tasks
/// (FR-015). One instance pairs a project root with the run directory
/// that holds its worktrees.
pub struct WorkspaceIsolation {
    project_root: PathBuf,
    run_dir: PathBuf,
}

impl WorkspaceIsolation {
    /// New isolation context. Worktrees live at
    /// `<run_dir>/worktree/<task-id>` (spec 023, Phase 6).
    pub fn new(project_root: impl Into<PathBuf>, run_dir: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            run_dir: run_dir.into(),
        }
    }

    /// The path a task's isolated workspace would occupy (pure function).
    pub fn worktree_path(&self, task_id: &str) -> PathBuf {
        self.run_dir.join("worktree").join(task_id)
    }

    /// Prepares an isolated workspace for `task` (FR-015).
    ///
    /// Idempotent: if `<run_dir>/worktree/<task.id>` already exists and is
    /// non-empty (a resumed run), it is reused as-is — the recorded mode is
    /// recovered from the presence of the `.joey-worktree-copy` marker
    /// (`FullCopy`) or its absence (`GitWorktree`).
    ///
    /// Otherwise, a linked git worktree is attempted via
    /// `git worktree add --detach <target>`. On any failure — including git
    /// being entirely missing — the fallback is a recursive full copy of the
    /// project (excluding build/dependency/VCS directories), after which the
    /// marker file records the workspace as a FullCopy with an EMPTY
    /// baseline (a copy excludes `.git` and can never be diffed). A missing
    /// git binary never fails `prepare`; a copy failure does.
    pub fn prepare(&self, task: &TaskNode) -> io::Result<IsolatedWorkspace> {
        let target = self.worktree_path(task.id.as_str());

        // Reuse an existing workspace from a previous (resumed) attempt.
        if is_non_empty_dir(&target) {
            let marker = target.join(FULL_COPY_MARKER);
            let (mode, baseline) = if marker.is_file() {
                let baseline = std::fs::read_to_string(&marker)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                (WorktreeMode::FullCopy, baseline)
            } else {
                let baseline = baseline_revision(&target)
                    .or_else(|| baseline_revision(&self.project_root))
                    .unwrap_or_default();
                (WorktreeMode::GitWorktree, baseline)
            };
            return Ok(IsolatedWorkspace {
                task_id: task.id.to_string(),
                path: target,
                baseline_revision: baseline,
                mode,
            });
        }

        let baseline = baseline_revision(&self.project_root).unwrap_or_default();

        // Try a linked git worktree first (argument style mirrors the
        // staging precedent in joey-speckit-ui staging_impl.rs).
        let worktree_ok = Command::new("git")
            .arg("worktree")
            .arg("add")
            .arg("--detach")
            .arg(&target)
            .current_dir(&self.project_root)
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if worktree_ok {
            return Ok(IsolatedWorkspace {
                task_id: task.id.to_string(),
                path: target,
                baseline_revision: baseline,
                mode: WorktreeMode::GitWorktree,
            });
        }

        // Full-copy fallback: git missing or worktree add failed. Copy
        // failure (unlike git absence) IS an error. The baseline is
        // deliberately recorded as EMPTY: a full copy excludes `.git`, so
        // it is not a git work tree and its changes can never be diffed
        // into a patch — `Joiner::collect` rejects workspaces without a
        // git baseline (and, defensively, any non-work-tree path), which
        // is the honest outcome instead of a silently-empty patch
        // (finding #7). The parent's baseline is still available via
        // `baseline_revision(&self.project_root)` by any caller that
        // needs it for run bookkeeping.
        full_copy(&self.project_root, &target)?;
        std::fs::write(target.join(FULL_COPY_MARKER), "")?;
        Ok(IsolatedWorkspace {
            task_id: task.id.to_string(),
            path: target,
            baseline_revision: String::new(),
            mode: WorktreeMode::FullCopy,
        })
    }

    /// Removes an isolated workspace. Idempotent: a missing path is `Ok(())`.
    ///
    /// For git worktrees, `git worktree remove --force <path>` is attempted
    /// (run from the project root) followed by a best-effort
    /// `git worktree prune` whose errors are ignored; if the remove failed,
    /// the directory is deleted directly. Full copies are always deleted
    /// directly.
    pub fn cleanup(&self, ws: &IsolatedWorkspace) -> io::Result<()> {
        if !ws.path.exists() {
            return Ok(());
        }
        match ws.mode {
            WorktreeMode::GitWorktree => {
                let removed = Command::new("git")
                    .arg("worktree")
                    .arg("remove")
                    .arg("--force")
                    .arg(&ws.path)
                    .current_dir(&self.project_root)
                    .output()
                    .map(|out| out.status.success())
                    .unwrap_or(false);
                // Best-effort prune; ignore all errors.
                let _ = Command::new("git")
                    .arg("worktree")
                    .arg("prune")
                    .current_dir(&self.project_root)
                    .output();
                if removed {
                    Ok(())
                } else {
                    std::fs::remove_dir_all(&ws.path)
                }
            }
            WorktreeMode::FullCopy => std::fs::remove_dir_all(&ws.path),
        }
    }
}

/// Whether `p` is an existing directory containing at least one entry.
fn is_non_empty_dir(p: &Path) -> bool {
    match std::fs::read_dir(p) {
        Ok(mut entries) => entries.next().is_some(),
        Err(_) => false,
    }
}

/// Recursively copies `src` to `dst`, excluding directories named in
/// [`FULL_COPY_EXCLUDE_DIRS`]. Creates directories as needed and copies
/// regular files with `std::fs::copy`; symlinks/specials are skipped.
fn full_copy(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        if file_type.is_dir() {
            if FULL_COPY_EXCLUDE_DIRS.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            full_copy(&entry.path(), &dst.join(&name))?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), dst.join(&name))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::VerificationPlanView;
    use crate::task_graph::{
        AcceptanceCriterion, IsolationMode, ModelTier, RiskLevel, TaskId, TaskStatus, TaskNode,
        WorkerRole,
    };

    /// Whether the system git binary is invocable (`git --version`).
    fn which_git() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// A tempdir initialized as a git repo with one commit containing
    /// `src/a.txt`. Returns `None` (skip) if any git step fails.
    fn scratch_repo() -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().ok()?;
        let git = |args: &[&str]| -> bool {
            Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };
        if !git(&["init"]) {
            return None;
        }
        if !git(&["config", "user.email", "test@example.com"]) {
            return None;
        }
        if !git(&["config", "user.name", "Joey Test"]) {
            return None;
        }
        let src = dir.path().join("src");
        std::fs::create_dir_all(&src).ok()?;
        std::fs::write(src.join("a.txt"), "hello\n").ok()?;
        if !git(&["add", "-A"]) {
            return None;
        }
        if !git(&["commit", "-m", "init"]) {
            return None;
        }
        Some(dir)
    }

    /// A minimal valid `TaskNode` with the given id and write-set.
    fn minimal_task(id: &str, write_set: &[&str]) -> TaskNode {
        TaskNode {
            id: TaskId::new(id).unwrap(),
            objective: "test objective".to_string(),
            dependencies: vec![],
            read_set: vec![],
            write_set: write_set.iter().map(PathBuf::from).collect(),
            artifact_ids: vec![],
            role: WorkerRole::Implementor,
            model_tier: ModelTier::Economical,
            risk: RiskLevel::Low,
            acceptance: vec![AcceptanceCriterion {
                criterion: "it works".to_string(),
                kind: "manual".to_string(),
            }],
            verification: VerificationPlanView::default(),
            isolation: IsolationMode::IsolatedWorktree,
            status: TaskStatus::Pending,
            attempts: 0,
        }
    }

    #[test]
    fn prepare_creates_isolated_copy() {
        if !which_git() {
            eprintln!("skip: git not available");
            return;
        }
        let Some(repo) = scratch_repo() else {
            eprintln!("skip: scratch repo unavailable");
            return;
        };
        let run_dir = tempfile::tempdir().unwrap();
        let iso = WorkspaceIsolation::new(repo.path(), run_dir.path());
        let node = minimal_task("task-a", &["src/a.rs"]);

        let ws = iso.prepare(&node).expect("prepare should succeed");
        assert_eq!(ws.task_id, "task-a");
        assert!(ws.path().exists(), "workspace path should exist");
        assert_eq!(ws.path(), iso.worktree_path("task-a"));
        assert!(
            ws.path().join("src").join("a.txt").is_file(),
            "committed file should be present in the worktree"
        );
        assert_eq!(ws.mode, WorktreeMode::GitWorktree);
        assert_eq!(ws.baseline_revision, baseline_revision(repo.path()).unwrap());
    }

    #[test]
    fn prepare_is_idempotent() {
        if !which_git() {
            eprintln!("skip: git not available");
            return;
        }
        let Some(repo) = scratch_repo() else {
            eprintln!("skip: scratch repo unavailable");
            return;
        };
        let run_dir = tempfile::tempdir().unwrap();
        let iso = WorkspaceIsolation::new(repo.path(), run_dir.path());
        let node = minimal_task("task-a", &["src/a.rs"]);

        let ws1 = iso.prepare(&node).expect("first prepare");
        let ws2 = iso.prepare(&node).expect("second prepare (reuse)");
        assert_eq!(ws1.path(), ws2.path());
        assert_eq!(ws1.mode, ws2.mode);
        assert_eq!(ws1.baseline_revision, ws2.baseline_revision);
    }

    #[test]
    fn prepare_falls_back_to_full_copy() {
        // A plain (non-git) directory forces the full-copy fallback.
        let plain = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(plain.path().join("src")).unwrap();
        std::fs::write(plain.path().join("src/a.txt"), "x").unwrap();
        std::fs::write(plain.path().join("top.txt"), "y").unwrap();
        std::fs::create_dir_all(plain.path().join("node_modules/pkg")).unwrap();
        std::fs::write(plain.path().join("node_modules/pkg/index.js"), "z").unwrap();

        let run_dir = tempfile::tempdir().unwrap();
        let iso = WorkspaceIsolation::new(plain.path(), run_dir.path());
        let node = minimal_task("task-b", &["src/a.rs"]);

        let ws = iso.prepare(&node).expect("prepare should fall back, not fail");
        assert_eq!(ws.mode, WorktreeMode::FullCopy);
        assert!(ws.path().join("src/a.txt").is_file());
        assert!(ws.path().join("top.txt").is_file());
        assert!(ws.path().join(FULL_COPY_MARKER).is_file());
        assert!(
            !ws.path().join("node_modules").exists(),
            "excluded directories must not be copied"
        );
    }

    /// Regression (finding #7, spawn side): when the project root IS a git
    /// repo but `git worktree add` fails, the FullCopy fallback must NOT
    /// record the parent's baseline — a full copy excludes `.git`, so the
    /// recorded baseline could never be diffed and `Joiner::collect` would
    /// silently emit an empty patch. The baseline must be empty so the
    /// existing empty-baseline guard rejects such workspaces explicitly.
    #[test]
    fn full_copy_fallback_records_empty_baseline() {
        if !which_git() {
            eprintln!("skip: git not available");
            return;
        }
        let Some(repo) = scratch_repo() else {
            eprintln!("skip: scratch repo unavailable");
            return;
        };
        // Force the worktree failure deterministically: `.git/worktrees`
        // as a regular file makes `git worktree add` refuse to proceed.
        std::fs::write(repo.path().join(".git").join("worktrees"), b"x").unwrap();
        let parent_baseline =
            baseline_revision(repo.path()).expect("parent repo still has HEAD");

        let run_dir = tempfile::tempdir().unwrap();
        let iso = WorkspaceIsolation::new(repo.path(), run_dir.path());
        let node = minimal_task("task-fc", &["src/a.rs"]);

        let ws = iso.prepare(&node).expect("prepare should fall back, not fail");
        assert_eq!(ws.mode, WorktreeMode::FullCopy, "poisoned worktree add must fall back");
        assert!(
            !ws.path().join(".git").exists(),
            "full copy must not carry .git"
        );
        assert!(
            ws.baseline_revision.is_empty(),
            "FullCopy must record an EMPTY baseline, not the parent's {parent_baseline}"
        );
        assert!(ws.path().join(FULL_COPY_MARKER).is_file());
    }

    #[test]
    fn cleanup_removes_worktree() {
        if !which_git() {
            eprintln!("skip: git not available");
            return;
        }
        let Some(repo) = scratch_repo() else {
            eprintln!("skip: scratch repo unavailable");
            return;
        };
        let run_dir = tempfile::tempdir().unwrap();
        let iso = WorkspaceIsolation::new(repo.path(), run_dir.path());
        let node = minimal_task("task-a", &["src/a.rs"]);

        let ws = iso.prepare(&node).expect("prepare");
        let path = ws.path().to_path_buf();
        assert!(path.exists());

        iso.cleanup(&ws).expect("cleanup should succeed");
        assert!(!path.exists(), "workspace should be gone after cleanup");

        // Idempotent: cleaning an already-removed workspace is Ok(()).
        iso.cleanup(&ws).expect("second cleanup should be Ok");
    }

    #[test]
    fn baseline_revision_none_outside_git() {
        let plain = tempfile::tempdir().unwrap();
        assert_eq!(baseline_revision(plain.path()), None);

        if !which_git() {
            eprintln!("skip: git not available");
            return;
        }
        let Some(repo) = scratch_repo() else {
            eprintln!("skip: scratch repo unavailable");
            return;
        };
        assert!(baseline_revision(repo.path()).is_some());
    }
}
