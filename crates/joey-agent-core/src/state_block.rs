//! Feature 028 (context economy): deterministic current-state block.
//!
//! Rule-rendered (never model-recollection) TASKS / SCRATCHPAD pointer /
//! PROGRESS section shown to the model each turn at the request tail.
//! Hard char bound `state_block.max_chars`; None when nothing to show
//! (FR-004/005). Rendered into the request clone only — never persisted.

use joey_tools::tools::todo_tool::TodoItem;

/// Scratchpad summary for the state block — decoupled from the tool crate's
/// stats type so this module has no dependency on scratchpad internals
/// (populated by agent.rs from `joey_tools::tools::scratchpad_tool::stats`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScratchpadSummary {
    /// Display path of the session's scratchpad file.
    pub path: String,
    /// Number of entries.
    pub entries: usize,
    /// Timestamp of the most recent entry.
    pub last_entry_at: Option<String>,
}

/// Everything the renderer needs for one turn.
// NOTE: no PartialEq/Eq — `TodoItem` (joey-tools) doesn't implement them and
// that crate is off-limits for this task; input equality is never needed.
#[derive(Debug, Clone)]
pub struct StateBlockInput<'a> {
    pub todos: &'a [TodoItem],
    pub scratchpad: Option<ScratchpadSummary>,
    pub turn: usize,
    pub max_turns: usize,
}

/// Render the deterministic state block. `None` when there are no todos and
/// no scratchpad entries (zero cost, zero noise).
pub fn render(input: &StateBlockInput<'_>, max_chars: usize) -> Option<String> {
    let todos_empty = input.todos.is_empty();
    let scratchpad_empty = input
        .scratchpad
        .as_ref()
        .map(|s| s.entries == 0)
        .unwrap_or(true);
    if todos_empty && scratchpad_empty {
        return None;
    }
    // Sections
    let mut tasks = String::from("TASKS:\n");
    if todos_empty {
        tasks.push_str("  (none)\n");
    } else {
        tasks.push_str(&joey_tools::tools::todo_tool::render(input.todos));
    }
    let scratchpad_line = match &input.scratchpad {
        Some(s) if s.entries > 0 => format!(
            "SCRATCHPAD: {} ({} entries, last {})",
            s.path,
            s.entries,
            s.last_entry_at.clone().unwrap_or_else(|| "n/a".to_string())
        ),
        _ => "SCRATCHPAD: (no entries)".to_string(),
    };
    let progress = format!("PROGRESS: turn {} of {}", input.turn, input.max_turns);
    let mut block = format!(
        "[STATE BLOCK — deterministic, auto-maintained]\n{}\n{}\n{}",
        tasks.trim_end_matches('\n'),
        scratchpad_line,
        progress
    );
    // Hard bound with deterministic tail-first truncation keeping headers.
    truncate_to_bound(&mut block, max_chars);
    Some(block)
}

/// A line is a header iff it is one of the section headers or the banner.
fn is_header(line: &str) -> bool {
    line.starts_with("TASKS:")
        || line.starts_with("SCRATCHPAD:")
        || line.starts_with("PROGRESS:")
        || line.starts_with("[STATE BLOCK")
}

/// Deterministic tail-first truncation: repeatedly drop the last non-header
/// line until the block fits `max_chars`. If only header lines remain and it
/// is still over bound, hard-cut at a char boundary with an ellipsis. Never
/// leaves the block exceeding `max_chars`.
fn truncate_to_bound(block: &mut String, max_chars: usize) {
    while block.chars().count() > max_chars {
        let lines: Vec<&str> = block.lines().collect();
        match lines.iter().rposition(|l| !is_header(l)) {
            Some(idx) => {
                let mut rebuilt: Vec<&str> = lines;
                rebuilt.remove(idx);
                *block = rebuilt.join("\n");
            }
            None => {
                // Only headers remain: join them and append an ellipsis,
                // hard-cutting to the bound if even that overflows.
                if max_chars == 0 {
                    *block = String::new();
                    return;
                }
                let mut s: String = lines.join("\n");
                s.push('…');
                if s.chars().count() > max_chars {
                    let keep = max_chars - 1;
                    let kept: String = s.chars().take(keep).collect();
                    s = format!("{}…", kept);
                }
                *block = s;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todo(id: &str, content: &str, status: &str) -> TodoItem {
        TodoItem {
            id: id.to_string(),
            content: content.to_string(),
            status: status.to_string(),
        }
    }

    fn scratchpad(entries: usize) -> Option<ScratchpadSummary> {
        Some(ScratchpadSummary {
            path: "~/.joey/scratchpad/session.md".to_string(),
            entries,
            last_entry_at: Some("2026-09-10T12:00:00Z".to_string()),
        })
    }

    #[test]
    fn render_with_todos() {
        let todos = vec![
            todo("1", "Write the renderer", "completed"),
            todo("2", "Wire into the turn loop", "in_progress"),
        ];
        let input = StateBlockInput {
            todos: &todos,
            scratchpad: scratchpad(3),
            turn: 3,
            max_turns: 90,
        };
        let block = render(&input, 2000).expect("expected Some(block)");
        assert!(block.contains("[STATE BLOCK — deterministic, auto-maintained]"));
        assert!(block.contains("TASKS:"));
        assert!(block.contains("[x] Write the renderer"));
        assert!(block.contains("[>] Wire into the turn loop"));
        assert!(block.contains("SCRATCHPAD:"));
        assert!(block.contains("3 entries"));
        assert!(block.contains("PROGRESS: turn 3 of 90"));
    }

    #[test]
    fn empty_renders_none() {
        let empty: Vec<TodoItem> = Vec::new();
        let input = StateBlockInput {
            todos: &empty,
            scratchpad: None,
            turn: 1,
            max_turns: 90,
        };
        assert!(render(&input, 2000).is_none());

        let input = StateBlockInput {
            todos: &empty,
            scratchpad: scratchpad(0),
            turn: 1,
            max_turns: 90,
        };
        assert!(render(&input, 2000).is_none());
    }

    #[test]
    fn bound_never_exceeded() {
        let todos: Vec<TodoItem> = (0..200)
            .map(|i| todo(&format!("t{}", i), &"x".repeat(50), "pending"))
            .collect();
        let input = StateBlockInput {
            todos: &todos,
            scratchpad: scratchpad(2),
            turn: 3,
            max_turns: 90,
        };
        let block = render(&input, 200).expect("expected Some(block)");
        assert!(block.chars().count() <= 200);
        assert!(block.contains("TASKS:"));
        assert!(block.contains("PROGRESS:"));
    }

    #[test]
    fn deterministic_same_input_same_output() {
        let todos = vec![
            todo("1", "First task", "completed"),
            todo("2", "Second task", "in_progress"),
            todo("3", "Third task", "pending"),
        ];
        let input = StateBlockInput {
            todos: &todos,
            scratchpad: scratchpad(1),
            turn: 5,
            max_turns: 90,
        };
        let a = render(&input, 120).expect("expected Some(block)");
        let b = render(&input, 120).expect("expected Some(block)");
        assert_eq!(a, b);
    }

    #[test]
    fn truncation_keeps_headers() {
        let todos: Vec<TodoItem> = (0..100)
            .map(|i| todo(&format!("t{}", i), &"y".repeat(60), "pending"))
            .collect();
        let input = StateBlockInput {
            todos: &todos,
            scratchpad: scratchpad(4),
            turn: 7,
            max_turns: 90,
        };
        let block = render(&input, 300).expect("expected Some(block)");
        assert!(block.contains("[STATE BLOCK"));
        assert!(block.contains("TASKS:"));
        assert!(block.contains("SCRATCHPAD:"));
        assert!(block.contains("PROGRESS:"));
        assert!(block.chars().count() <= 300);
    }

    #[test]
    fn todos_only_and_scratchpad_only() {
        // Todos only, no scratchpad.
        let todos = vec![todo("1", "Only task", "pending")];
        let input = StateBlockInput {
            todos: &todos,
            scratchpad: None,
            turn: 2,
            max_turns: 90,
        };
        let block = render(&input, 2000).expect("expected Some(block)");
        assert!(block.contains("[ ] Only task"));
        assert!(block.contains("SCRATCHPAD: (no entries)"));

        // Scratchpad only, no todos.
        let empty: Vec<TodoItem> = Vec::new();
        let input = StateBlockInput {
            todos: &empty,
            scratchpad: scratchpad(2),
            turn: 2,
            max_turns: 90,
        };
        let block = render(&input, 2000).expect("expected Some(block)");
        assert!(block.contains("TASKS:"));
        assert!(block.contains("(none)"));
    }
}
