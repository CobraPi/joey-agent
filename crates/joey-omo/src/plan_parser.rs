//! Plan parser: parse `.omo/plans/{name}.md` task lists.
//!
//! Port of contracts/orchestration-pipeline.md BC-031.
//! Extracts task rows matching `- [ ] N. <title>` (implementation) and
//! `- [ ] F<num>. <title>` (final verification).

use serde::{Deserialize, Serialize};

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
    let mut last_task_deps: Vec<usize> = Vec::new();

    for line in markdown.lines() {
        let trimmed = line.trim();

        // Check for dependency annotation
        if let Some(rest) = trimmed.strip_prefix("> Depends on:") {
            last_task_deps = rest
                .split(',')
                .filter_map(|s| s.trim().parse::<usize>().ok())
                .collect();
            // Attach to the last task
            if let Some(task) = tasks.last_mut() {
                task.dependencies = last_task_deps.clone();
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
                    (num, title, true)
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

        // F1 is final verification
        let f1 = plan.tasks.iter().find(|t| t.is_final_verification).unwrap();
        assert_eq!(f1.number, 1);
        assert!(f1.title.contains("Final verification"));

        // Task 0 is completed
        let t0 = plan.tasks.iter().find(|t| t.number == 0).unwrap();
        assert!(t0.completed);
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
