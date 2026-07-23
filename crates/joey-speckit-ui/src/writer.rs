//! Conflict-checked writes to feature source Markdown files.

use std::path::Path;

use thiserror::Error;

use crate::conflict::{check_conflict, content_hash, ConflictError};

#[derive(Debug, Error)]
pub enum WriteError {
    #[error("conflict: {0}")]
    Conflict(#[from] ConflictError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Read a file's current content and hash, without modifying it.
pub fn read_with_hash(path: &Path) -> Result<(String, String), WriteError> {
    let content = std::fs::read_to_string(path)?;
    let hash = content_hash(&content);
    Ok((content, hash))
}

/// Overwrite `path` with `new_content` iff the file's current content hash
/// matches `based_on_hash`. On conflict, the file is left unmodified and
/// `WriteError::Conflict` is returned with the actual current hash.
pub fn write_if_unchanged(
    path: &Path,
    new_content: &str,
    based_on_hash: &str,
) -> Result<String, WriteError> {
    let current_content = std::fs::read_to_string(path).unwrap_or_default();
    check_conflict(&current_content, based_on_hash)?;
    std::fs::write(path, new_content)?;
    Ok(content_hash(new_content))
}

/// Replace a single line matching `target_text` (exact, trimmed match) in
/// the file at `path` with `new_text`, honoring conflict detection.
/// Returns the new full content hash on success.
pub fn replace_line_if_unchanged(
    path: &Path,
    target_text: &str,
    new_text: &str,
    based_on_hash: &str,
) -> Result<String, WriteError> {
    let current_content = std::fs::read_to_string(path).unwrap_or_default();
    check_conflict(&current_content, based_on_hash)?;

    let mut replaced = false;
    let new_content: String = current_content
        .lines()
        .map(|line| {
            if !replaced && line.trim() == target_text.trim() {
                replaced = true;
                new_text.to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");

    let new_content = if current_content.ends_with('\n') {
        format!("{}\n", new_content)
    } else {
        new_content
    };

    std::fs::write(path, &new_content)?;
    Ok(content_hash(&new_content))
}

/// Mark a single task line (identified by its leading `T###` id) as done
/// (`- [X] ...`) in `specs/<feature_id>/tasks.md`. This performs a raw
/// filesystem write (not conflict-checked against a caller-supplied hash)
/// because it is invoked from the execution pipeline after a task run
/// completes, not from a user-initiated edit — the completion event itself
/// is the source of truth. If the tasks.md file or the matching task line
/// cannot be found, this is a no-op (logged by the caller) rather than an
/// error, since the UI's view of task status doesn't have to block a
/// successful task-execution response.
pub fn mark_task_complete(
    repo_root: &Path,
    feature_id: &str,
    task_id: &str,
) -> std::io::Result<()> {
    let tasks_path = repo_root.join("specs").join(feature_id).join("tasks.md");
    let content = match std::fs::read_to_string(&tasks_path) {
        Ok(c) => c,
        Err(_) => return Ok(()),
    };

    let mut changed = false;
    let new_content: String = content
        .lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if !changed
                && (trimmed.starts_with("- [ ]") || trimmed.starts_with("- [~]") || trimmed.starts_with("- [-]"))
            {
                // Confirm the line's task id token matches task_id exactly
                // (avoid e.g. T001 matching T0010).
                let after_checkbox = trimmed
                    .split_once(']')
                    .map(|x| x.1)
                    .unwrap_or("")
                    .trim_start();
                let id_token = after_checkbox.split_whitespace().next().unwrap_or("");
                if id_token == task_id {
                    changed = true;
                    return line.replacen("- [ ]", "- [X]", 1).replacen("- [~]", "- [X]", 1).replacen("- [-]", "- [X]", 1);
                }
            }
            line.to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");

    if !changed {
        return Ok(());
    }

    let new_content = if content.ends_with('\n') {
        format!("{}\n", new_content)
    } else {
        new_content
    };

    std::fs::write(&tasks_path, new_content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn write_succeeds_when_hash_matches() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("spec.md");
        std::fs::write(&path, "original").unwrap();
        let hash = content_hash("original");
        let new_hash = write_if_unchanged(&path, "updated", &hash).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated");
        assert_eq!(new_hash, content_hash("updated"));
    }

    #[test]
    fn write_rejected_on_stale_hash_and_file_unmodified() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("spec.md");
        std::fs::write(&path, "original").unwrap();
        let stale_hash = content_hash("not-original");
        let err = write_if_unchanged(&path, "updated", &stale_hash);
        assert!(err.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
    }

    #[test]
    fn mark_task_complete_flips_matching_todo_line_only() {
        let dir = tempdir().unwrap();
        let feature_dir = dir.path().join("specs").join("001-test");
        std::fs::create_dir_all(&feature_dir).unwrap();
        std::fs::write(
            feature_dir.join("tasks.md"),
            "- [ ] T001 First task in src/a.rs\n- [ ] T002 Second task in src/b.rs\n",
        )
        .unwrap();

        mark_task_complete(dir.path(), "001-test", "T002").unwrap();

        let content = std::fs::read_to_string(feature_dir.join("tasks.md")).unwrap();
        assert_eq!(
            content,
            "- [ ] T001 First task in src/a.rs\n- [X] T002 Second task in src/b.rs\n"
        );
    }

    #[test]
    fn mark_task_complete_is_noop_when_task_not_found() {
        let dir = tempdir().unwrap();
        let feature_dir = dir.path().join("specs").join("001-test");
        std::fs::create_dir_all(&feature_dir).unwrap();
        std::fs::write(feature_dir.join("tasks.md"), "- [ ] T001 Only task\n").unwrap();

        // Should not error even though T999 doesn't exist.
        mark_task_complete(dir.path(), "001-test", "T999").unwrap();

        let content = std::fs::read_to_string(feature_dir.join("tasks.md")).unwrap();
        assert_eq!(content, "- [ ] T001 Only task\n");
    }

    #[test]
    fn mark_task_complete_is_noop_when_tasks_file_missing() {
        let dir = tempdir().unwrap();
        // No specs/ dir at all — must not error.
        mark_task_complete(dir.path(), "001-test", "T001").unwrap();
    }
}
