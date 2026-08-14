//! Slash-command completion for the CLI line editor (feature: slash popup).
//!
//! `SlashCompleter` feeds the reedline `DescriptionMenu` ("slash_menu") with
//! the full slash-command registry — names, aliases, descriptions, arg hints,
//! and implemented status. The menu is bound to Tab in the REPL; when the
//! input starts with `/` it offers only the relevant commands (prefix match).

use reedline::{Completer, Span, Suggestion};

use crate::slash;

/// Completer over the slash-command registry.
pub struct SlashCompleter;

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

impl Completer for SlashCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        // Only complete when the line starts with '/' and the cursor is inside
        // the first word (the command token).
        if !line.starts_with('/') || pos < 1 {
            return Vec::new();
        }
        let head = &line[..pos.min(line.len())];
        // Past the first whitespace there is nothing to complete (arguments
        // are free-form).
        if head.trim().contains(' ') {
            return Vec::new();
        }
        let typed = head.strip_prefix('/').unwrap_or("");
        let span = Span::new(0, head.len());

        // Prefix-match names AND aliases against the typed fragment; an empty
        // fragment lists everything (Tab right after `/`).
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
            // Also offer each matching alias (so /q finds /queue via q).
            for alias in def.aliases {
                if !typed.is_empty() && alias.starts_with(typed) {
                    let extra = format!("alias of /{}", def.name);
                    out.push(suggest(def, span, format!("/{}", alias), extra));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completes_only_after_slash() {
        let mut c = SlashCompleter;
        assert!(c.complete("hello", 5).is_empty());
        assert!(c.complete("", 0).is_empty());
        assert!(!c.complete("/he", 3).is_empty());
    }

    #[test]
    fn empty_slash_lists_all_commands() {
        let mut c = SlashCompleter;
        let completions = c.complete("/", 1);
        assert!(completions.len() >= 40, "registry should offer all commands");
        assert!(completions.iter().any(|c| c.value == "/help"));
        assert!(completions.iter().any(|c| c.value == "/quit"));
    }

    #[test]
    fn prefix_match_finds_relevant() {
        let mut c = SlashCompleter;
        let completions = c.complete("/neu", 4);
        assert!(!completions.is_empty());
        assert!(completions.iter().all(|c| c.value.starts_with("/neu")));
        assert!(completions.iter().any(|c| c.value == "/neurocode"));
    }

    #[test]
    fn alias_suggestions_included() {
        let mut c = SlashCompleter;
        // "q" is the alias of /queue.
        let completions = c.complete("/q", 2);
        assert!(completions.iter().any(|c| c.value == "/queue"));
        // "reset" is the alias of /new.
        let completions = c.complete("/res", 4);
        assert!(completions.iter().any(|c| c.value == "/reset"));
    }

    #[test]
    fn completion_carries_description_and_hint() {
        let mut c = SlashCompleter;
        let completions = c.complete("/hel", 4);
        let help = completions
            .iter()
            .find(|c| c.value == "/help")
            .expect("/help should be suggested");
        assert!(help.description.as_deref().unwrap().contains("commands"));
        assert!(help.extra.as_ref().unwrap()[0].contains("available"));
    }

    #[test]
    fn no_completion_past_first_word() {
        let mut c = SlashCompleter;
        // Cursor inside the argument tail — nothing to complete.
        assert!(c.complete("/model gp", 10).is_empty());
    }
}
