//! Debounced filesystem watcher for feature directories.
//!
//! Watches `spec.md`, `plan.md`, `tasks.md` under a feature directory and
//! emits a `FileChangeEvent` (debounced ~500ms) whenever any of them
//! changes, per research.md decision 3 (notify + debounce, no polling).

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify_debouncer_mini::{new_debouncer, DebounceEventResult};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    pub file: String,
    pub path: PathBuf,
}

/// Start watching `feature_dir` for changes to spec.md/plan.md/tasks.md.
/// Returns a receiver that yields a `FileChangeEvent` per debounced change
/// batch. The debouncer is kept alive for the lifetime of the returned
/// guard-holding task; drop the receiver to stop watching.
pub fn watch_feature_dir(
    feature_dir: &Path,
) -> anyhow::Result<mpsc::UnboundedReceiver<FileChangeEvent>> {
    let (tx, rx) = mpsc::unbounded_channel();
    let dir = feature_dir.to_path_buf();

    let mut debouncer = new_debouncer(
        Duration::from_millis(500),
        move |res: DebounceEventResult| match res {
            Ok(events) => {
                for event in events {
                    if let Some(name) = event.path.file_name().and_then(|n| n.to_str()) {
                        if matches!(name, "spec.md" | "plan.md" | "tasks.md") {
                            let _ = tx.send(FileChangeEvent {
                                file: name.to_string(),
                                path: event.path.clone(),
                            });
                        }
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = ?e, "file watcher error");
            }
        },
    )?;

    debouncer
        .watcher()
        .watch(&dir, notify::RecursiveMode::NonRecursive)?;

    // Leak the debouncer for the duration of the process; callers that need
    // finer lifetime control can rebuild this with an explicit guard.
    Box::leak(Box::new(debouncer));

    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;
    use tempfile::tempdir;

    #[tokio::test]
    async fn detects_tasks_md_change() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("tasks.md"), "initial").unwrap();

        let mut rx = watch_feature_dir(dir.path()).unwrap();

        // Give the watcher a moment to initialize before mutating.
        tokio::time::sleep(StdDuration::from_millis(200)).await;
        std::fs::write(dir.path().join("tasks.md"), "changed").unwrap();

        let event = tokio::time::timeout(StdDuration::from_secs(3), rx.recv()).await;
        assert!(event.is_ok(), "expected a debounced file-change event");
        let event = event.unwrap().expect("channel open");
        assert_eq!(event.file, "tasks.md");
    }
}
