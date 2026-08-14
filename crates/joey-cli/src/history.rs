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
    let Ok(content) = std::fs::read_to_string(history_path()) else {
        return Vec::new();
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(decode_entry)
        .collect()
}

/// Append one entry (dedup against the previous entry — reedline semantics).
/// Creates the file (and its parent directory) when missing.
pub fn record(text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let path = history_path();
    let mut existing = load();
    if existing.last().map(|l| l.as_str()) == Some(text) {
        return; // consecutive duplicate — skip
    }
    existing.push(text.to_string());
    // Enforce the cap (drop oldest).
    if existing.len() > CAPACITY {
        let drop_n = existing.len() - CAPACITY;
        existing.drain(0..drop_n);
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let Ok(mut f) = std::fs::File::create(&path) else {
        return;
    };
    for entry in &existing {
        let _ = writeln!(f, "{}", encode_entry(entry));
    }
    let _ = f.flush();
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
