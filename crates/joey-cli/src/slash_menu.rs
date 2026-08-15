//! Smart completion for the CLI line editor (port of upstream
//! `hermes_cli/commands.py::SlashCommandCompleter` + `SlashCommandAutoSuggest`).
//!
//! Stages, in order:
//! 1. `/…` input — slash-command names/aliases; after `/cmd ` the command's
//!    subcommands (derived from args_hint pipe patterns).
//! 2. Non-slash input with an `@` word — Claude-Code-style context refs
//!    (@diff/@staged/@file:/@folder:/@git:/@url:) + fuzzy project files.
//! 3. Non-slash input with a path-like word — filesystem completions.
//!
//! `SmartHinter` provides fish-style ghost text: the remainder of a unique
//! slash-name/subcommand completion, falling back to history.

use reedline::{Completer, Hinter, Span, Suggestion};
use std::path::PathBuf;

use joey_tools::completion as engine;
use crate::slash;

/// Shared engine (project-file cache) — one per REPL.
pub struct SmartCompleter {
    engine: engine::CompletionEngine,
    cwd: PathBuf,
}

impl SmartCompleter {
    pub fn new(cwd: PathBuf) -> Self {
        Self { engine: engine::CompletionEngine::new(), cwd }
    }
}


/// Floor `pos` to the nearest UTF-8 char boundary in `line` (callers get
/// `pos` from reedline, which can report mid-codepoint offsets when the
/// cursor sits inside a multibyte cluster).
fn floor_to_char_boundary(line: &str, pos: usize) -> usize {
    let mut i = pos.min(line.len());
    while i > 0 && !line.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Build a [`Suggestion`] for one registry command (span filled by caller).
fn suggest(def: &slash::CommandDef, span: Span, value: String, extra: String) -> Suggestion {
    Suggestion {
        value,
        description: Some(def.description.to_string()),
        extra: Some(vec![extra]),
        style: None,
        span,
        append_whitespace: false,
    }
}


/// Subcommands for a slash command, derived from its args_hint pipes.
fn subcommands_for(name: &str) -> Vec<String> {
    crate::slash::lookup(name)
        .map(|def| engine::pipe_subcommands(def.args_hint))
        .unwrap_or_default()
}

impl Completer for SmartCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let head = &line[..floor_to_char_boundary(line, pos)];

        // ── Slash-command stages ──
        if head.starts_with('/') {
            let first_tok_end = head.find(' ').unwrap_or(head.len());
            if first_tok_end == head.len() {
                // Completing the command token itself.
                let typed = head.strip_prefix('/').unwrap_or("");
                return slash_name_suggestions(typed, Span::new(0, head.len()));
            }
            // Past the command: subcommand completion for the first arg word.
            let base = head[..first_tok_end].strip_prefix('/').unwrap_or("");
            let base_name = match slash::lookup(base) {
                Some(def) if def.implemented => def.name,
                _ => return Vec::new(),
            };
            let sub_head = &head[first_tok_end + 1..];
            if sub_head.contains(' ') {
                return Vec::new(); // past the first argument word
            }
            let subs = subcommands_for(base_name);
            let span = Span::new(first_tok_end + 1, head.len());
            return subs
                .iter()
                .filter(|s| s.starts_with(sub_head.to_lowercase().as_str()) && s.as_str() != sub_head.to_lowercase())
                .map(|s| Suggestion {
                    value: s.clone(),
                    description: Some(format!("argument of /{base_name}")),
                    extra: Some(vec![format!("args: {}", slash::lookup(base).map(|d| d.args_hint).unwrap_or("—"))]),
                    style: None,
                    span,
                    append_whitespace: false,
                })
                .collect();
        }

        // ── @-context stage ──
        if let Some(word) = engine::extract_context_word(head) {
            let span_start = head.len().saturating_sub(word.len());
            let files = self.engine.project_files_blocking(&self.cwd);
            return self
                .engine
                .context_completions(&word, &self.cwd, &files, 30)
                .into_iter()
                .map(|item| Suggestion {
                    value: item.replacement,
                    description: Some(item.meta),
                    extra: None,
                    style: None,
                    span: Span::new(span_start, head.len()),
                    append_whitespace: false,
                })
                .collect();
        }

        // ── Path stage ──
        if let Some(word) = engine::extract_path_word(head) {
            let span_start = head.len() - word.len();
            return engine::path_completions(&word, 30)
                .into_iter()
                .map(|item| Suggestion {
                    value: item.replacement,
                    description: Some(item.meta),
                    extra: None,
                    style: None,
                    span: Span::new(span_start, head.len()),
                    append_whitespace: false,
                })
                .collect();
        }

        Vec::new()
    }
}

/// Slash-name/alias suggestions for a typed fragment.
fn slash_name_suggestions(typed: &str, span: Span) -> Vec<Suggestion> {
    let mut out = Vec::new();
    for def in slash::REGISTRY {
        if typed.is_empty() || def.name.starts_with(typed) {
            let extra = format!(
                "args: {} · {}",
                if def.args_hint.is_empty() { "—" } else { def.args_hint },
                if def.implemented { "available" } else { "not yet implemented" },
            );
            out.push(suggest(def, span, format!("/{}", def.name), extra));
        }
        for alias in def.aliases {
            if !typed.is_empty() && alias.starts_with(typed) {
                out.push(suggest(def, span, format!("/{}", alias), format!("alias of /{}", def.name)));
            }
        }
    }
    out
}

/// Fish-style ghost text: remainder of a unique slash-name or subcommand
/// completion; history fallback for plain text (reedline passes history).
pub struct SmartHinter;

impl Hinter for SmartHinter {
    fn handle(
        &mut self,
        line: &str,
        pos: usize,
        history: &dyn reedline::History,
        _use_ansi_coloring: bool,
        _cwd: &str,
    ) -> String {
        if let Some(hint) = self.slash_hint(line, pos) {
            return hint;
        }
        // History fallback: newest matching entry's remainder (fish-style).
        let head = &line[..floor_to_char_boundary(line, pos)];
        if head.is_empty() {
            return String::new();
        }
        use reedline::{SearchDirection, SearchQuery};
        let query = SearchQuery::everything(SearchDirection::Backward, None);
        let Ok(items) = history.search(query) else {
            return String::new();
        };
        for item in items.iter().take(500) {
            let entry = item.command_line.as_str();
            if entry.len() > head.len() && entry.starts_with(head) {
                return entry[head.len()..].to_string();
            }
        }
        String::new()
    }

    fn complete_hint(&self) -> String {
        String::new()
    }

    fn next_hint_token(&self) -> String {
        String::new()
    }
}

impl SmartHinter {
    /// Ghost hint for slash input: `/upd` → `ate` (rest of a unique command
    /// name), `/reasoning of` → `f` (rest of a unique subcommand).
    fn slash_hint(&self, line: &str, pos: usize) -> Option<String> {
        let head = &line[..floor_to_char_boundary(line, pos)];
        if !head.starts_with('/') {
            return None;
        }
        let mut parts = head.splitn(2, ' ');
        let base = parts.next().unwrap_or("").strip_prefix('/').unwrap_or("");
        let sub = parts.next();
        match sub {
            None => {
                // Completing the command name: unique prefix remainder.
                let typed = base;
                if typed.is_empty() {
                    return None;
                }
                let mut candidates: Vec<&str> = slash::REGISTRY
                    .iter()
                    .filter(|d| d.name.starts_with(typed) && d.name != typed)
                    .map(|d| d.name)
                    .collect();
                for def in slash::REGISTRY {
                    for alias in def.aliases {
                        if alias.starts_with(typed) && *alias != typed {
                            candidates.push(alias);
                        }
                    }
                }
                candidates.sort();
                candidates.dedup();
                if candidates.len() == 1 {
                    return Some(candidates[0][typed.len()..].to_string());
                }
                None
            }
            Some(sub) => {
                if sub.contains(' ') {
                    return None;
                }
                let def = slash::lookup(base)?;
                let subs = subcommands_for(def.name);
                let mut candidates: Vec<&str> = subs
                    .iter()
                    .filter(|s| s.starts_with(sub) && s.as_str() != sub)
                    .map(|s| s.as_str())
                    .collect();
                candidates.sort();
                candidates.dedup();
                if candidates.len() == 1 {
                    return Some(candidates[0][sub.len()..].to_string());
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_only_after_slash() {
        let mut c = SmartCompleter::new(PathBuf::from("."));
        assert!(c.complete("hello", 5).is_empty());
        assert!(c.complete("", 0).is_empty());
        assert!(!c.complete("/he", 3).is_empty());
    }

    #[test]
    fn empty_slash_lists_all_commands() {
        let mut c = SmartCompleter::new(PathBuf::from("."));
        let completions = c.complete("/", 1);
        assert!(completions.len() >= 40, "registry should offer all commands");
        assert!(completions.iter().any(|c| c.value == "/help"));
        assert!(completions.iter().any(|c| c.value == "/quit"));
    }

    #[test]
    fn prefix_match_finds_relevant() {
        let mut c = SmartCompleter::new(PathBuf::from("."));
        let completions = c.complete("/neu", 4);
        assert!(!completions.is_empty());
        assert!(completions.iter().all(|c| c.value.starts_with("/neu")));
        assert!(completions.iter().any(|c| c.value == "/neurocode"));
    }

    #[test]
    fn alias_suggestions_included() {
        let mut c = SmartCompleter::new(PathBuf::from("."));
        let completions = c.complete("/q", 2);
        assert!(completions.iter().any(|c| c.value == "/queue"));
        let completions = c.complete("/res", 4);
        assert!(completions.iter().any(|c| c.value == "/reset"));
    }

    #[test]
    fn subcommand_completion_after_space() {
        let mut c = SmartCompleter::new(PathBuf::from("."));
        // /timestamps has [on|off|status].
        let completions = c.complete("/timestamps o", 13);
        let values: Vec<&str> = completions.iter().map(|s| s.value.as_str()).collect();
        assert!(values.contains(&"on"), "got {values:?}");
        assert!(values.contains(&"off"), "got {values:?}");
        assert!(!values.contains(&"status"));
        // Exact subcommand typed — nothing more to offer.
        assert!(c.complete("/timestamps on", 14).is_empty());
        // Past the first argument word — nothing.
        assert!(c.complete("/timestamps on x", 16).is_empty());
    }

    #[test]
    fn subcommand_completion_implemented_only() {
        let mut c = SmartCompleter::new(PathBuf::from("."));
        // /voice is registered but not implemented — no subcommand offers.
        assert!(c.complete("/voice o", 8).is_empty());
    }

    #[test]
    fn hinter_slash_name_remainder() {
        let h = SmartHinter;
        // /hel uniquely completes to /help.
        let mut history = reedline::FileBackedHistory::with_file(10, std::env::temp_dir().join("joey_hinter_test_hist")).unwrap();
        let _ = &mut history;
        assert_eq!(h.slash_hint("/hel", 4).as_deref(), Some("p"));
        assert_eq!(h.slash_hint("/q", 2), None); // ambiguous (queue/quit)
        assert_eq!(h.slash_hint("/timestamps of", 14).as_deref(), Some("f"));
        assert_eq!(h.slash_hint("/timestamps s", 13).as_deref(), Some("tatus"));
        assert_eq!(h.slash_hint("/timestamps o", 13), None); // on/off ambiguous
    }

    #[test]
    fn hinter_no_hint_for_plain_text_directly() {
        let h = SmartHinter;
        assert_eq!(h.slash_hint("hello", 5), None);
    }
}

#[cfg(test)]
mod multibyte_tests {
    use super::*;

    /// Regression: `&line[..pos]` panicked when `pos` fell inside a multibyte
    /// char (e.g. cursor reported mid-codepoint). Completion must degrade to
    /// "no completions", never panic.
    #[test]
    fn complete_no_panic_on_non_boundary_pos() {
        let mut c = SmartCompleter::new(PathBuf::from("."));
        // "é" occupies bytes 1..3; pos=2 splits it.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            c.complete("é", 2);
        }))
        .expect("complete() must not panic on mid-char pos");
        // Hinter path too ("/é", pos=2 splits the é).
        let mut history = reedline::FileBackedHistory::with_file(
            10,
            std::env::temp_dir().join(format!("joey_mb_hist_{}", std::process::id())),
        )
        .unwrap();
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            SmartHinter.handle("/é", 2, &mut history, false, ".");
        }))
        .expect("handle() must not panic on mid-char pos");
    }
}
