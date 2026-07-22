//! Default SOUL.md template seeded into JOEY_HOME on first run
//! (port of upstream `hermes_cli/default_soul.py`).

/// The default persona. Branding note: the port replaces upstream's
/// "created by Nous Research" attribution with "based on Hermes Agent by
/// Nous Research"; the rest of the text is verbatim.
pub const DEFAULT_SOUL_MD: &str = "You are Joey Agent, an intelligent AI assistant based on Hermes Agent by Nous Research. \
You are helpful, knowledgeable, and direct. You assist users with a wide \
range of tasks including answering questions, writing and editing code, \
analyzing information, creative work, and executing actions via your tools. \
You communicate clearly, admit uncertainty when appropriate, and prioritize \
being genuinely useful over being verbose unless otherwise directed below. \
Be targeted and efficient in your exploration and investigations.";

/// Legacy SOUL.md boilerplate older Hermes installers seeded — pure comment
/// scaffolding with zero user intent, safe to upgrade in place (a home
/// migrated from a hermes install may carry one). Kept verbatim: these are
/// literal historical file contents, not branding surface.
const LEGACY_TEMPLATE_SOULS: [&str; 2] = [
    "# Hermes Agent Persona\n\
     \n\
     <!--\n\
     This file defines the agent's personality and tone.\n\
     The agent will embody whatever you write here.\n\
     Edit this to customize how Hermes communicates with you.\n\
     \n\
     Examples:\n\
     \x20 - \"You are a warm, playful assistant who uses kaomoji occasionally.\"\n\
     \x20 - \"You are a concise technical expert. No fluff, just facts.\"\n\
     \x20 - \"You speak like a friendly coworker who happens to know everything.\"\n\
     \n\
     This file is loaded fresh each message -- no restart needed.\n\
     Delete the contents (or this file) to use the default personality.\n\
     -->",
    "# Hermes Agent Persona\n\
     \n\
     <!--\n\
     This file defines the agent's personality and tone.\n\
     The agent will embody whatever you write here.\n\
     Edit this to customize how Hermes communicates with you.\n\
     \n\
     This file is loaded fresh each message -- no restart needed.\n\
     Delete the contents (or this file) to use the default personality.\n\
     -->",
];

/// Normalize SOUL.md content for legacy-template comparison: unify line
/// endings, strip a leading UTF-8 BOM, trim surrounding whitespace.
fn normalize_soul(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim_start_matches('\u{feff}')
        .trim()
        .to_string()
}

/// True if `text` is an old empty-template SOUL.md (no user persona).
/// Any deviation — the user typed even one character outside the comment —
/// makes this return false.
pub fn is_legacy_template_soul(text: &str) -> bool {
    let normalized = normalize_soul(text);
    LEGACY_TEMPLATE_SOULS
        .iter()
        .any(|t| normalized == normalize_soul(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_detection() {
        assert!(is_legacy_template_soul(LEGACY_TEMPLATE_SOULS[0]));
        assert!(is_legacy_template_soul(&format!("\u{feff}{}\r\n", LEGACY_TEMPLATE_SOULS[1])));
        assert!(!is_legacy_template_soul("You are a pirate."));
        assert!(!is_legacy_template_soul(DEFAULT_SOUL_MD));
        assert!(!is_legacy_template_soul(&format!("{}\nBe a pirate.", LEGACY_TEMPLATE_SOULS[0])));
    }

    #[test]
    fn default_soul_branding() {
        assert!(DEFAULT_SOUL_MD.starts_with("You are Joey Agent"));
        assert!(DEFAULT_SOUL_MD.contains("based on Hermes Agent by Nous Research"));
        assert!(DEFAULT_SOUL_MD.ends_with("exploration and investigations."));
    }
}
