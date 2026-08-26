//! BoulderState: tracks active plan-execution work via `.omo/boulder.json`.
//!
//! Port of data-model.md `BoulderState`. File-based JSON persistence (VR-004).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── BoulderWorkStatus ───────────────────────────────────────────────

/// Status of a boulder work entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum BoulderWorkStatus {
    #[default]
    Active,
    Completed,
    Abandoned,
}


// ── BoulderWork ─────────────────────────────────────────────────────

/// A single active plan-execution work entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoulderWork {
    /// Unique work ID.
    pub id: String,
    /// Path to `.omo/plans/{name}.md`.
    pub plan_path: String,
    /// Plan slug (derived from filename).
    pub plan_name: String,
    /// Agent session executing this work.
    pub session_id: String,
    /// Agent name (usually "atlas").
    pub agent: String,
    /// Optional git worktree path.
    #[serde(default)]
    pub worktree_path: Option<String>,
    /// Current status.
    #[serde(default)]
    pub status: BoulderWorkStatus,
    /// ISO 8601 timestamp when the work started.
    pub started_at: String,
}

// ── BoulderState ────────────────────────────────────────────────────

/// Tracks active plan-execution work. Persisted as `.omo/boulder.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoulderState {
    /// Active work entries.
    #[serde(default)]
    pub works: Vec<BoulderWork>,
    /// Schema version.
    #[serde(default = "default_version")]
    pub version: u32,
}

fn default_version() -> u32 {
    1
}

/// Unique sibling temp path for atomic writes: same directory (same
/// filesystem, so rename is atomic), `.boulder.json.<pid>.<thread-id>.tmp`
/// so concurrent writers from different sessions don't clobber each other's
/// temp files.
fn atomic_temp_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "boulder.json".to_string());
    name.push_str(&format!(
        ".{}.{}.tmp",
        std::process::id(),
        thread_id(),
    ));
    dest.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

fn thread_id() -> std::string::String {
    format!("{:?}", std::thread::current().id())
}

impl Default for BoulderState {
    fn default() -> Self {
        Self {
            works: Vec::new(),
            version: 1,
        }
    }
}

impl BoulderState {
    /// Read the boulder state from a `.omo/` directory.
    /// Missing file returns an empty state (VR-004: not an error).
    pub fn read(omo_dir: &Path) -> Self {
        let path = omo_dir.join("boulder.json");
        match std::fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => BoulderState::default(),
        }
    }

    /// Write the boulder state to a `.omo/` directory.
    ///
    /// Atomic (VR-004 hardening): a plain `fs::write` truncates the target
    /// and streams bytes, so concurrent Atlas sessions can interleave and
    /// leave a truncated/torn `boulder.json`. Instead: write to a uniquely
    /// named temp file in the same directory, fsync it, then rename over the
    /// target — the same atomic-write pattern joey-cron uses
    /// (`jobs.rs::atomic_write_secure`). Rename within a directory is
    /// atomic on POSIX (and Windows `RENAME` semantics via
    /// `fs::rename`/remove-first), so readers never observe a partial file.
    pub fn write(&self, omo_dir: &Path) -> std::io::Result<()> {
        let path = omo_dir.join("boulder.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(std::io::Error::other)?;

        use std::io::Write;
        let tmp = atomic_temp_path(&path);
        {
            let mut file = std::fs::File::create(&tmp)?;
            // Write + fsync the temp file BEFORE renaming so the renamed
            // file's contents are durable, not just its directory entry.
            file.write_all(json.as_bytes())?;
            file.sync_all()?;
        }
        // Rename over the destination. On Windows, rename onto an existing
        // file fails, so remove first — the small window is fine here
        // because the replacement is a complete, fsynced file.
        #[cfg(windows)]
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Create a new work entry.
    pub fn create_work(
        &mut self,
        plan_path: String,
        plan_name: String,
        session_id: String,
    ) -> &BoulderWork {
        let work = BoulderWork {
            id: format!("work_{}", uuid::Uuid::new_v4().simple()),
            plan_path,
            plan_name,
            session_id,
            agent: "atlas".into(),
            worktree_path: None,
            status: BoulderWorkStatus::Active,
            started_at: chrono::Utc::now().to_rfc3339(),
        };
        self.works.push(work);
        self.works.last().unwrap()
    }

    /// Mark a work entry as completed.
    pub fn complete_work(&mut self, work_id: &str) {
        if let Some(work) = self.works.iter_mut().find(|w| w.id == work_id) {
            work.status = BoulderWorkStatus::Completed;
        }
    }

    /// Select the single active work entry (if exactly one is Active).
    pub fn select_active(&self) -> Option<&BoulderWork> {
        let active: Vec<&BoulderWork> = self
            .works
            .iter()
            .filter(|w| w.status == BoulderWorkStatus::Active)
            .collect();
        if active.len() == 1 {
            Some(active[0])
        } else {
            None
        }
    }

    /// Count of active works.
    pub fn active_count(&self) -> usize {
        self.works
            .iter()
            .filter(|w| w.status == BoulderWorkStatus::Active)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// T099: BoulderState round-trip — write then read produces identical state.
    #[test]
    fn boulder_state_round_trip() {
        let dir = tempdir().unwrap();
        let omo = dir.path();

        // Missing file → empty state (VR-004)
        let empty = BoulderState::read(omo);
        assert!(empty.works.is_empty());
        assert_eq!(empty.version, 1);

        // Write a state with one work
        let mut state = BoulderState::default();
        state.create_work(
            ".omo/plans/feature.md".into(),
            "feature".into(),
            "session_123".into(),
        );
        state.write(omo).unwrap();

        // Read back — identical
        let read_back = BoulderState::read(omo);
        assert_eq!(read_back.works.len(), 1);
        assert_eq!(read_back.works[0].plan_name, "feature");
        assert_eq!(read_back.works[0].session_id, "session_123");
        assert_eq!(read_back.works[0].status, BoulderWorkStatus::Active);
    }

    #[test]
    fn select_active_returns_single_active() {
        let mut state = BoulderState::default();
        state.create_work("a.md".into(), "a".into(), "s1".into());
        // Exactly one active → Some
        assert!(state.select_active().is_some());

        // Two active → None (ambiguous)
        state.create_work("b.md".into(), "b".into(), "s2".into());
        assert!(state.select_active().is_none());
    }

    #[test]
    fn complete_work_marks_completed() {
        let mut state = BoulderState::default();
        let work = state.create_work("a.md".into(), "a".into(), "s1".into());
        let id = work.id.clone();
        state.complete_work(&id);
        assert_eq!(state.works[0].status, BoulderWorkStatus::Completed);
    }
}

#[cfg(test)]
mod atomic_write_tests {
    use super::*;

    /// Concurrent writers must never corrupt boulder.json: the atomic
    /// temp+rename write means a reader always sees a complete file (old or
    /// new), never a truncated interleave.
    #[test]
    fn concurrent_writes_never_corrupt_state() {
        let dir = std::env::temp_dir().join(format!("joey_boulder_race_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);

        let mk = |n: u32| BoulderState {
            works: vec![BoulderWork {
                id: format!("w{n}"),
                plan_path: format!(".omo/plans/w{n}.md"),
                plan_name: format!("w{n}"),
                session_id: format!("sess-{n}"),
                agent: "atlas".into(),
                worktree_path: None,
                status: BoulderWorkStatus::Active,
                started_at: "2026-01-01T00:00:00".into(),
            }],
            version: 1,
        };
        mk(0).write(&dir).expect("seed write");

        let writers: Vec<_> = (0..4u32)
            .map(|n| {
                let d = dir.clone();
                std::thread::spawn(move || {
                    for i in 0..50u32 {
                        let mut st = mk(n);
                        st.works[0].plan_name = format!("w{n}-iter{i}");
                        st.write(&d).map_err(|e| e.to_string())?;
                    }
                    Ok::<(), String>(())
                })
            })
            .collect();
        for h in writers {
            h.join().expect("thread panicked").expect("writes ok");
        }

        // Final file must parse as a complete BoulderState.
        let final_state = BoulderState::read(&dir);
        assert_eq!(final_state.works.len(), 1, "complete state survived");
        // No temp litter.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
