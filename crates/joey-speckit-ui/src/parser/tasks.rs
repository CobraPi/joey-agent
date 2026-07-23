//! Parser for `tasks.md` into a `Vec<Task>`.
//!
//! Each task line looks like:
//! `- [ ] T005 [P] Define core model types (Feature, Specification, ...) in ...`
//! or with `[X]`/`[x]` for done. Lines that don't match the checkbox pattern
//! are ignored (not part of the task list); checkbox lines that are
//! malformed still produce a `Task` entry with `Status::Unparsed` rather
//! than being dropped, per data-model.md Edge Cases.

use crate::model::{Task, TaskStatus};

pub fn parse_tasks(content: &str) -> Vec<Task> {
    let mut tasks = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(task) = parse_task_line(trimmed) {
            tasks.push(task);
        }
    }
    tasks
}

fn parse_task_line(line: &str) -> Option<Task> {
    // Expect a leading "- [ ]" / "- [X]" / "- [x]" checkbox.
    let rest = line.strip_prefix("- [")?;
    let close = rest.find(']')?;
    let marker = rest[..close].trim();
    let after_checkbox = rest[close + 1..].trim_start();

    let status = match marker {
        "" | " " => TaskStatus::Todo,
        "X" | "x" => TaskStatus::Done,
        "~" | "-" => TaskStatus::InProgress,
        _ => TaskStatus::Unparsed,
    };

    // after_checkbox now looks like "T005 [P] Define core model types ..."
    let mut tokens = after_checkbox.split_whitespace();
    let id = tokens.next().unwrap_or("UNKNOWN").to_string();
    let mut parallel_eligible = false;
    let remainder_start = if after_checkbox.trim_start().starts_with(&id) {
        let mut idx = after_checkbox.find(&id).map(|i| i + id.len()).unwrap_or(0);
        // Peek for a "[P]" or "[US1]" tag right after the id.
        let after_id = after_checkbox[idx..].trim_start();
        if after_id.starts_with("[P]") {
            parallel_eligible = true;
        }
        // Recompute idx to point past any bracket tags for description start.
        idx = after_checkbox.len() - after_id.len();
        idx
    } else {
        0
    };

    let description = after_checkbox[remainder_start..].trim().to_string();

    let user_story_ref = extract_user_story_ref(&description);
    let target_files = extract_target_files(&description);

    Some(Task {
        id,
        parallel_eligible,
        description,
        target_files,
        status,
        user_story_ref,
    })
}

/// Look for a bracketed user-story tag like `[US1]` anywhere in the
/// description.
fn extract_user_story_ref(description: &str) -> Option<String> {
    let bytes = description.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'[' {
            if let Some(end) = description[i..].find(']') {
                let inner = &description[i + 1..i + end];
                if inner.starts_with("US") && inner[2..].chars().all(|c| c.is_ascii_digit()) {
                    return Some(inner.to_string());
                }
            }
        }
        i += 1;
    }
    None
}

/// Extract inline-code file paths (`` `crates/foo/src/bar.rs` ``) as target
/// files.
fn extract_target_files(description: &str) -> Vec<String> {
    let mut files = Vec::new();
    let mut chars = description.char_indices().peekable();
    let mut start: Option<usize> = None;
    for (idx, ch) in description.char_indices() {
        if ch == '`' {
            if let Some(s) = start.take() {
                let candidate = &description[s..idx];
                if candidate.contains('/') || candidate.ends_with(".rs") || candidate.ends_with(".md")
                {
                    files.push(candidate.to_string());
                }
            } else {
                start = Some(idx + 1);
            }
        }
    }
    let _ = chars.next(); // keep peekable used to avoid unused warning in edge builds
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_task() {
        let line = "- [ ] T005 [P] Define core model types (Feature, Specification, UserStory, Requirement, Plan, ConstitutionGate, Task, ClarificationEntry, AnalysisFinding) in `crates/joey-speckit-ui/src/model.rs`";
        let tasks = parse_tasks(line);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "T005");
        assert!(tasks[0].parallel_eligible);
        assert_eq!(tasks[0].status, TaskStatus::Todo);
        assert!(tasks[0].target_files.iter().any(|f| f.contains("model.rs")));
    }

    #[test]
    fn parses_done_task() {
        let line = "- [X] T001 Create `crates/joey-speckit-ui` crate skeleton";
        let tasks = parse_tasks(line);
        assert_eq!(tasks[0].status, TaskStatus::Done);
    }

    #[test]
    fn malformed_checkbox_is_unparsed_not_dropped() {
        let line = "- [?] T999 Something weird";
        let tasks = parse_tasks(line);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, TaskStatus::Unparsed);
    }

    #[test]
    fn non_task_lines_ignored() {
        let content = "# Tasks\nSome prose.\n- [ ] T001 Do a thing\n";
        let tasks = parse_tasks(content);
        assert_eq!(tasks.len(), 1);
    }

    #[test]
    fn extracts_user_story_ref() {
        let line = "- [ ] T016 [P] [US1] Contract test for PATCH";
        let tasks = parse_tasks(line);
        assert_eq!(tasks[0].user_story_ref.as_deref(), Some("US1"));
    }
}
