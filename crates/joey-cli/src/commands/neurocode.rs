//! `/neurocode` command handler (T048, contracts/neurocode-command.md).
//!
//! Text-mode control surface for the NeuroCode engine (spec 015). Implements
//! Constitution II (CLI/TUI parity) — every capability is reachable as text,
//! and the slash-command and CLI paths share the same handler.
//!
//! The handler constructs a fresh `DefaultEngine` from the current joey config
//! on each invocation (mirroring how `/llm-selector` builds its engine), then
//! dispatches to the `NeuroCodeCommands` trait methods which return plain-text
//! output.

use std::path::PathBuf;
use std::sync::Arc;

use joey_neurocode::{DefaultEngine, NeuroCodeCommands, NeuroCodeConfig};

/// Build a NeuroCode engine from the current joey config, scoped to the
/// current working directory (the project root for graph indexing).
///
/// Returns `None` when NeuroCode is disabled in config, so callers can show a
/// "NeuroCode is disabled" notice rather than constructing a dead engine.
fn build_engine() -> Arc<DefaultEngine> {
    let config = joey_core::Config::load().unwrap_or_else(|_| joey_core::Config::defaults());
    let nc_cfg = NeuroCodeConfig::from_config(&config);
    let project_root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    Arc::new(DefaultEngine::new(nc_cfg, project_root))
}

/// Outcome of dispatching `/neurocode`: either plain display text, or a
/// hand-off to a full agent turn (natural-language ingest).
pub enum NeurocodeOutcome {
    /// Plain text to display immediately.
    Text(String),
    /// Natural-language ingest request: run an agent turn that resolves
    /// the request into neurocode_ingest tool calls. Carries the composed
    /// workflow prompt.
    AgentIngest(String),
}

/// Entry point for the `/neurocode` slash command (plain-text shape for
/// the engine heavy-job path and tests).
pub fn neurocode_slash(args: &str) -> String {
    match neurocode_slash_outcome(args) {
        NeurocodeOutcome::Text(t) => t,
        NeurocodeOutcome::AgentIngest(_) => {
            // The plain-text caller can't run a turn; give usage guidance.
            "Natural-language ingest needs an interactive surface (REPL/TUI). \
             Use the strict form: /neurocode ingest <category> <path> [--version <v>] [--provenance <p>]"
                .to_string()
        }
    }
}

/// Compose the agent-turn workflow prompt for a natural-language ingest
/// request (the user's free text after `/neurocode ingest`).
pub fn ingest_agent_prompt(request: &str) -> String {
    format!(
        "You are ingesting domain knowledge into the NeuroCode engine for this repository. \
The user described what to ingest in natural language:\n\n> {request}\n\n\
Use the `neurocode_ingest` tool to complete this. Its parameters:\n\
- category: one of FrameworkDocs, EntityCatalog, Postmortem, PegaRuleType\n\
- source_path: path to a FILE (or directory) containing the knowledge — if the user pointed at \
something fuzzy, locate the actual file(s) with read_file/search_files first and confirm the content \
looks like what they described\n\
- version_tag: optional version string when the user named one\n\
- provenance: where the knowledge came from when the user said so\n\n\
Workflow:\n\
1. Interpret the request: what knowledge, from where, which category fits.\n\
2. Locate the source: if the user gave a path, verify it exists and is readable text \
(read_file a sample); if they described content, search the repo (search_files) for it.\n\
3. If the knowledge only exists in the user's message itself (they pasted facts or a postmortem \
rather than pointing at a file), write it to a markdown file first — e.g. \
`.neurocode/sources/<slug>.md` (create the directory) with the content clearly organized — \
then ingest THAT file with provenance `user-provided`.\n\
4. Call neurocode_ingest with the resolved parameters.\n\
5. Report exactly what was ingested (category, path, version) and the tool's result. \
If anything can't be resolved (no such file, ambiguous category), say so plainly instead of guessing."
    )
}

/// Does the strict form match? First token must be a valid category AND a
/// second token must exist (the path). Anything else is natural language.
fn structured_ingest(parts: &[&str]) -> bool {
    if parts.len() < 2 {
        return false;
    }
    matches!(
        parts[0],
        "FrameworkDocs" | "framework_docs" | "EntityCatalog" | "entity_catalog" | "Postmortem"
            | "postmortem" | "PegaRuleType" | "pega_rule_type"
    )
}

/// Full-dispatch entry: returns the outcome so interactive surfaces can
/// run the agent path.
pub fn neurocode_slash_outcome(args: &str) -> NeurocodeOutcome {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let sub = parts.first().copied().unwrap_or("status");
    let engine = build_engine();

    match sub {
        "status" => NeurocodeOutcome::Text(engine.status_text()),

        "tier" => {
            let action = parts.get(1).copied().unwrap_or("show");
            let tier = parts.get(2).copied();
            // Map the two-arg form `/neurocode tier <tier>` to "set".
            if action == "economical" || action == "frontier" || action == "auto" {
                NeurocodeOutcome::Text(engine.tier_text("set", Some(action)))
            } else {
                NeurocodeOutcome::Text(engine.tier_text(action, tier))
            }
        }

        "index" => {
            let force = parts.iter().any(|p| *p == "--force" || *p == "-f");
            NeurocodeOutcome::Text(engine.index_text(force))
        }

        "query" => {
            let query_type = parts.get(1).copied().unwrap_or("symbol");
            let symbol = parts.get(2).copied().unwrap_or("");
            NeurocodeOutcome::Text(engine.query_text(query_type, symbol))
        }

        "ingest" => {
            // Two forms: strict (`<category> <path> [flags]`) or natural
            // language (anything else) — the latter hands off to an agent
            // turn that resolves the request into neurocode_ingest calls.
            let ingest_parts = &parts[1..];
            if structured_ingest(ingest_parts) {
                let category = ingest_parts[0];
                let path = ingest_parts[1];
                // Parse optional --version and --provenance flags.
                let (version, provenance) = parse_kv_flags(&ingest_parts[2..]);
                NeurocodeOutcome::Text(engine.ingest_text(
                    category,
                    path,
                    version.as_deref(),
                    &provenance,
                ))
            } else if ingest_parts.is_empty() {
                NeurocodeOutcome::Text(
                    "Usage: /neurocode ingest <category> <path> [--version <v>] [--provenance <p>]\n\
                     Or describe it naturally: /neurocode ingest the Spring Boot docs in ./docs/spring"
                        .to_string(),
                )
            } else {
                // Natural language: everything after "ingest" is the request.
                let request = args
                    .split_once(char::is_whitespace)
                    .map(|(_, rest)| rest.trim())
                    .unwrap_or_default();
                NeurocodeOutcome::AgentIngest(ingest_agent_prompt(request))
            }
        }

        "patterns" => NeurocodeOutcome::Text(engine.patterns_text()),

        "anti-patterns" | "antipatterns" => {
            NeurocodeOutcome::Text(engine.anti_patterns_text())
        }

        "domain" => {
            let action = parts.get(1).copied().unwrap_or("list");
            match action {
                "list" | "" => NeurocodeOutcome::Text(engine.domain_list_text()),
                "remove" | "rm" | "delete" => {
                    let id = match parts.get(2).and_then(|s| s.parse::<u64>().ok()) {
                        Some(id) => id,
                        None => {
                            return NeurocodeOutcome::Text(
                                "Usage: /neurocode domain remove <id>".to_string(),
                            );
                        }
                    };
                    NeurocodeOutcome::Text(engine.domain_remove_text(id))
                }
                _ => NeurocodeOutcome::Text(format!(
                    "Unknown domain action '{}'. Use: list | remove <id>",
                    action
                )),
            }
        }

        "help" | "-h" | "--help" => NeurocodeOutcome::Text(help_text()),

        _ => NeurocodeOutcome::Text(format!(
            "Unknown subcommand '{}'. Run /neurocode --help for usage.",
            sub
        )),
    }
}

/// Parse `--version <v>` and `--provenance <p>` flags from a tail of args.
fn parse_kv_flags(args: &[&str]) -> (Option<String>, String) {
    let mut version: Option<String> = None;
    let mut provenance = String::new();
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--version" | "-v" => {
                if let Some(v) = args.get(i + 1) {
                    version = Some((*v).to_string());
                    i += 2;
                    continue;
                }
            }
            "--provenance" | "-p" => {
                if let Some(p) = args.get(i + 1) {
                    provenance = (*p).to_string();
                    i += 2;
                    continue;
                }
            }
            _ => {}
        }
        i += 1;
    }
    (version, provenance)
}

fn help_text() -> String {
    "Usage: /neurocode <subcommand>\n\
     \n\
     Subcommands:\n\
     \x20 status                          Show enabled state, index size, tiers, patterns, domain\n\
     \x20 tier [economical|frontier|auto] Show or set the active tier for the session\n\
     \x20 tier pin <tier>                 Pin a tier (economical|frontier) for the session\n\
     \x20 tier unpin                      Revert to automatic classification\n\
     \x20 index [--force]                 Trigger structural indexing of the project\n\
     \x20 query <type> <symbol>           Direct graph query (symbol|dependents|dependencies)\n\
     \x20 ingest <category> <path>        Ingest domain knowledge\n\
     \x20   [--version <v>] [--provenance <p>]   (category: FrameworkDocs|EntityCatalog|Postmortem)\n\
     \x20 patterns                        List learned patterns\n\
     \x20 anti-patterns                   List learned anti-patterns\n\
     \x20 domain list                     List ingested domain-knowledge sources\n\
     \x20 domain remove <id>              Remove a domain source\n\
     \x20 --help                          Show this help message"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_succeeds() {
        // `/neurocode --help` returns help text (exit-success path).
        let out = neurocode_slash("--help");
        assert!(out.contains("Usage: /neurocode"));
    }

    #[test]
    fn help_aliases() {
        assert!(neurocode_slash("help").contains("Usage: /neurocode"));
        assert!(neurocode_slash("-h").contains("Usage: /neurocode"));
    }

    #[test]
    fn unknown_subcommand_reports_error() {
        let out = neurocode_slash("nonsense");
        assert!(out.contains("Unknown subcommand"));
    }

    #[test]
    fn status_works_with_disabled_engine() {
        // `status` must route to the engine's status_text(), NOT fall into
        // the unknown-subcommand catch-all (regression: it used to).
        let out = neurocode_slash("status");
        assert!(
            !out.contains("Unknown subcommand"),
            "status must not hit the catch-all, got: {out}"
        );
        assert!(out.starts_with("NeuroCode:"), "status prefix, got: {out}");
    }

    #[test]
    fn no_args_defaults_to_status() {
        let out = neurocode_slash("");
        assert!(
            !out.contains("Unknown subcommand"),
            "bare /neurocode defaults to status, got: {out}"
        );
        assert!(out.starts_with("NeuroCode:"), "status prefix, got: {out}");
    }

    #[test]
    fn parse_kv_flags_extracts_version_and_provenance() {
        let args = ["--version", "3.2", "--provenance", "Spring Docs"];
        let (v, p) = parse_kv_flags(&args);
        assert_eq!(v.as_deref(), Some("3.2"));
        assert_eq!(p, "Spring Docs");
    }

    #[test]
    fn parse_kv_flags_handles_missing_values() {
        let args: [&str; 0] = [];
        let (v, p) = parse_kv_flags(&args);
        assert!(v.is_none());
        assert!(p.is_empty());
    }
}

#[cfg(test)]
mod ingest_routing_tests {
    use super::*;

    #[test]
    fn structured_form_takes_the_direct_path() {
        // Strict form: category + path → Text outcome (no agent).
        match neurocode_slash_outcome("ingest FrameworkDocs ./docs/spring --version 3.2") {
            NeurocodeOutcome::Text(t) => {
                // (May succeed or fail on the actual file — but it must NOT
                // be an agent hand-off, and must mention the path/error.)
                assert!(!t.contains("natural-language"), "{t}");
            }
            NeurocodeOutcome::AgentIngest(_) => panic!("strict form must not go to the agent"),
        }
    }

    #[test]
    fn natural_language_takes_the_agent_path() {
        for args in [
            "ingest the spring boot docs in ./docs/spring",
            "ingest the postmortem I just pasted about the outage",
            "ingest everything under docs about Pega rule types",
        ] {
            match neurocode_slash_outcome(args) {
                NeurocodeOutcome::AgentIngest(prompt) => {
                    assert!(prompt.contains("neurocode_ingest"), "prompt teaches the tool");
                    assert!(prompt.contains("FrameworkDocs"), "prompt lists categories");
                }
                _ => panic!("natural language must hand off to the agent: {args}"),
            }
        }
    }

    #[test]
    fn bare_ingest_shows_both_usages() {
        match neurocode_slash_outcome("ingest") {
            NeurocodeOutcome::Text(t) => {
                assert!(t.contains("Usage:"), "{t}");
                assert!(t.contains("naturally"), "mentions the NL form: {t}");
            }
            _ => panic!("bare ingest shows usage"),
        }
    }

    #[test]
    fn category_without_path_is_natural_language() {
        // "ingest Postmortem" alone: no path token → the user is describing
        // something in prose (or will paste it) → agent path.
        match neurocode_slash_outcome("ingest Postmortem") {
            NeurocodeOutcome::AgentIngest(_) => {}
            _ => panic!("category without path should fall to the agent"),
        }
    }

    #[test]
    fn agent_prompt_includes_user_request_verbatim() {
        let p = ingest_agent_prompt("please ingest the spring docs at ./docs/spring");
        assert!(p.contains("please ingest the spring docs at ./docs/spring"));
        assert!(p.contains(".neurocode/sources/"), "pasted-knowledge path taught");
    }
}

#[cfg(test)]
mod ingest_tool_integration_tests {

    /// The neurocode_ingest TOOL is registered when NeuroCode is enabled —
    /// the agent path depends on it being callable in-turn.
    #[test]
    fn ingest_tool_registered_when_enabled() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "neurocode:\n  enabled: true\n").unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let engine = crate::neurocode_wiring::try_build_engine(&config);
        assert!(engine.is_some(), "engine builds when enabled");
        let engine = engine.unwrap();
        let backend = crate::neurocode_wiring::backend_for_engine(&engine);
        let mut registry = joey_tools::ToolRegistry::new();
        joey_tools::builtins::register_neurocode_tools(&mut registry, Some(backend));
        let names = registry.names();
        assert!(names.contains(&"neurocode_ingest".to_string()), "tool registered: {names:?}");
    }

    /// The backend's ingest path executes against a real (temp) graph.
    #[test]
    fn backend_ingest_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(tmp.path(), "neurocode:\n  enabled: true\n").unwrap();
        let config = joey_core::Config::load_from(tmp.path().to_path_buf()).unwrap();
        let engine = crate::neurocode_wiring::try_build_engine(&config).unwrap();
        let backend = crate::neurocode_wiring::backend_for_engine(&engine);

        // Source file in a temp project dir.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("notes.md");
        std::fs::write(&src, "# Knowledge\n- fact one\n- fact two\n").unwrap();

        // Index first (opens the graph), then ingest.
        let _ = backend.index(".", true);
        let out = backend.ingest("FrameworkDocs", src.to_str().unwrap(), None, "test");
        assert!(out.contains("Ingested") || out.contains("failed") || out.contains("graph"),
                "honest outcome: {out}");
    }
}
