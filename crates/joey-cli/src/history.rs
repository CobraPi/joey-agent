//! Shared input-history file for the CLI and TUI surfaces (feature: full
//! command history).
//!
//! Both surfaces persist to `~/.joey/.joey_history` — the SAME file the CLI's
//! reedline `FileBackedHistory` uses. reedline's on-disk format is one entry
//! per line with embedded newlines escaped as the literal 4-char sequence
//! `<\n>` (reedline `history::file_backed::NEWLINE_ESCAPE`). The TUI reads and
//! writes the identical format so a prompt entered in either surface is
//! recallable in both.

use std::io::Write;
use std::path::PathBuf;

/// reedline's newline escape (file_backed.rs `NEWLINE_ESCAPE`).
const NEWLINE_ESCAPE: &str = "<\\n>";
/// Cap matching reedline's FileBackedHistory capacity in the REPL.
pub const CAPACITY: usize = 10_000;

/// The shared history file path (`~/.joey/.joey_history`).
pub fn history_path() -> PathBuf {
    joey_core::joey_home().join(".joey_history")
}

fn encode_entry(s: &str) -> String {
    s.replace('\n', NEWLINE_ESCAPE)
}

fn decode_entry(s: &str) -> String {
    s.replace(NEWLINE_ESCAPE, "\n")
}

/// Load the full shared history (oldest first, newest last).
pub fn load() -> Vec<String> {
    load_at(&history_path())
}

fn load_at(path: &std::path::Path) -> Vec<String> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(decode_entry)
        .collect()
}

/// Guard a read-modify-write cycle on the shared history file. The CLI REPL
/// and the TUI are separate processes writing the SAME file; an unguarded
/// truncate-rewrite interleaves with the other writer and tears the file or
/// silently drops entries. Lock protocol: `create_new` (O_EXCL) lock file,
/// spin briefly, break locks stale for > 10 s (a crashed writer), and fall
/// back to proceeding unlocked after 5 s — history is best-effort and must
/// never hang the UI on a wedged lock.
fn with_history_lock<T>(path: &std::path::Path, f: impl FnOnce() -> T) -> T {
    use std::io::ErrorKind;
    let lock_path = path.with_extension("lock");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut locked = None;
    loop {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(f) => {
                locked = Some(f);
                break;
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                // Break a stale lock (writer crashed mid-cycle).
                if let Ok(md) = std::fs::metadata(&lock_path) {
                    let stale = md
                        .modified()
                        .ok()
                        .and_then(|m| m.elapsed().ok())
                        .map(|d| d > std::time::Duration::from_secs(10))
                        .unwrap_or(false);
                    if stale {
                        let _ = std::fs::remove_file(&lock_path);
                        continue;
                    }
                }
                if std::time::Instant::now() > deadline {
                    break; // wedged — proceed unlocked (best-effort)
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(_) => break, // lock dir unwritable — proceed unlocked
        }
    }
    let out = f();
    drop(locked);
    let _ = std::fs::remove_file(&lock_path);
    out
}

/// Append one entry (dedup against the previous entry — reedline semantics).
/// Creates the file (and its parent directory) when missing.
pub fn record(text: &str) {
    record_at(&history_path(), text);
}

fn record_at(path: &std::path::Path, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    with_history_lock(path, || {
        let mut existing = load_at(path);
        if existing.last().map(|l| l.as_str()) == Some(text) {
            return; // consecutive duplicate — skip
        }
        existing.push(text.to_string());
        // Enforce the cap (drop oldest).
        if existing.len() > CAPACITY {
            let drop_n = existing.len() - CAPACITY;
            existing.drain(0..drop_n);
        }
        // Commit atomically: write a unique sibling temp file, then rename
        // over the real path — a concurrent reader never sees a torn file.
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        let Ok(mut f) = std::fs::File::create(&tmp) else {
            return;
        };
        for entry in &existing {
            let _ = writeln!(f, "{}", encode_entry(entry));
        }
        let _ = f.flush();
        let _ = f.sync_all();
        let _ = std::fs::rename(&tmp, path);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let s = "line one\nline two /model gpt-4o";
        assert_eq!(decode_entry(&encode_entry(s)), s);
    }

    #[test]
    fn encode_has_no_raw_newline() {
        assert!(!encode_entry("a\nb").contains('\n'));
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;

    /// Regression: `record()` used to truncate the shared file in place
    /// (File::create) while another surface (CLI or TUI) was mid-rewrite —
    /// interleaved read-modify-write cycles silently dropped entries and
    /// could leave a torn (half-written) file. With the lock-file guarded
    /// read-modify-write + atomic temp-rename commit, concurrent recorders
    /// never lose entries and readers never see a partial file.
    #[test]
    fn concurrent_records_do_not_lose_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(".joey_history");
        let path = std::sync::Arc::new(path);
        let mut handles = Vec::new();
        for t in 0..8 {
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..25 {
                    record_at(&path, &format!("t{t}-e{i}"));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let all = load_at(&path);
        for t in 0..8 {
            for i in 0..25 {
                let text = format!("t{t}-e{i}");
                assert!(
                    all.contains(&text),
                    "entry {text} lost — history file was torn by concurrent writers"
                );
            }
        }
        assert_eq!(all.len(), 8 * 25, "every concurrent record survives");
        // No temp litter left behind.
        let leftovers: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains("tmp."))
            .collect();
        assert!(leftovers.is_empty(), "temp commit files cleaned up");
    }
}
