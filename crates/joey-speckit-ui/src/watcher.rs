//! Debounced filesystem watcher for feature directories.
//!
//! Watches `spec.md`, `plan.md`, `tasks.md` under a feature directory and
//! emits a `FileChangeEvent` (debounced ~500ms) whenever any of them
//! changes, per research.md decision 3 (notify + debounce, no polling).
//!
//! One debouncer per distinct feature directory (a process-global registry):
//! WS connections come and go, but the watcher survives and is REUSED —
//! the old per-connection `Box::leak` accumulated a fresh fs-watch for
//! every websocket ever opened.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use notify_debouncer_mini::{new_debouncer, DebounceEventResult};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct FileChangeEvent {
    pub file: String,
    pub path: PathBuf,
}

/// Per-dir fan-out state: senders for every live receiver.
struct DirWatch {
    senders: Vec<mpsc::UnboundedSender<FileChangeEvent>>,
}

static WATCHERS: std::sync::OnceLock<
    std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<std::sync::Mutex<DirWatch>>>>,
> = std::sync::OnceLock::new();

fn watchers() -> &'static std::sync::Mutex<HashMap<PathBuf, std::sync::Arc<std::sync::Mutex<DirWatch>>>> {
    WATCHERS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Start (or join) watching `feature_dir` for changes to
/// spec.md/plan.md/tasks.md. Returns a receiver yielding one
/// `FileChangeEvent` per debounced change batch. Dropping the receiver
/// detaches this subscriber; the underlying fs-watch is shared per dir
/// (dead subscribers are pruned on the next event).
pub fn watch_feature_dir(
    feature_dir: &Path,
) -> anyhow::Result<mpsc::UnboundedReceiver<FileChangeEvent>> {
    let (tx, rx) = mpsc::unbounded_channel();
    let dir = feature_dir.to_path_buf();

    let mut reg = watchers().lock().unwrap_or_else(|p| p.into_inner());
    let entry = reg.entry(dir.clone()).or_insert_with(|| {
        // First watcher for this dir: create the debouncer + shared state.
        let state = std::sync::Arc::new(std::sync::Mutex::new(DirWatch { senders: Vec::new() }));
        let cb_state = state.clone();
        let mut debouncer = new_debouncer(
            Duration::from_millis(500),
            move |res: DebounceEventResult| match res {
                Ok(events) => {
                    for event in events {
                        if let Some(name) = event.path.file_name().and_then(|n| n.to_str()) {
                            if matches!(name, "spec.md" | "plan.md" | "tasks.md") {
                                let ev = FileChangeEvent {
                                    file: name.to_string(),
                                    path: event.path.clone(),
                                };
                                let mut st = cb_state.lock().unwrap_or_else(|p| p.into_inner());
                                // Prune dead subscribers as we go.
                                st.senders.retain(|s| s.send(ev.clone()).is_ok());
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = ?e, "file watcher error");
                }
            },
        )
        .expect("debouncer construction is infallible for the default backend");
        if let Err(e) = debouncer
            .watcher()
            .watch(&dir, notify::RecursiveMode::NonRecursive)
        {
            tracing::warn!(error = ?e, dir = %dir.display(), "fs watch registration failed");
        }
        // Leak ONE debouncer per dir for the process lifetime — bounded by
        // the number of distinct feature dirs, not by connection count.
        Box::leak(Box::new(debouncer));
        state
    });

    entry
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .senders
        .push(tx);

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
