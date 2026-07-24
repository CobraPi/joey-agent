//! BoulderState: tracks active plan-execution work via `.omo/boulder.json`.
//!
//! Port of data-model.md `BoulderState`. File-based JSON persistence (VR-004).

use std::path::Path;

use serde::{Deserialize, Serialize};

// ── BoulderWorkStatus ───────────────────────────────────────────────

/// Status of a boulder work entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoulderWorkStatus {
    Active,
    Completed,
    Abandoned,
}

impl Default for BoulderWorkStatus {
    fn default() -> Self {
        Self::Active
    }
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
    pub fn write(&self, omo_dir: &Path) -> std::io::Result<()> {
        let path = omo_dir.join("boulder.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(path, json)
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
