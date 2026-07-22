//! Version control system: session-scoped filesystem checkpointing via git.
//!
//! On session start, a fresh git repo ("shadow repo") is initialized in
//! `~/.joey/checkpoints/<session_id>`. The working directory is added as a
//! git worktree (via `git --git-dir ... --work-tree <cwd>`), and periodic
//! snapshots capture the filesystem state. Users can revert to any checkpoint
//! with `/revert <n>`.
//!
//! This is ephemeral — the shadow repo is created fresh every session and
//! cleaned up when the session ends (or left behind for debugging).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

/// The checkpoint manager for a single session.
pub struct CheckpointManager {
    /// The git-dir of the shadow repo.
    shadow_repo: PathBuf,
    /// The working directory being tracked.
    work_tree: PathBuf,
    /// Sequential checkpoint counter.
    next_checkpoint: usize,
    /// True if git is available and the shadow repo was initialized.
    enabled: bool,
}

/// One checkpoint entry.
#[derive(Debug, Clone)]
pub struct Checkpoint {
    pub number: usize,
    pub commit_hash: String,
    pub message: String,
    pub timestamp: String,
    pub files_changed: usize,
}

impl CheckpointManager {
    /// Create a new checkpoint manager for the given session and working
    /// directory. Initializes a fresh shadow git repo. Fails gracefully (sets
    /// `enabled = false`) if git is not available.
    pub fn new(session_id: &str, work_tree: &Path) -> Self {
        let shadow_root = joey_core::joey_home().join("checkpoints").join(session_id);
        let mut mgr = CheckpointManager {
            shadow_repo: shadow_root,
            work_tree: work_tree.to_path_buf(),
            next_checkpoint: 1,
            enabled: false,
        };

        // Check git availability.
        if which::which("git").is_err() {
            tracing::debug!("git not found — checkpoints disabled");
            return mgr;
        }

        match mgr.init_shadow_repo() {
            Ok(()) => {
                mgr.enabled = true;
                tracing::info!(
                    "Checkpoint system initialized: shadow repo at {} tracking {}",
                    mgr.shadow_repo.display(),
                    mgr.work_tree.display()
                );
            }
            Err(e) => {
                tracing::warn!("Failed to initialize checkpoint shadow repo: {}", e);
            }
        }

        mgr
    }

    /// Whether the checkpoint system is active.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Initialize a bare shadow git repo and configure it to track the
    /// working directory.
    fn init_shadow_repo(&mut self) -> Result<()> {
        // Remove any stale shadow repo from a previous session with the same ID.
        if self.shadow_repo.exists() {
            std::fs::remove_dir_all(&self.shadow_repo).with_context(|| {
                format!("removing stale shadow repo {}", self.shadow_repo.display())
            })?;
        }
        std::fs::create_dir_all(&self.shadow_repo)?;

        // Init a bare repo — GIT_WORK_TREE must be unset during `git init --bare`
        // (git refuses to init with a work tree specified).
        {
            let mut cmd = Command::new("git");
            cmd.env("GIT_DIR", &self.shadow_repo);
            cmd.env_remove("GIT_WORK_TREE");
            cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
            cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
            cmd.args(["init", "--bare", "--quiet"]);
            let output = cmd.output().with_context(|| "running git init --bare")?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                anyhow::bail!("git init --bare failed: {}", stderr.trim());
            }
        }

        // Configure the default branch name.
        self.run_git(&["symbolic-ref", "HEAD", "refs/heads/main"])?;

        // Add everything as the initial commit.
        self.create_checkpoint_internal("Session start (initial state)", true)?;

        Ok(())
    }

    /// Create a checkpoint (commit all current file state into the shadow repo).
    /// Returns the checkpoint number, or None if disabled/failed.
    pub fn checkpoint(&mut self, message: &str) -> Option<usize> {
        if !self.enabled {
            return None;
        }
        match self.create_checkpoint_internal(message, false) {
            Ok(num) => {
                tracing::debug!("Checkpoint #{} created: {}", num, message);
                Some(num)
            }
            Err(e) => {
                tracing::warn!("Checkpoint creation failed: {}", e);
                None
            }
        }
    }

    fn create_checkpoint_internal(&mut self, message: &str, is_initial: bool) -> Result<usize> {
        // Stage all files (add + remove for deleted files).
        self.run_git(&["add", "--all", "--", "."])?;

        // Check if there's anything to commit. On the initial run, we always
        // commit. On subsequent runs, skip if nothing changed.
        if !is_initial {
            let output = self.run_git_capture(&["status", "--porcelain"])?;
            if output.trim().is_empty() {
                // Nothing changed — no new checkpoint needed.
                return Ok(self.next_checkpoint.saturating_sub(1));
            }
        }

        // Commit.
        let num = self.next_checkpoint;
        let full_message = format!("[{}] {}", num, message);
        self.run_git(&["commit", "--quiet", "--allow-empty", "-m", &full_message])?;

        self.next_checkpoint += 1;
        Ok(num)
    }

    /// List all checkpoints (newest first).
    pub fn list(&self) -> Result<Vec<Checkpoint>> {
        if !self.enabled {
            return Ok(Vec::new());
        }
        let log = self.run_git_capture(&["log", "--pretty=format:%H|%s|%ai", "--name-only"])?;

        let mut checkpoints = Vec::new();
        for entry in log.split("\n\n") {
            let mut lines = entry.lines();
            let header = lines.next().unwrap_or("");
            let parts: Vec<&str> = header.splitn(3, '|').collect();
            if parts.len() < 3 {
                continue;
            }
            let commit_hash = parts[0].to_string();
            let subject = parts[1].to_string();
            let timestamp = parts[2].to_string();

            // Extract checkpoint number from "[N] message" format.
            let number = subject
                .strip_prefix('[')
                .and_then(|s| s.split(']').next())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);

            let files_changed = lines.filter(|l| !l.is_empty()).count();

            checkpoints.push(Checkpoint {
                number,
                commit_hash,
                message: subject
                    .strip_prefix('[')
                    .and_then(|s| s.find(']').map(|i| s[i + 1..].trim().to_string()))
                    .unwrap_or_else(|| subject.clone()),
                timestamp,
                files_changed,
            });
        }
        Ok(checkpoints)
    }

    /// Revert the working directory to the state at checkpoint `number`.
    /// This does a `git checkout` of the commit, then copies files back.
    pub fn revert(&self, number: usize) -> Result<()> {
        if !self.enabled {
            anyhow::bail!("Checkpoint system is not enabled");
        }
        let checkpoints = self.list()?;
        let target = checkpoints
            .iter()
            .find(|c| c.number == number)
            .with_context(|| format!("Checkpoint #{} not found", number))?;

        // Checkout the commit's tree into the working directory.
        // We use `git checkout <hash> -- .` to restore files.
        let hash = &target.commit_hash;
        self.run_git(&["checkout", hash, "--", "."])?;

        // Also remove files that existed after this checkpoint but not before.
        // We do this by checking which files were added in commits after `number`.
        // Simpler approach: diff between current HEAD and target, then remove
        // files that are new.
        let files_to_remove = self.run_git_capture(&[
            "diff",
            "--name-only",
            "--diff-filter=A",
            &format!("{}..HEAD", hash),
        ])?;
        for file in files_to_remove.lines() {
            if file.is_empty() {
                continue;
            }
            let target_path = self.work_tree.join(file);
            if target_path.exists() {
                let _ = std::fs::remove_file(&target_path);
            }
        }

        Ok(())
    }

    /// Run a git command against the shadow repo + work tree.
    fn run_git(&self, args: &[&str]) -> Result<()> {
        self.run_git_capture(args).map(|_| ()).with_context(|| {
            format!(
                "git {} (shadow repo: {})",
                args.join(" "),
                self.shadow_repo.display()
            )
        })
    }

    fn run_git_capture(&self, args: &[&str]) -> Result<String> {
        let mut cmd = Command::new("git");
        cmd.env("GIT_DIR", &self.shadow_repo);
        cmd.env("GIT_WORK_TREE", &self.work_tree);
        // Don't sign commits (GPG).
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
        cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
        cmd.args(args);
        cmd.current_dir(&self.work_tree);

        let output = cmd
            .output()
            .with_context(|| format!("running git {}", args.join(" ")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            anyhow::bail!(
                "git {} failed (exit {:?}): {} {}",
                args.join(" "),
                output.status.code(),
                stderr.trim(),
                stdout.trim()
            );
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Clean up the shadow repo (called on session end).
    pub fn cleanup(&self) {
        if self.shadow_repo.exists() {
            let _ = std::fs::remove_dir_all(&self.shadow_repo);
            tracing::debug!("Cleaned up shadow repo: {}", self.shadow_repo.display());
        }
    }

    /// The shadow repo path.
    pub fn repo_path(&self) -> &Path {
        &self.shadow_repo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tests must run serially because they use HomeOverrideGuard which is
    /// process-global.
    fn git_available() -> bool {
        which::which("git").is_ok()
    }

    #[test]
    fn checkpoint_lifecycle() {
        let _lock = crate::test_env_lock();

        if !git_available() {
            eprintln!("git not available — skipping checkpoint test");
            return;
        }

        let home_dir = tempfile::tempdir().unwrap();
        let _home_guard =
            joey_core::constants::HomeOverrideGuard::new(home_dir.path().to_path_buf());

        let dir = tempfile::tempdir().unwrap();
        let work_tree = dir.path();

        // Create some initial files.
        std::fs::write(work_tree.join("a.txt"), "initial").unwrap();

        let mut mgr = CheckpointManager::new("test-session-1", work_tree);
        assert!(mgr.is_enabled(), "checkpoint system should be enabled");

        // Modify and checkpoint.
        std::fs::write(work_tree.join("b.txt"), "second file").unwrap();
        let cp2 = mgr.checkpoint("Added b.txt");
        assert_eq!(cp2, Some(2));

        // List checkpoints.
        let list = mgr.list().unwrap();
        assert!(list.len() >= 2, "should have at least 2 checkpoints");

        // Modify again.
        std::fs::write(work_tree.join("c.txt"), "third file").unwrap();
        std::fs::write(work_tree.join("a.txt"), "modified").unwrap();
        let cp3 = mgr.checkpoint("Added c.txt, modified a.txt");
        assert_eq!(cp3, Some(3));

        // Revert to checkpoint 2 — c.txt should be gone, a.txt restored.
        mgr.revert(2).unwrap();
        assert!(
            !work_tree.join("c.txt").exists(),
            "c.txt should be removed after revert"
        );
        assert_eq!(
            std::fs::read_to_string(work_tree.join("a.txt")).unwrap(),
            "initial",
            "a.txt should be reverted to initial state"
        );
        assert!(work_tree.join("b.txt").exists(), "b.txt should still exist");

        // Cleanup.
        mgr.cleanup();
        assert!(
            !mgr.repo_path().exists(),
            "shadow repo should be cleaned up"
        );
    }

    #[test]
    fn checkpoint_noop_on_no_changes() {
        let _lock = crate::test_env_lock();

        if !git_available() {
            return;
        }

        let home_dir = tempfile::tempdir().unwrap();
        let _home_guard =
            joey_core::constants::HomeOverrideGuard::new(home_dir.path().to_path_buf());

        let dir = tempfile::tempdir().unwrap();
        let work_tree = dir.path();
        std::fs::write(work_tree.join("x.txt"), "content").unwrap();

        let mut mgr = CheckpointManager::new("test-noop", work_tree);
        assert!(mgr.is_enabled());

        // No changes since init → checkpoint returns the same number.
        let cp = mgr.checkpoint("nothing changed");
        // Should return the last checkpoint number (1 = initial), not a new one.
        assert_eq!(cp, Some(1));

        mgr.cleanup();
    }
}
