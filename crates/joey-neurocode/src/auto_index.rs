//! Automatic re-indexing of the structural graph after large edits
//! (feature 015 follow-up: dynamic context across turns).
//!
//! Problem: NeuroCode assembles its dependency-aware context from the
//! per-project `graph.db`. That index is built once (`/neurocode index`)
//! and then goes stale as the agent edits files — the context graph
//! reflected the codebase as it was at index time, not as it is now. The
//! staleness note in the assembled context warns about it, but the graph
//! never refreshed on its own.
//!
//! This module tracks edits observed through [`AutoIndexState`] and decides
//! when they cross the "large edits" threshold ([`AutoIndexConfig`]):
//! enough distinct files, or enough cumulative edited lines. The agent
//! turn loop asks [`AutoIndexState::should_reindex`] at turn end and, when
//! it returns true, rebuilds the index (debounced by a minimum interval so
//! a burst of small patches doesn't re-index every turn).
//!
//! Re-indexing is *additive* to correctness, never blocking: failures are
//! reported and ignored, and the next turn's assembly simply continues
//! against the previous index (with its staleness note intact).

use std::collections::BTreeSet;
use std::time::Instant;

use crate::config::AutoIndexConfig;

/// Mutable edit-tracking state backing the re-index decision.
#[derive(Debug)]
pub struct AutoIndexState {
    config: AutoIndexConfig,
    /// Distinct source-file paths edited since the last index build.
    edited_files: BTreeSet<String>,
    /// Cumulative added+removed lines across those edits.
    edited_lines: usize,
    /// When the last re-index (manual or automatic) happened. `None`
    /// means "never indexed in this engine's lifetime" — the first
    /// automatic pass may run immediately (subject to thresholds).
    last_index_at: Option<Instant>,
    /// Monotonic generation counter — bumped on every completed re-index.
    /// Callers compare this to detect "the graph changed under me".
    generation: u64,
}

impl AutoIndexState {
    pub fn new(config: &AutoIndexConfig) -> Self {
        Self {
            config: config.clone(),
            edited_files: BTreeSet::new(),
            edited_lines: 0,
            last_index_at: None,
            generation: 0,
        }
    }

    /// Record one edited file with its added/removed line counts. Cheap
    /// (set insert + adds). Called from the agent's FileChange handling.
    pub fn record_edit(&mut self, path: &str, added: usize, removed: usize) {
        self.edited_files.insert(path.to_string());
        self.edited_lines += added.saturating_add(removed);
    }

    /// True when the observed edits cross the configured "large edits"
    /// threshold AND the debounce interval has elapsed since the last
    /// index build. Pure decision — no I/O.
    pub fn should_reindex(&self) -> bool {
        if !self.config.enabled {
            return false;
        }
        let threshold_hit = self.edited_files.len() >= self.config.file_threshold
            || self.edited_lines >= self.config.line_threshold;
        if !threshold_hit {
            return false;
        }
        match self.last_index_at {
            None => true,
            Some(at) => at.elapsed().as_secs_f64() >= self.config.min_interval_secs,
        }
    }

    /// How far the tracker has progressed toward the thresholds, for UI
    /// display ("2/3 files toward auto-reindex").
    pub fn progress(&self) -> AutoIndexProgress {
        AutoIndexProgress {
            files: self.edited_files.len(),
            file_threshold: self.config.file_threshold,
            lines: self.edited_lines,
            line_threshold: self.config.line_threshold,
        }
    }

    /// Mark a re-index as complete: clears the observed-edit trackers and
    /// starts a fresh debounce window. Bumps the generation so callers can
    /// detect that the graph changed.
    pub fn note_reindexed(&mut self) -> u64 {
        self.edited_files.clear();
        self.edited_lines = 0;
        self.last_index_at = Some(Instant::now());
        self.generation += 1;
        self.generation
    }

    /// Current generation (bumped on every completed re-index).
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The configured thresholds (for status display).
    pub fn config(&self) -> &AutoIndexConfig {
        &self.config
    }
}

/// Snapshot of tracker progress toward the re-index thresholds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AutoIndexProgress {
    pub files: usize,
    pub file_threshold: usize,
    pub lines: usize,
    pub line_threshold: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> AutoIndexState {
        AutoIndexState::new(&AutoIndexConfig::default())
    }

    #[test]
    fn below_thresholds_no_reindex() {
        let mut s = state();
        s.record_edit("src/a.rs", 10, 5);
        s.record_edit("src/b.rs", 20, 5);
        assert!(!s.should_reindex(), "2 files / 40 lines is small");
    }

    #[test]
    fn file_threshold_triggers() {
        let mut s = state();
        s.record_edit("src/a.rs", 1, 0);
        s.record_edit("src/b.rs", 1, 0);
        assert!(!s.should_reindex());
        s.record_edit("src/c.rs", 1, 0);
        assert!(s.should_reindex(), "3 distinct files crossed the threshold");
    }

    #[test]
    fn line_threshold_triggers_with_one_file() {
        let mut s = state();
        s.record_edit("src/big_rewrite.rs", 150, 60);
        assert!(
            s.should_reindex(),
            "210 edited lines in one file counts as large"
        );
    }

    #[test]
    fn repeated_edits_to_same_file_count_once_for_files() {
        let mut s = state();
        for _ in 0..10 {
            s.record_edit("src/same.rs", 5, 5);
        }
        // 100 lines < 200 threshold, and only 1 distinct file < 3.
        assert!(!s.should_reindex(), "one file hammered is not 3 files");
        assert_eq!(s.progress().files, 1);
        assert_eq!(s.progress().lines, 100);
    }

    #[test]
    fn reindex_resets_tracking_and_bumps_generation() {
        let mut s = state();
        s.record_edit("a", 500, 0); // cross line threshold
        assert!(s.should_reindex());
        let g1 = s.note_reindexed();
        assert_eq!(g1, 1);
        assert!(!s.should_reindex(), "trackers cleared after re-index");
        assert_eq!(s.progress().files, 0);
        assert_eq!(s.progress().lines, 0);
        s.record_edit("b", 500, 0);
        assert!(
            !s.should_reindex(),
            "debounce blocks an immediate second pass"
        );
        assert_eq!(s.generation(), 1);
    }

    #[test]
    fn disabled_never_triggers() {
        let cfg = AutoIndexConfig {
            enabled: false,
            ..AutoIndexConfig::default()
        };
        let mut s = AutoIndexState::new(&cfg);
        s.record_edit("a", 10_000, 10_000);
        assert!(!s.should_reindex());
    }

    #[test]
    fn progress_reports_thresholds() {
        let s = state();
        let p = s.progress();
        assert_eq!(p.file_threshold, 3);
        assert_eq!(p.line_threshold, 200);
    }
}
