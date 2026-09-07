//! GoalState: per-session persistent objective via `.omo/goals.json`.
//!
//! Port of data-model.md `GoalState` and contracts/slash-commands.md.

use std::path::Path;

use serde::{Deserialize, Serialize};

// ── GoalStatus ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum GoalStatus {
    #[default]
    Active,
    Paused,
}


// ── GoalState ───────────────────────────────────────────────────────

/// Per-session persistent objective.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalState {
    pub session_id: String,
    pub objective: String,
    #[serde(default)]
    pub status: GoalStatus,
    /// Additional success criteria managed via `/subgoal`
    /// (additive; `#[serde(default)]` keeps older files loadable).
    #[serde(default)]
    pub subgoals: Vec<Subgoal>,
    pub set_at: String,
}

// ── Atomic write helper (VR-004 hardening, mirrors boulder.rs) ──────

/// Unique sibling temp path for atomic writes: same directory (same
/// filesystem, so rename is atomic), `.goals.json.<pid>.<thread-id>.tmp`
/// so concurrent writers don't clobber each other's temp files.
///
/// Duplicated from `boulder.rs` (the same pattern there guards
/// `boulder.json`); the helper is small and private to each module.
fn atomic_temp_path(dest: &Path) -> std::path::PathBuf {
    let mut name = dest.file_name().map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "goals.json".to_string());
    name.push_str(&format!(
        ".{}.{}.tmp",
        std::process::id(),
        thread_id(),
    ));
    dest.parent().unwrap_or_else(|| Path::new(".")).join(name)
}

fn thread_id() -> String {
    format!("{:?}", std::thread::current().id())
}

impl GoalState {
    /// Read the goal state from a `.omo/` directory.
    /// Missing file returns None (no goal set).
    pub fn read(omo_dir: &Path) -> Option<Self> {
        let path = omo_dir.join("goals.json");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
    }

    /// Write the goal state to a `.omo/` directory.
    ///
    /// Atomic (mirrors `boulder.rs::BoulderState::write`): a plain
    /// `fs::write` truncates the target and streams bytes, so a crash
    /// mid-write corrupts `goals.json` (silently read as `None` on the
    /// next load). Instead: write to a uniquely named temp file in the
    /// same directory, fsync it, then rename over the target — rename
    /// within a directory is atomic on POSIX, so readers never observe
    /// a partial file.
    pub fn write(&self, omo_dir: &Path) -> std::io::Result<()> {
        let path = omo_dir.join("goals.json");
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

    /// Clear (remove) the goal state file.
    pub fn clear(omo_dir: &Path) {
        let path = omo_dir.join("goals.json");
        let _ = std::fs::remove_file(path);
    }

    /// Create a new active goal.
    pub fn new(session_id: String, objective: String) -> Self {
        Self {
            session_id,
            objective,
            status: GoalStatus::Active,
            subgoals: Vec::new(),
            set_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

// ── Subgoal ────────────────────────────────────────────────────────

/// One extra success criterion on the active goal (`/subgoal`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subgoal {
    /// 1-based display/handle number (stable while the goal lives).
    pub number: usize,
    pub text: String,
    #[serde(default)]
    pub done: bool,
    pub added_at: String,
}

impl Subgoal {
    pub fn new(number: usize, text: impl Into<String>) -> Self {
        Self {
            number,
            text: text.into(),
            done: false,
            added_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

/// Parsed action from a `/subgoal` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubgoalAction {
    /// `<text>` — add a criterion.
    Add(String),
    /// `remove N` — delete criterion N.
    Remove(usize),
    /// `done N` / `undone N` — toggle completion.
    SetDone { number: usize, done: bool },
    /// `clear` — remove all criteria.
    Clear,
    /// `` / `list` — show criteria.
    Show,
}

/// Parse a `/subgoal` argument string.
pub fn parse_subgoal_command(input: &str) -> SubgoalAction {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed == "list" || trimmed == "show" {
        return SubgoalAction::Show;
    }
    if trimmed == "clear" || trimmed == "reset" {
        return SubgoalAction::Clear;
    }
    let mut parts = trimmed.splitn(3, char::is_whitespace);
    let head = parts.next().unwrap_or("");
    match head.to_lowercase().as_str() {
        "remove" | "rm" | "delete" => {
            let n = parts.next().unwrap_or("").trim().parse().unwrap_or(0);
            SubgoalAction::Remove(n)
        }
        "done" | "check" => {
            let n = parts.next().unwrap_or("").trim().parse().unwrap_or(0);
            SubgoalAction::SetDone { number: n, done: true }
        }
        "undone" | "uncheck" => {
            let n = parts.next().unwrap_or("").trim().parse().unwrap_or(0);
            SubgoalAction::SetDone { number: n, done: false }
        }
        _ => SubgoalAction::Add(trimmed.to_string()),
    }
}

// ── GoalAction ──────────────────────────────────────────────────────

/// Parsed action from a `/goal` command (T100).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalAction {
    /// `/goal set <text>` — set active goal.
    Set { objective: String },
    /// `/goal pause` — goal becomes Paused.
    Pause,
    /// `/goal resume` — goal becomes Active.
    Resume,
    /// `/goal clear` — goal removed.
    Clear,
    /// `/goal` or `/goal show` — display current goal.
    Show,
}

/// Parse a `/goal` command string into a GoalAction (T100).
///
/// Examples:
///   "" → Show
///   "set Ship feature" → Set { objective: "Ship feature" }
///   "pause" → Pause
///   "resume" → Resume
///   "clear" → Clear
///   "show" → Show
pub fn parse_goal_command(input: &str) -> GoalAction {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return GoalAction::Show;
    }
    let (subcommand, rest) = match trimmed.split_once(char::is_whitespace) {
        Some((cmd, rest)) => (cmd, rest.trim()),
        None => (trimmed, ""),
    };
    match subcommand.to_ascii_lowercase().as_str() {
        "set" => GoalAction::Set {
            objective: rest.to_string(),
        },
        "pause" => GoalAction::Pause,
        "resume" => GoalAction::Resume,
        "clear" => GoalAction::Clear,
        "show" => GoalAction::Show,
        _ => GoalAction::Show, // Unknown subcommand → show
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T100: parse_goal_command parsing
    #[test]
    fn parse_goal_command_variants() {
        assert_eq!(parse_goal_command(""), GoalAction::Show);
        assert_eq!(parse_goal_command("show"), GoalAction::Show);
        assert_eq!(
            parse_goal_command("set Ship feature"),
            GoalAction::Set {
                objective: "Ship feature".into()
            }
        );
        assert_eq!(parse_goal_command("pause"), GoalAction::Pause);
        assert_eq!(parse_goal_command("resume"), GoalAction::Resume);
        assert_eq!(parse_goal_command("clear"), GoalAction::Clear);
    }

    #[test]
    fn goal_state_round_trip() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let omo = dir.path();

        // No goal initially
        assert!(GoalState::read(omo).is_none());

        // Set and write
        let goal = GoalState::new("session_1".into(), "Ship the feature".into());
        goal.write(omo).unwrap();

        // Read back
        let read_back = GoalState::read(omo).unwrap();
        assert_eq!(read_back.objective, "Ship the feature");
        assert_eq!(read_back.status, GoalStatus::Active);

        // Clear
        GoalState::clear(omo);
        assert!(GoalState::read(omo).is_none());
    }

    #[test]
    fn parse_subgoal_command_variants() {
        assert_eq!(parse_subgoal_command(""), SubgoalAction::Show);
        assert_eq!(parse_subgoal_command("list"), SubgoalAction::Show);
        assert_eq!(
            parse_subgoal_command("must include tests"),
            SubgoalAction::Add("must include tests".into())
        );
        assert_eq!(parse_subgoal_command("remove 2"), SubgoalAction::Remove(2));
        assert_eq!(parse_subgoal_command("rm 1"), SubgoalAction::Remove(1));
        assert_eq!(
            parse_subgoal_command("done 3"),
            SubgoalAction::SetDone { number: 3, done: true }
        );
        assert_eq!(
            parse_subgoal_command("undone 3"),
            SubgoalAction::SetDone { number: 3, done: false }
        );
        assert_eq!(parse_subgoal_command("clear"), SubgoalAction::Clear);
    }

    #[test]
    fn goal_state_subgoals_round_trip() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let omo = dir.path();

        let mut goal = GoalState::new("s".into(), "obj".into());
        goal.subgoals.push(Subgoal::new(1, "criterion one"));
        goal.subgoals.push(Subgoal::new(2, "criterion two"));
        goal.write(omo).unwrap();

        let back = GoalState::read(omo).unwrap();
        assert_eq!(back.subgoals.len(), 2);
        assert_eq!(back.subgoals[1].text, "criterion two");
        assert!(!back.subgoals[0].done);
    }

    #[test]
    fn legacy_goal_file_without_subgoals_loads() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let omo = dir.path();
        std::fs::write(
            omo.join("goals.json"),
            r#"{"session_id":"s","objective":"old","status":"active","set_at":"2024-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        let goal = GoalState::read(omo).unwrap();
        assert!(goal.subgoals.is_empty());
    }

    /// #12 regression: `write` is atomic — it writes a sibling temp file,
    /// fsyncs, and renames over the target, so a crash mid-write can never
    /// leave a truncated `goals.json` (which would silently read as `None`).
    /// Metaphor for surviving interruption: after every write the directory
    /// holds exactly one complete, parseable `goals.json` and zero temp
    /// litter; an overwrite replaces the previous content wholesale.
    #[test]
    fn goal_write_is_atomic_temp_fsync_rename() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let omo = dir.path();

        let goal = GoalState::new("s1".into(), "first objective".into());
        goal.write(omo).unwrap();

        // Temp file is gone (renamed into place), target parses completely.
        let leftovers: Vec<_> = std::fs::read_dir(omo)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left: {leftovers:?}");
        assert_eq!(GoalState::read(omo).unwrap().objective, "first objective");

        // Overwrite: the rename path must replace the previous file whole —
        // a reader between writes only ever sees the old OR the new content.
        let goal2 = GoalState::new("s2".into(), "second objective".into());
        goal2.write(omo).unwrap();
        let back = GoalState::read(omo).unwrap();
        assert_eq!(back.objective, "second objective");
        assert_eq!(back.session_id, "s2");

        // Still exactly one non-temp file: goals.json itself.
        let files: Vec<String> = std::fs::read_dir(omo)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files, vec!["goals.json".to_string()]);
    }
}
