//! Plan parser: parse `.omo/plans/{name}.md` task lists.
//!
//! Port of contracts/orchestration-pipeline.md BC-031.
//! Extracts task rows matching `- [ ] N. <title>` (implementation) and
//! `- [ ] F<num>. <title>` (final verification).

use serde::{Deserialize, Serialize};

/// Numeric offset placing F-tasks in a range implementation tasks can never
/// reach, so `F1` and task `1` stay distinct in number-keyed collections
/// (completion sets, dependency unblocking).
pub const F_TASK_NUMBER_OFFSET: usize = 1 << 30;

/// A single parsed task from a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParsedTask {
    /// Task number (N from `- [ ] N. <title>`).
    pub number: usize,
    /// Task title.
    pub title: String,
    /// Whether this is a final verification task (F<num> prefix).
    pub is_final_verification: bool,
    /// Dependencies (task numbers this task depends on), if any.
    #[serde(default)]
    pub dependencies: Vec<usize>,
    /// Whether the task is completed.
    pub completed: bool,
}

/// A parsed plan with tasks and dependency information.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ParsedPlan {
    pub tasks: Vec<ParsedTask>,
}

impl ParsedPlan {
    /// Implementation tasks (non-final-verification).
    pub fn implementation_tasks(&self) -> Vec<&ParsedTask> {
        self.tasks.iter().filter(|t| !t.is_final_verification).collect()
    }

    /// Final verification tasks.
    pub fn final_verification_tasks(&self) -> Vec<&ParsedTask> {
        self.tasks.iter().filter(|t| t.is_final_verification).collect()
    }

    /// Tasks in dependency order — tasks with unmet dependencies are not
    /// started until blockers complete (BC-032).
    pub fn ready_tasks(&self, completed: &std::collections::HashSet<usize>) -> Vec<&ParsedTask> {
        self.tasks
            .iter()
            .filter(|t| {
                !t.completed
                    && !completed.contains(&t.number)
                    && t.dependencies.iter().all(|dep| completed.contains(dep))
            })
            .collect()
    }
}

/// Parse a single `> Depends on:` token strictly.
///
/// Accepted forms:
///   - `N`  → implementation task N
///   - `FN` → final-verification task N (mapped into the F-number range
///     via `F_TASK_NUMBER_OFFSET`, matching how task rows store F-numbers)
///
/// Anything else is invalid and is dropped with a recorded warning —
/// previously unparseable tokens were silently discarded by a
/// `filter_map`, hiding plan-authoring mistakes like `F1x` or `banana`.
fn parse_dep_token(token: &str) -> Option<usize> {
    let token = token.trim();
    let parsed = if let Some(num_part) = token.strip_prefix('F') {
        num_part
            .parse::<usize>()
            .ok()
            .map(|n| F_TASK_NUMBER_OFFSET + n)
    } else {
        token.parse::<usize>().ok()
    };
    match parsed {
        Some(dep) => Some(dep),
        None => {
            tracing::warn!(token = token, "invalid plan dependency token dropped");
            None
        }
    }
}

/// Parse a plan markdown document into a structured plan (T102, BC-031).
///
/// Recognizes:
///   `- [ ] N. <title>` — implementation task (N is a number)
///   `- [ ] F<num>. <title>` — final verification task
///   `- [x] N. <title>` — completed task
///
/// Dependency lines (optional): `> Depends on: N, M` following a task.
pub fn parse_plan(markdown: &str) -> ParsedPlan {
    let mut tasks: Vec<ParsedTask> = Vec::new();
    let mut last_task_deps: Vec<usize>;

    for line in markdown.lines() {
        let trimmed = line.trim();

        // Check for dependency annotation
        if let Some(rest) = trimmed.strip_prefix("> Depends on:") {
            last_task_deps = rest.split(',').filter_map(parse_dep_token).collect();
            // Attach to the last task. A dependency line BEFORE the first
            // task row has nothing to attach to — skip it with a recorded
            // warning instead of silently dropping the constraint.
            match tasks.last_mut() {
                Some(task) => task.dependencies = last_task_deps.clone(),
                None => tracing::warn!(
                    deps = ?last_task_deps,
                    "plan dependency line before the first task ignored"
                ),
            }
            continue;
        }

        // Match task lines: `- [ ] N. <title>` or `- [ ] FN. <title>` or `- [x] ...`
        let (checked, rest) = if let Some(r) = trimmed.strip_prefix("- [x]") {
            (true, r)
        } else if let Some(r) = trimmed.strip_prefix("- [X]") {
            (true, r)
        } else if let Some(r) = trimmed.strip_prefix("- [ ]") {
            (false, r)
        } else {
            continue;
        };

        let rest = rest.trim();

        // Check for final verification task: F<num>
        let (number, title, is_final) = if let Some(num_part) = rest.strip_prefix('F') {
            // F<num>. title
            if let Some(dot_pos) = num_part.find('.') {
                if let Ok(num) = num_part[..dot_pos].parse::<usize>() {
                    let title = num_part[dot_pos + 1..].trim().to_string();
                    // F-tasks live in a distinct numeric range: storing the
                    // raw F-number collided with the implementation task of
                    // the same number (F1 vs task 1) in every `number`-keyed
                    // lookup (completion sets, dependency unblocking).
                    (F_TASK_NUMBER_OFFSET + num, title, true)
                } else {
                    continue;
                }
            } else {
                continue;
            }
        } else {
            // Regular: N. title
            if let Some(dot_pos) = rest.find('.') {
                if let Ok(num) = rest[..dot_pos].parse::<usize>() {
                    let title = rest[dot_pos + 1..].trim().to_string();
                    (num, title, false)
                } else {
                    continue;
                }
            } else {
                continue;
            }
        };

        tasks.push(ParsedTask {
            number,
            title,
            is_final_verification: is_final,
            dependencies: Vec::new(),
            completed: checked,
        });
    }

    // Validate dependencies: every dep must refer to an existing task
    // number. A dangling dep (typo, removed task) would otherwise stall the
    // task forever in `ready_tasks` (its blocker never completes), so drop
    // it with a recorded warning instead.
    let known_numbers: std::collections::HashSet<usize> =
        tasks.iter().map(|t| t.number).collect();
    for task in tasks.iter_mut() {
        let (valid, invalid): (Vec<usize>, Vec<usize>) = task
            .dependencies
            .iter()
            .partition(|dep| known_numbers.contains(dep));
        if !invalid.is_empty() {
            tracing::warn!(
                task = task.number,
                invalid_deps = ?invalid,
                "plan dependencies referencing unknown tasks dropped"
            );
            task.dependencies = valid;
        }
    }

    ParsedPlan { tasks }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T102: parse a sample plan markdown and extract correct task list.
    #[test]
    fn parse_sample_plan() {
        let markdown = r#"# Plan: Feature Implementation

## Tasks

- [ ] 1. Set up project structure
- [ ] 2. Implement core logic
> Depends on: 1
- [ ] 3. Write tests
> Depends on: 2
- [ ] F1. Final verification: all tests pass

## Completed (for reference)
- [x] 0. Initial setup (already done)
"#;

        let plan = parse_plan(markdown);

        // 5 tasks total (0-3 + F1)
        assert_eq!(plan.tasks.len(), 5);

        // Task 1
        let t1 = plan.tasks.iter().find(|t| t.number == 1).unwrap();
        assert_eq!(t1.title, "Set up project structure");
        assert!(!t1.is_final_verification);
        assert!(!t1.completed);

        // Task 2 has dependency on 1
        let t2 = plan.tasks.iter().find(|t| t.number == 2).unwrap();
        assert_eq!(t2.dependencies, vec![1]);

        // Task 3 has dependency on 2
        let t3 = plan.tasks.iter().find(|t| t.number == 3).unwrap();
        assert_eq!(t3.dependencies, vec![2]);

        // F1 is final verification — its number is offset into the F-range
        // so it cannot collide with implementation task 1.
        let f1 = plan.tasks.iter().find(|t| t.is_final_verification).unwrap();
        assert_eq!(f1.number, F_TASK_NUMBER_OFFSET + 1);
        assert!(f1.title.contains("Final verification"));

        // Task 0 is completed
        let t0 = plan.tasks.iter().find(|t| t.number == 0).unwrap();
        assert!(t0.completed);
    }

    #[test]
    fn dependency_line_before_first_task_is_ignored() {
        // A `> Depends on: N` line BEFORE the first task row has nothing
        // to attach to: it is skipped with a recorded warning (not a
        // panic, not a silent corruption of the following task).
        let markdown = "# Plan\n\n> Depends on: 1\n\n- [ ] 1. First task\n";
        let plan = parse_plan(markdown);
        assert_eq!(plan.tasks.len(), 1, "the task row after the stray annotation must survive");
        assert_eq!(plan.tasks[0].number, 1);
        assert_eq!(plan.tasks[0].title, "First task");
        assert!(
            plan.tasks[0].dependencies.is_empty(),
            "stray pre-task annotation must not leak into the first task"
        );
    }

    #[test]
    fn f_prefixed_dependency_token_is_parsed() {
        // `Depends on: F1` must wire the final-verification task's offset
        // number, not be silently dropped (the old filter_map only parsed
        // plain integers). The dep line attaches to the preceding task row.
        let markdown =
            "- [ ] 1. Build feature\n> Depends on: F1\n- [ ] F1. Final verification\n";
        let plan = parse_plan(markdown);
        let t1 = plan.tasks.iter().find(|t| t.number == 1).unwrap();
        assert_eq!(
            t1.dependencies,
            vec![F_TASK_NUMBER_OFFSET + 1],
            "`F1` dep must map to the F-number range"
        );
    }

    #[test]
    fn invalid_dependency_token_is_dropped() {
        // Unparseable tokens are dropped (with a logged warning) rather
        // than silently filter-mapped away — behavior is the same drop,
        // but strict parsing means tokens like `banana` can never pass.
        let markdown = "- [ ] 1. Build\n> Depends on: banana\n- [ ] 2. Test\n";
        let plan = parse_plan(markdown);
        assert!(plan.tasks[0].dependencies.is_empty());
    }

    #[test]
    fn dependency_on_unknown_task_is_dropped_no_stall() {
        // `Depends on: 99` with no task 99 must be dropped so the task
        // does not stall forever waiting for a blocker that can never
        // complete.
        let markdown = "- [ ] 1. Build\n> Depends on: 99\n";
        let plan = parse_plan(markdown);
        assert!(
            plan.tasks[0].dependencies.is_empty(),
            "dangling dep must be dropped, not kept"
        );
        // And the task is ready immediately (no stall).
        let completed = std::collections::HashSet::new();
        let ready = plan.ready_tasks(&completed);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].number, 1);
    }

    #[test]
    fn valid_dependencies_are_kept_after_validation() {
        // Validation must only drop invalid deps; valid ones survive.
        let markdown = "- [ ] 1. Build\n- [ ] 2. Test\n> Depends on: 1, 99, banana\n";
        let plan = parse_plan(markdown);
        let t2 = plan.tasks.iter().find(|t| t.number == 2).unwrap();
        assert_eq!(t2.dependencies, vec![1], "only the valid dep `1` survives");
    }

    #[test]
    fn ready_tasks_respects_dependencies() {
        let plan = ParsedPlan {
            tasks: vec![
                ParsedTask {
                    number: 1,
                    title: "First".into(),
                    is_final_verification: false,
                    dependencies: vec![],
                    completed: false,
                },
                ParsedTask {
                    number: 2,
                    title: "Second".into(),
                    is_final_verification: false,
                    dependencies: vec![1],
                    completed: false,
                },
            ],
        };

        let empty: std::collections::HashSet<usize> = std::collections::HashSet::new();
        let ready = plan.ready_tasks(&empty);
        // Only task 1 is ready (no deps), task 2 depends on 1
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].number, 1);

        // After completing task 1
        let mut completed = std::collections::HashSet::new();
        completed.insert(1);
        let ready2 = plan.ready_tasks(&completed);
        assert_eq!(ready2.len(), 1);
        assert_eq!(ready2[0].number, 2);
    }
}
