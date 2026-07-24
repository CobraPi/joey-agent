//! NotepadStore: append-only markdown wisdom accumulation under `.omo/notepads/`.
//!
//! Port of data-model.md `NotepadStore`. Five markdown files per plan:
//! learnings, decisions, issues, verification, problems.

use std::path::{Path, PathBuf};

/// The five notepad files. Each accumulates different wisdom categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotepadFile {
    Learnings,
    Decisions,
    Issues,
    Verification,
    Problems,
}

impl NotepadFile {
    pub fn filename(self) -> &'static str {
        match self {
            Self::Learnings => "learnings.md",
            Self::Decisions => "decisions.md",
            Self::Issues => "issues.md",
            Self::Verification => "verification.md",
            Self::Problems => "problems.md",
        }
    }

    pub fn all() -> &'static [NotepadFile] {
        &[
            Self::Learnings,
            Self::Decisions,
            Self::Issues,
            Self::Verification,
            Self::Problems,
        ]
    }
}

/// Append-only markdown notepad store for wisdom accumulation.
/// Files live under `.omo/notepads/{plan-name}/`.
#[derive(Debug, Clone)]
pub struct NotepadStore {
    base_dir: PathBuf,
}

impl NotepadStore {
    /// Create a store rooted at `.omo/notepads/{plan-name}/`.
    pub fn new(omo_dir: &Path, plan_name: &str) -> Self {
        Self {
            base_dir: omo_dir.join("notepads").join(plan_name),
        }
    }

    /// The base directory for this plan's notepads.
    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Append content to a notepad file (VR-005: append-only, never rewritten).
    /// Creates the file if it doesn't exist. Creates the directory if needed.
    pub fn append(&self, file: NotepadFile, content: &str) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.base_dir)?;
        let path = self.base_dir.join(file.filename());
        let entry = if content.ends_with('\n') {
            content.to_string()
        } else {
            format!("{}\n", content)
        };
        use std::io::Write;
        let mut file_handle = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        file_handle.write_all(entry.as_bytes())?;
        Ok(())
    }

    /// Read all content from all 5 notepad files, concatenated (T101).
    /// Used for passing accumulated wisdom forward to subsequent subagents.
    pub fn read_all(&self) -> String {
        let mut result = String::new();
        for file in NotepadFile::all() {
            let path = self.base_dir.join(file.filename());
            if let Ok(contents) = std::fs::read_to_string(&path) {
                if !contents.is_empty() {
                    result.push_str(&format!("## {}\n\n{}\n\n", file.filename(), contents));
                }
            }
        }
        result
    }

    /// Read a single notepad file.
    pub fn read(&self, file: NotepadFile) -> String {
        let path = self.base_dir.join(file.filename());
        std::fs::read_to_string(&path).unwrap_or_default()
    }
}

/// Extract learnings from a delegation result summary (T088).
///
/// Parses the summary for conventions, successes, failures, and gotchas,
/// appending them to the appropriate notepad files.
pub fn extract_and_append_learnings(
    store: &NotepadStore,
    summary: &str,
) -> std::io::Result<()> {
    // Heuristic extraction: look for key markers in the summary.
    let lower = summary.to_ascii_lowercase();

    // Patterns → notepad file
    let conventions: Vec<&str> = summary
        .lines()
        .filter(|l| {
            let ll = l.to_ascii_lowercase();
            ll.contains("convention") || ll.contains("pattern") || ll.contains("standard")
        })
        .collect();

    let issues: Vec<&str> = summary
        .lines()
        .filter(|l| {
            let ll = l.to_ascii_lowercase();
            ll.contains("error") || ll.contains("failed") || ll.contains("issue") || ll.contains("gotcha")
        })
        .collect();

    let decisions: Vec<&str> = summary
        .lines()
        .filter(|l| {
            let ll = l.to_ascii_lowercase();
            ll.contains("decided") || ll.contains("chose") || ll.contains("rationale")
        })
        .collect();

    let _ = &lower;

    if !conventions.is_empty() {
        store.append(
            NotepadFile::Learnings,
            &format!("- Conventions:\n{}", conventions.iter().map(|l| format!("  {}", l)).collect::<Vec<_>>().join("\n")),
        )?;
    }
    if !issues.is_empty() {
        store.append(
            NotepadFile::Issues,
            &format!("- Issues:\n{}", issues.iter().map(|l| format!("  {}", l)).collect::<Vec<_>>().join("\n")),
        )?;
    }
    if !decisions.is_empty() {
        store.append(
            NotepadFile::Decisions,
            &format!("- Decisions:\n{}", decisions.iter().map(|l| format!("  {}", l)).collect::<Vec<_>>().join("\n")),
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// T101: append() adds content without overwriting; read_all() returns
    /// concatenated content from all 5 files.
    #[test]
    fn append_and_read_all() {
        let dir = tempdir().unwrap();
        let store = NotepadStore::new(dir.path(), "test-plan");

        // Append to learnings
        store.append(NotepadFile::Learnings, "First learning").unwrap();
        store.append(NotepadFile::Learnings, "Second learning").unwrap();

        // Append to decisions
        store.append(NotepadFile::Decisions, "Key decision").unwrap();

        // Read learnings — both entries present (not overwritten)
        let learnings = store.read(NotepadFile::Learnings);
        assert!(learnings.contains("First learning"));
        assert!(learnings.contains("Second learning"));

        // Read all — concatenated
        let all = store.read_all();
        assert!(all.contains("learnings.md"));
        assert!(all.contains("First learning"));
        assert!(all.contains("decisions.md"));
        assert!(all.contains("Key decision"));
    }
}
