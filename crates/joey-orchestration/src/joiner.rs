//! Three-way joiner for the orchestration runtime (spec 023 T019, US4).
//!
//! Collects each task's changes from its isolated workspace as a
//! [`ChangeBundle`] (FR-016), detects undeclared writes as divergences
//! (FR-017), and integrates bundles into the shared project root via
//! `git apply --3way` with pre-apply conflict surfacing (FR-017/FR-018,
//! SC-006). Integration never creates commits — no user-visible history
//! entries (FR-018).
//!
//! Git is driven through `std::process::Command` (no libgit2).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use joey_core::utils::atomic_replace;

use crate::evidence::{EvidenceKind, RunHandle};
use crate::task_graph::TaskNode;
use crate::workspace::IsolatedWorkspace;

/// `atomic_replace` returns `anyhow::Result`; adapt to `std::io::Result`.
fn atomic_write(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    atomic_replace(path, contents)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e.to_string()))
}

/// The unit of integration for one task (FR-016).
///
/// Captures what a task *declared* it would write versus what it
/// *actually* wrote, the patch artifact, and the ids of the evidence
/// records backing the collection (the records themselves live in the
/// run store, referenced by id).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChangeBundle {
    /// The task this bundle belongs to.
    pub task_id: String,
    /// The git revision the isolated workspace was based on.
    pub baseline_sha: String,
    /// Repository-relative paths the task declared it would write.
    pub declared_write_set: Vec<PathBuf>,
    /// Repository-relative paths the task actually wrote.
    pub actual_write_set: Vec<PathBuf>,
    /// Path of the patch artifact (`patches/<task-id>.patch`).
    pub patch_path: PathBuf,
    /// Ids of the evidence records backing this bundle.
    pub evidence_ids: Vec<String>,
}

/// The result of integrating a batch of bundles (FR-017/FR-018).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct IntegrationReport {
    /// Tasks whose patches were applied, in application order.
    pub applied_task_ids: Vec<String>,
    /// Divergence notes (populated by collection, not integration).
    pub divergences: Vec<DivergenceNote>,
}

/// One task's undeclared-write divergence (FR-017).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DivergenceNote {
    /// The task that wrote outside its declared write set.
    pub task_id: String,
    /// The repository-relative paths it wrote without declaring.
    pub undeclared_paths: Vec<String>,
}

/// Integration failed: the named task's patch conflicts (FR-017).
///
/// Surfaced *before* the conflicting patch is applied (SC-006 per-patch
/// atomicity): bundles earlier in the batch stay applied; nothing from
/// the conflicting patch reaches the tree.
#[derive(Debug, Clone, PartialEq, thiserror::Error, serde::Serialize, serde::Deserialize)]
#[error("integration conflict for task {task_id}: {reason}")]
pub struct IntegrationConflict {
    /// The task whose patch could not be applied.
    pub task_id: String,
    /// The patch artifact that conflicted.
    pub patch_path: PathBuf,
    /// Git's explanation (trimmed stderr/stdout or exit-code note).
    pub reason: String,
}

/// Collects change bundles from isolated workspaces and integrates them
/// into the shared project root via three-way apply (FR-016/FR-017/FR-018).
pub struct Joiner {
    project_root: PathBuf,
}

impl Joiner {
    /// Create a joiner operating on the shared checkout at `project_root`.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }

    /// Collect one task's changes from its isolated workspace (FR-016).
    ///
    /// Produces `patches/<task-id>.patch` by first recording untracked
    /// files as intent-to-add (`git add -N .`, best-effort) so new files
    /// show up in the diff, then running `git diff <baseline>` inside the
    /// workspace (git failure ⇒ an empty patch file, never an error),
    /// derives the actual write set from
    /// `git diff --name-only <baseline>`, and checks it against the task's
    /// declared write set. A workspace whose `baseline_revision` is empty
    /// (a non-git FullCopy) is rejected with `InvalidData`: its changes
    /// cannot be collected as a patch, and an empty bundle would make
    /// integrate silently report the task applied while discarding them.
    /// The same rejection applies when the workspace is not a git work
    /// tree even though `baseline_revision` is non-empty — a FullCopy
    /// workspace excludes `.git` (see `workspace::FULL_COPY_EXCLUDE_DIRS`)
    /// yet may carry the PARENT's recorded baseline; there every git
    /// invocation below would fail into a swallowed empty patch.
    /// FR-017 divergence check: any actual path not in
    /// the declared set is recorded as a `DivergenceReport` evidence record
    /// (referenced by id in the bundle); a `CommandOutput` evidence record
    /// with the patch size is always recorded.
    pub fn collect(
        &self,
        ws: &IsolatedWorkspace,
        task: &TaskNode,
        run: &mut RunHandle,
    ) -> std::io::Result<ChangeBundle> {
        let patch_path = run.patch_path(task.id.as_str());

        // A workspace without a git baseline (non-git FullCopy) cannot be
        // diffed: `git diff ""` fails, yielding an empty patch that
        // integrate would treat as a no-op while silently discarding all
        // of the task's changes. Surface instead of swallowing (FR-016).
        if ws.baseline_revision.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "cannot collect changes from a workspace without a git baseline",
            ));
        }

        // A non-empty recorded baseline is not sufficient: a FullCopy
        // workspace excludes `.git`, so `git add -N`/`git diff` inside it
        // would all fail and be swallowed into an EMPTY patch + EMPTY
        // actual write set — the worker's changes would be silently
        // dropped while integrate reports the task "applied". Hard-check
        // that ws.path() is actually inside a git work tree (a linked
        // GitWorktree passes; a FullCopy copy does not) before diffing.
        // `git rev-parse --is-inside-work-tree` prints `true` and exits 0
        // inside a work tree; anything else (non-zero exit outside a
        // repository, or `false`) fails the check.
        let inside = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(ws.path())
            .output();
        let is_work_tree = matches!(inside, Ok(out) if out.status.success()
            && String::from_utf8_lossy(&out.stdout).trim() == "true");
        if !is_work_tree {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "cannot collect changes from {}: not a git work tree \
                     (FullCopy workspaces exclude .git; worker output \
                     cannot be captured as a patch)",
                    ws.path().display()
                ),
            ));
        }

        // Record untracked files as intent-to-add so files newly created
        // by the child appear in both `git diff <baseline>` and
        // `git diff --name-only <baseline>` (otherwise they are silently
        // dropped and the FR-017 divergence check is blind to them).
        // Best-effort: failure (e.g. an already-tracked clean tree, or
        // git missing) is ignored, exactly like the diff failures below.
        let _ = Command::new("git")
            .args(["add", "-N", "."])
            .current_dir(ws.path())
            .output();

        // Produce the patch artifact. On git failure or an empty repo the
        // patch is empty — collection must never fail because of git.
        let diff_out = Command::new("git")
            .arg("diff")
            .arg(&ws.baseline_revision)
            .current_dir(ws.path())
            .output();
        let patch_bytes = match diff_out {
            Ok(out) if out.status.success() => out.stdout,
            _ => Vec::new(),
        };
        atomic_write(&patch_path, &patch_bytes)?;
        let patch_len = patch_bytes.len();

        // Actual write set: paths changed vs the baseline revision,
        // relative (git emits forward slashes). Empty on git failure.
        let names_out = Command::new("git")
            .args(["diff", "--name-only"])
            .arg(&ws.baseline_revision)
            .current_dir(ws.path())
            .output();
        let actual_write_set: Vec<PathBuf> = match names_out {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(PathBuf::from)
                .collect(),
            _ => Vec::new(),
        };

        // FR-017 divergence check: string comparison on forward-slash paths.
        let declared: Vec<String> = task
            .write_set
            .iter()
            .map(|p| p.display().to_string().replace('\\', "/"))
            .collect();
        let undeclared: Vec<String> = actual_write_set
            .iter()
            .map(|p| p.display().to_string())
            .filter(|p| !declared.iter().any(|d| d == p))
            .collect();

        let mut evidence_ids = Vec::new();
        if !undeclared.is_empty() {
            let record = run.record_evidence(
                task.id.as_str(),
                EvidenceKind::DivergenceReport,
                serde_json::json!({
                    "undeclared_paths": undeclared,
                    "baseline": ws.baseline_revision,
                }),
            )?;
            evidence_ids.push(record.id);
        }
        let command_record = run.record_evidence(
            task.id.as_str(),
            EvidenceKind::CommandOutput,
            serde_json::json!({
                "command": "git diff",
                "bytes": patch_len,
            }),
        )?;
        evidence_ids.push(command_record.id);

        Ok(ChangeBundle {
            task_id: task.id.to_string(),
            baseline_sha: ws.baseline_revision.clone(),
            declared_write_set: task.write_set.clone(),
            actual_write_set,
            patch_path,
            evidence_ids,
        })
    }

    /// Integrate bundles into the project root, in order (FR-017/FR-018).
    ///
    /// Per patch (SC-006 atomicity): first `git apply --check --3way`
    /// — a non-zero exit *or* a reported 3-way conflict surfaces as
    /// [`IntegrationConflict`] immediately, leaving the tree untouched by
    /// this patch (bundles before it stay applied). A clean check is
    /// followed by the real `git apply --3way`; a non-zero exit surfaces
    /// the same way. On success the task id joins the report and the
    /// `on_integrated` index-refresh hook runs (its failure aborts as a
    /// conflict with `index refresh failed: …`). An empty patch file is a
    /// no-op that still reports as applied. No `git commit` is ever run —
    /// integration leaves no user-visible history entries (FR-018).
    pub fn integrate(
        &self,
        bundles: &[ChangeBundle],
        mut on_integrated: impl FnMut(&ChangeBundle) -> std::io::Result<()>,
    ) -> Result<IntegrationReport, IntegrationConflict> {
        let mut report = IntegrationReport::default();

        for bundle in bundles {
            let patch = bundle.patch_path.display().to_string();
            // An empty patch is a trivial no-op: `git apply` would reject
            // it ("No valid patches in input"), so short-circuit.
            let is_empty_patch = fs::metadata(&bundle.patch_path)
                .map(|m| m.len() == 0)
                .unwrap_or(false);

            if !is_empty_patch {
                // Pre-apply check. Note: with --3way, git exits 0 even
                // when the merge would conflict, reporting the conflict on
                // stdout ("Applied patch to '…' with conflicts."); a real
                // conflicted apply would exit 1 but leave conflict markers
                // in the tree. Surfacing the conflict here keeps the tree
                // untouched by the failing patch (SC-006).
                let check = Command::new("git")
                    .args(["apply", "--check", "--3way", &patch])
                    .current_dir(&self.project_root)
                    .output()
                    .map_err(|e| self.conflict(bundle, format!("failed to run git: {e}")))?;
                let check_text = combined_output(&check);
                if !check.status.success() || check_text.contains("with conflicts") {
                    return Err(self.conflict(bundle, reason_from(&check, "git apply --check")));
                }

                let apply = Command::new("git")
                    .args(["apply", "--3way", &patch])
                    .current_dir(&self.project_root)
                    .output()
                    .map_err(|e| self.conflict(bundle, format!("failed to run git: {e}")))?;
                if !apply.status.success() {
                    return Err(self.conflict(bundle, reason_from(&apply, "git apply --3way")));
                }
            }

            report.applied_task_ids.push(bundle.task_id.clone());
            if let Err(e) = on_integrated(bundle) {
                return Err(self.conflict(
                    bundle,
                    format!("index refresh failed: {e}"),
                ));
            }
        }

        Ok(report)
    }

    fn conflict(&self, bundle: &ChangeBundle, reason: String) -> IntegrationConflict {
        IntegrationConflict {
            task_id: bundle.task_id.clone(),
            patch_path: bundle.patch_path.clone(),
            reason,
        }
    }
}

/// Combined (stdout + stderr) captured text of a git invocation.
fn combined_output(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Git's explanation for a failed invocation: trimmed stderr, else
/// trimmed stdout, else an exit-code note.
fn reason_from(out: &Output, what: &str) -> String {
    let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if !stdout.is_empty() {
        return stdout;
    }
    format!("{what} exited {}", out.status.code().unwrap_or(-1))
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::evidence::EvidenceRecord;
    use crate::evaluator::VerificationPlanView;
    use crate::task_graph::{
        AcceptanceCriterion, IsolationMode, ModelTier, RiskLevel, TaskId, TaskStatus, WorkerRole,
    };
    use crate::workspace::WorktreeMode;

    /// True when a usable `git` is on PATH (tests skip gracefully if not).
    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// Run git in `cwd`; Ok only when it exits zero.
    fn git_ok(cwd: &Path, args: &[&str]) -> Option<Output> {
        let out = Command::new("git").args(args).current_dir(cwd).output().ok()?;
        if out.status.success() {
            Some(out)
        } else {
            None
        }
    }

    /// Scratch repo with base files committed; returns the HEAD sha.
    fn init_scratch(root: &Path) -> String {
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "fn a() {}\n").unwrap();
        fs::write(root.join("src/b.rs"), "fn b() {}\n").unwrap();
        fs::write(root.join("src/shared.txt"), "line1\nline2\nline3\n").unwrap();
        git_ok(root, &["init", "-q"]).expect("git init");
        git_ok(root, &["config", "user.email", "test@example.com"]).expect("git config email");
        git_ok(root, &["config", "user.name", "Test"]).expect("git config name");
        git_ok(root, &["add", "."]).expect("git add");
        git_ok(root, &["commit", "-qm", "base"]).expect("git commit");
        String::from_utf8(git_ok(root, &["rev-parse", "HEAD"]).unwrap().stdout)
            .unwrap()
            .trim()
            .to_string()
    }

    /// Recursive copy of a directory tree (including `.git`), so the copy
    /// is itself a working repository at the same revision.
    fn copy_dir(src: &Path, dst: &Path) {
        fs::create_dir_all(dst).unwrap();
        for entry in fs::read_dir(src).unwrap().flatten() {
            let from = entry.path();
            let to = dst.join(entry.file_name());
            if from.is_dir() {
                copy_dir(&from, &to);
            } else {
                fs::copy(&from, &to).unwrap();
            }
        }
    }

    fn count_commits(root: &Path) -> usize {
        String::from_utf8(
            git_ok(root, &["log", "--oneline"]).expect("git log").stdout,
        )
        .unwrap()
        .lines()
        .count()
    }

    fn task(id: &str, writes: &[&str]) -> TaskNode {
        TaskNode {
            id: TaskId::new(id).unwrap(),
            objective: format!("objective for {id}"),
            dependencies: vec![],
            read_set: vec![],
            write_set: writes.iter().map(PathBuf::from).collect(),
            artifact_ids: vec![],
            role: WorkerRole::Implementor,
            model_tier: ModelTier::Economical,
            risk: RiskLevel::Low,
            acceptance: vec![AcceptanceCriterion {
                criterion: "it works".to_string(),
                kind: "manual".to_string(),
            }],
            verification: VerificationPlanView::default(),
            isolation: IsolationMode::SharedCheckout,
            status: TaskStatus::Pending,
            attempts: 0,
        }
    }

    /// Minimal isolated workspace built directly as a struct literal
    /// (workspace.rs fields are pub).
    fn ws(task_id: &str, path: &Path, rev: &str) -> IsolatedWorkspace {
        IsolatedWorkspace {
            task_id: task_id.to_string(),
            path: path.to_path_buf(),
            baseline_revision: rev.to_string(),
            mode: WorktreeMode::FullCopy,
        }
    }

    #[test]
    fn collect_produces_declared_bundle() {
        if !git_available() {
            eprintln!("skipping: git unavailable");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let rev = init_scratch(&repo);

        let copy = tmp.path().join("copy-a");
        copy_dir(&repo, &copy);
        fs::write(copy.join("src/a.rs"), "fn a1() {}\n").unwrap();

        let run_root = tmp.path().join("run");
        let mut run = RunHandle::create_at(&run_root, "run-1", &rev).unwrap();
        let node = task("task-a", &["src/a.rs"]);

        let joiner = Joiner::new(&repo);
        let bundle = joiner.collect(&ws("task-a", &copy, &rev), &node, &mut run).unwrap();

        assert_eq!(bundle.task_id, "task-a");
        assert_eq!(bundle.baseline_sha, rev);
        assert_eq!(bundle.actual_write_set, vec![PathBuf::from("src/a.rs")]);
        assert!(bundle.declared_write_set.contains(&PathBuf::from("src/a.rs")));
        assert!(bundle.patch_path.is_file());
        assert!(fs::metadata(&bundle.patch_path).unwrap().len() > 0);
        assert!(!bundle.evidence_ids.is_empty());
    }

    #[test]
    fn collect_includes_untracked_new_files() {
        if !git_available() {
            eprintln!("skipping: git unavailable");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let rev = init_scratch(&repo);

        // Child creates a brand-NEW untracked file (nothing else changes).
        let copy = tmp.path().join("copy-a");
        copy_dir(&repo, &copy);
        fs::write(copy.join("src/new_file.rs"), "fn brand_new() {}\n").unwrap();

        let run_root = tmp.path().join("run");
        let mut run = RunHandle::create_at(&run_root, "run-1", &rev).unwrap();
        let node = task("task-a", &["src/new_file.rs"]);

        let joiner = Joiner::new(&repo);
        let bundle = joiner
            .collect(&ws("task-a", &copy, &rev), &node, &mut run)
            .unwrap();

        // The untracked file must appear in the actual write set…
        assert_eq!(
            bundle.actual_write_set,
            vec![PathBuf::from("src/new_file.rs")],
            "untracked new files must be collected, not silently dropped"
        );
        // …and in the patch artifact (intent-to-add makes it diffable)…
        let patch = fs::read_to_string(&bundle.patch_path).unwrap();
        assert!(
            patch.contains("src/new_file.rs"),
            "patch must contain the new file: {patch}"
        );
        assert!(patch.contains("fn brand_new()"), "patch carries content");
        // …so no divergence is flagged (it was declared).
        let evidence_path = run_root.join("evidence").join("task-a.json");
        let raw = fs::read_to_string(&evidence_path).unwrap();
        assert!(
            !raw.contains("\"divergence_report\""),
            "declared new file is not a divergence: {raw}"
        );

        // End-to-end: the bundle integrates and the file lands in the root.
        let report = joiner.integrate(&[bundle], |_| Ok(())).unwrap();
        assert_eq!(report.applied_task_ids, vec!["task-a"]);
        assert_eq!(
            fs::read_to_string(repo.join("src/new_file.rs")).unwrap(),
            "fn brand_new() {}\n"
        );
    }

    #[test]
    fn collect_rejects_workspace_without_git_baseline() {
        if !git_available() {
            eprintln!("skipping: git unavailable");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        init_scratch(&repo);

        // Non-git FullCopy workspace: a plain directory copy with edits,
        // whose baseline_revision is empty.
        let copy = tmp.path().join("copy-nongit");
        fs::create_dir_all(copy.join("src")).unwrap();
        fs::write(copy.join("src/a.rs"), "fn a1() {}\n").unwrap();

        let run_root = tmp.path().join("run");
        let mut run = RunHandle::create_at(&run_root, "run-1", "").unwrap();
        let node = task("task-a", &["src/a.rs"]);

        let joiner = Joiner::new(&repo);
        let err = joiner
            .collect(&ws("task-a", &copy, ""), &node, &mut run)
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            err.to_string()
                .contains("without a git baseline"),
            "error message: {err}"
        );
    }

    /// Regression (finding #7): a FullCopy workspace excludes `.git` but a
    /// non-empty baseline_revision may still be recorded from the parent
    /// repo. `git add -N`/`git diff` would all fail and be swallowed into
    /// an empty patch — silently dropping the worker's changes. collect()
    /// must return an explicit Err, never an empty bundle.
    #[test]
    fn collect_rejects_gitless_workspace_with_recorded_baseline() {
        if !git_available() {
            eprintln!("skipping: git unavailable");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let rev = init_scratch(&repo);

        // FullCopy-shaped workspace: a copy WITHOUT `.git` (exactly what
        // workspace::full_copy produces), worker edits inside it, and a
        // NON-EMPTY baseline recorded from the parent.
        let copy = tmp.path().join("copy-fullcopy");
        copy_dir(&repo, &copy);
        fs::remove_dir_all(copy.join(".git")).unwrap();
        fs::write(copy.join("src/a.rs"), "fn a1() {}\n").unwrap();

        let run_root = tmp.path().join("run");
        let mut run = RunHandle::create_at(&run_root, "run-1", &rev).unwrap();
        let node = task("task-a", &["src/a.rs"]);

        let joiner = Joiner::new(&repo);
        let err = joiner
            .collect(&ws("task-a", &copy, &rev), &node, &mut run)
            .expect_err("git-less copy with a recorded baseline must be rejected");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::InvalidData,
            "error kind: {err}"
        );
        assert!(
            err.to_string().contains("not a git work tree"),
            "error message: {err}"
        );
    }

    #[test]
    fn collect_flags_undeclared_writes() {
        if !git_available() {
            eprintln!("skipping: git unavailable");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let rev = init_scratch(&repo);

        // Declare src/a.rs but actually write src/b.rs (undeclared).
        let copy = tmp.path().join("copy-a");
        copy_dir(&repo, &copy);
        fs::write(copy.join("src/b.rs"), "fn b1() {}\n").unwrap();

        let run_root = tmp.path().join("run");
        let mut run = RunHandle::create_at(&run_root, "run-1", &rev).unwrap();
        let node = task("task-a", &["src/a.rs"]);

        let joiner = Joiner::new(&repo);
        let bundle = joiner.collect(&ws("task-a", &copy, &rev), &node, &mut run).unwrap();

        assert_eq!(bundle.actual_write_set, vec![PathBuf::from("src/b.rs")]);

        // The on-disk evidence file (snake_case wire form) carries a
        // divergence report naming the undeclared path.
        let evidence_path = run_root.join("evidence").join("task-a.json");
        let raw = fs::read_to_string(&evidence_path).unwrap();
        assert!(raw.contains("\"divergence_report\""), "raw: {raw}");
        let records: Vec<EvidenceRecord> = serde_json::from_str(&raw).unwrap();
        let divergence = records
            .iter()
            .find(|r| r.kind == EvidenceKind::DivergenceReport)
            .expect("DivergenceReport record present");
        assert_eq!(
            divergence.payload["undeclared_paths"],
            serde_json::json!(["src/b.rs"])
        );
        assert_eq!(divergence.payload["baseline"], serde_json::json!(rev));
        // The divergence record id is referenced by the bundle.
        assert!(bundle.evidence_ids.contains(&divergence.id));
    }

    #[test]
    fn integrate_applies_disjoint_patches_in_order() {
        if !git_available() {
            eprintln!("skipping: git unavailable");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let rev = init_scratch(&repo);

        let copy1 = tmp.path().join("copy-1");
        let copy2 = tmp.path().join("copy-2");
        copy_dir(&repo, &copy1);
        copy_dir(&repo, &copy2);
        fs::write(copy1.join("src/a.rs"), "fn a1() {}\n").unwrap();
        fs::write(copy2.join("src/b.rs"), "fn b1() {}\n").unwrap();

        let run_root = tmp.path().join("run");
        let mut run = RunHandle::create_at(&run_root, "run-1", &rev).unwrap();
        let joiner = Joiner::new(&repo);
        let node_a = task("task-a", &["src/a.rs"]);
        let node_b = task("task-b", &["src/b.rs"]);
        let b1 = joiner.collect(&ws("task-a", &copy1, &rev), &node_a, &mut run).unwrap();
        let b2 = joiner.collect(&ws("task-b", &copy2, &rev), &node_b, &mut run).unwrap();

        let commits_before = count_commits(&repo);

        let mut callback_order: Vec<String> = Vec::new();
        let report = joiner
            .integrate(&[b1, b2], |b| {
                callback_order.push(b.task_id.clone());
                Ok(())
            })
            .unwrap();

        assert_eq!(report.applied_task_ids, vec!["task-a", "task-b"]);
        assert_eq!(callback_order, vec!["task-a", "task-b"]);
        assert_eq!(
            fs::read_to_string(repo.join("src/a.rs")).unwrap(),
            "fn a1() {}\n"
        );
        assert_eq!(
            fs::read_to_string(repo.join("src/b.rs")).unwrap(),
            "fn b1() {}\n"
        );
        // FR-018: integration creates no commits.
        assert_eq!(count_commits(&repo), commits_before);
    }

    #[test]
    fn integrate_surfaces_conflict_untouched() {
        if !git_available() {
            eprintln!("skipping: git unavailable");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let rev = init_scratch(&repo);

        // Both tasks edit line 1 of the shared file, differently.
        let copy1 = tmp.path().join("copy-1");
        let copy2 = tmp.path().join("copy-2");
        copy_dir(&repo, &copy1);
        copy_dir(&repo, &copy2);
        fs::write(copy1.join("src/shared.txt"), "one-A\nline2\nline3\n").unwrap();
        fs::write(copy2.join("src/shared.txt"), "one-B\nline2\nline3\n").unwrap();

        let run_root = tmp.path().join("run");
        let mut run = RunHandle::create_at(&run_root, "run-1", &rev).unwrap();
        let joiner = Joiner::new(&repo);
        let node_a = task("task-a", &["src/shared.txt"]);
        let node_b = task("task-b", &["src/shared.txt"]);
        let b1 = joiner.collect(&ws("task-a", &copy1, &rev), &node_a, &mut run).unwrap();
        let b2 = joiner.collect(&ws("task-b", &copy2, &rev), &node_b, &mut run).unwrap();

        let commits_before = count_commits(&repo);

        // b1 applies cleanly, THEN b2 conflicts.
        let err = joiner.integrate(&[b1, b2.clone()], |_| Ok(())).unwrap_err();
        assert_eq!(err.task_id, "task-b");
        assert_eq!(err.patch_path, b2.patch_path);
        assert!(!err.reason.is_empty());

        // The conflicting change is NOT in the tree: only b1's version.
        assert_eq!(
            fs::read_to_string(repo.join("src/shared.txt")).unwrap(),
            "one-A\nline2\nline3\n"
        );
        // FR-018: no commits, before or after the conflict.
        assert_eq!(count_commits(&repo), commits_before);
    }

    #[test]
    fn empty_patch_bundle_is_noop() {
        if !git_available() {
            eprintln!("skipping: git unavailable");
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let rev = init_scratch(&repo);

        let run_root = tmp.path().join("run");
        let run = RunHandle::create_at(&run_root, "run-1", &rev).unwrap();
        let patch = run.patch_path("task-empty");
        fs::write(&patch, b"").unwrap();

        let bundle = ChangeBundle {
            task_id: "task-empty".to_string(),
            baseline_sha: rev.clone(),
            declared_write_set: vec![],
            actual_write_set: vec![],
            patch_path: patch,
            evidence_ids: vec![],
        };

        let joiner = Joiner::new(&repo);
        let commits_before = count_commits(&repo);
        let mut calls = 0;
        let report = joiner
            .integrate(&[bundle], |_| {
                calls += 1;
                Ok(())
            })
            .unwrap();

        assert_eq!(report.applied_task_ids, vec!["task-empty"]);
        assert_eq!(calls, 1);
        assert_eq!(fs::read_to_string(repo.join("src/a.rs")).unwrap(), "fn a() {}\n");
        assert_eq!(count_commits(&repo), commits_before);
    }
}
