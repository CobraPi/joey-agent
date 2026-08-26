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
    pub fn write(&self, omo_dir: &Path) -> std::io::Result<()> {
        let path = omo_dir.join("goals.json");
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(std::io::Error::other)?;
        std::fs::write(path, json)
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
}
