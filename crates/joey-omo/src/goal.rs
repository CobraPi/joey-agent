//! GoalState: per-session persistent objective via `.omo/goals.json`.
//!
//! Port of data-model.md `GoalState` and contracts/slash-commands.md.

use std::path::Path;

use serde::{Deserialize, Serialize};

// ── GoalStatus ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GoalStatus {
    Active,
    Paused,
}

impl Default for GoalStatus {
    fn default() -> Self {
        Self::Active
    }
}

// ── GoalState ───────────────────────────────────────────────────────

/// Per-session persistent objective.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalState {
    pub session_id: String,
    pub objective: String,
    #[serde(default)]
    pub status: GoalStatus,
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
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
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
            set_at: chrono::Utc::now().to_rfc3339(),
        }
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
}
