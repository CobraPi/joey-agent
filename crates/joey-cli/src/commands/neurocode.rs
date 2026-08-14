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

/// Entry point for the `/neurocode` slash command.
///
/// `args` is the raw argument string after `/neurocode`. Returns the
/// plain-text output to display (Constitution II).
pub fn neurocode_slash(args: &str) -> String {
    let parts: Vec<&str> = args.split_whitespace().collect();
    let sub = parts.first().copied().unwrap_or("status");
    let engine = build_engine();

    match sub {
        "status" | "" => engine.status_text(),

        "tier" => {
            let action = parts.get(1).copied().unwrap_or("show");
            let tier = parts.get(2).copied();
            // Map the two-arg form `/neurocode tier <tier>` to "set".
            if action == "economical" || action == "frontier" || action == "auto" {
                engine.tier_text("set", Some(action))
            } else {
                engine.tier_text(action, tier)
            }
        }

        "index" => {
            let force = parts.iter().any(|p| *p == "--force" || *p == "-f");
            engine.index_text(force)
        }

        "query" => {
            let query_type = parts.get(1).copied().unwrap_or("symbol");
            let symbol = parts.get(2).copied().unwrap_or("");
            engine.query_text(query_type, symbol)
        }

        "ingest" => {
            let category = match parts.get(1).copied() {
                Some(c) => c,
                None => {
                    return "Usage: /neurocode ingest <category> <path> [--version <v>] \
                            [--provenance <p>]"
                        .to_string();
                }
            };
            let path = match parts.get(2).copied() {
                Some(p) => p,
                None => {
                    return "Usage: /neurocode ingest <category> <path> [--version <v>] \
                            [--provenance <p>]"
                        .to_string();
                }
            };
            // Parse optional --version and --provenance flags.
            let (version, provenance) = parse_kv_flags(&parts[3..]);
            engine.ingest_text(category, path, version.as_deref(), &provenance)
        }

        "patterns" => engine.patterns_text(),

        "anti-patterns" | "antipatterns" => engine.anti_patterns_text(),

        "domain" => {
            let action = parts.get(1).copied().unwrap_or("list");
            match action {
                "list" | "" => engine.domain_list_text(),
                "remove" | "rm" | "delete" => {
                    let id = match parts.get(2).and_then(|s| s.parse::<u64>().ok()) {
                        Some(id) => id,
                        None => {
                            return "Usage: /neurocode domain remove <id>".to_string();
                        }
                    };
                    engine.domain_remove_text(id)
                }
                _ => format!(
                    "Unknown domain action '{}'. Use: list | remove <id>",
                    action
                ),
            }
        }

        "help" | "-h" | "--help" => help_text(),

        _ => format!(
            "Unknown subcommand '{}'. Run /neurocode --help for usage.",
            sub
        ),
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
        // With default config (disabled), status should still return a
        // well-formed status string without panicking.
        let out = neurocode_slash("status");
        assert!(out.starts_with("NeuroCode:"));
    }

    #[test]
    fn no_args_defaults_to_status() {
        let out = neurocode_slash("");
        assert!(out.starts_with("NeuroCode:"));
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
