//! Toolset registry (port of `toolsets.py`), rebranded `hermes-*` → `joey-*`.
//!
//! Memberships are ported verbatim — including the names of tools this port
//! has not (yet) implemented. Resolution may therefore yield unregistered
//! names; the tool registry simply filters them at definition time, exactly
//! as upstream's registry does.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::RwLock;

use once_cell::sync::Lazy;

/// A toolset definition.
pub struct Toolset {
    pub description: &'static str,
    pub tools: &'static [&'static str],
    pub includes: &'static [&'static str],
}

/// Shared tool list for CLI and all messaging platform toolsets
/// (upstream `_HERMES_CORE_TOOLS`, membership verbatim).
pub const CORE_TOOLS: &[&str] = &[
    // Web
    "web_search",
    "web_extract",
    // Terminal + process management
    "terminal",
    "process",
    // Desktop GUI terminal pane readers (gated on the GUI upstream)
    "read_terminal",
    "close_terminal",
    // File manipulation
    "read_file",
    "write_file",
    "patch",
    "search_files",
    // Vision + image generation
    "vision_analyze",
    "image_generate",
    // Skills
    "skills_list",
    "skill_view",
    "skill_manage",
    // Browser automation
    "browser_navigate",
    "browser_snapshot",
    "browser_click",
    "browser_type",
    "browser_scroll",
    "browser_back",
    "browser_press",
    "browser_get_images",
    "browser_vision",
    "browser_console",
    "browser_cdp",
    "browser_dialog",
    // Text-to-speech
    "text_to_speech",
    // Planning & memory
    "todo",
    "memory",
    // Session history search
    "session_search",
    // Clarifying questions
    "clarify",
    // Code execution + delegation
    "execute_code",
    "delegate_task",
    // Cronjob management
    "cronjob",
    // Home Assistant smart home control
    "ha_list_entities",
    "ha_get_state",
    "ha_list_services",
    "ha_call_service",
    // Kanban multi-agent coordination
    "kanban_show",
    "kanban_list",
    "kanban_complete",
    "kanban_block",
    "kanban_heartbeat",
    "kanban_comment",
    "kanban_create",
    "kanban_link",
    "kanban_unblock",
    "kanban_attach",
    "kanban_attach_url",
    "kanban_attachments",
    // Computer use
    "computer_use",
];

static TOOLSETS: Lazy<HashMap<&'static str, Toolset>> = Lazy::new(|| {
    let mut m = HashMap::new();
    m.insert(
        "web",
        Toolset {
            description: "Web research and content extraction tools",
            tools: &["web_search", "web_extract"],
            includes: &[],
        },
    );
    m.insert(
        "search",
        Toolset {
            description: "Web search only (no content extraction/scraping)",
            tools: &["web_search"],
            includes: &[],
        },
    );
    m.insert(
        "terminal",
        Toolset {
            description: "Terminal/command execution and process management tools",
            tools: &["terminal", "process"],
            includes: &[],
        },
    );
    m.insert(
        "skills",
        Toolset {
            description: "Access, create, edit, and manage skill documents with specialized instructions and knowledge",
            tools: &["skills_list", "skill_view", "skill_manage"],
            includes: &[],
        },
    );
    m.insert(
        "cronjob",
        Toolset {
            description: "Cronjob management tool - create, list, update, pause, resume, remove, and trigger scheduled tasks",
            tools: &["cronjob"],
            includes: &[],
        },
    );
    m.insert(
        "file",
        Toolset {
            description: "File manipulation tools: read, write, patch (with fuzzy matching), and search (content + files)",
            tools: &["read_file", "write_file", "patch", "search_files"],
            includes: &[],
        },
    );
    m.insert(
        "todo",
        Toolset {
            description: "Task planning and tracking for multi-step work",
            tools: &["todo"],
            includes: &[],
        },
    );
    m.insert(
        "memory",
        Toolset {
            description: "Persistent memory across sessions (personal notes + user profile)",
            tools: &["memory"],
            includes: &[],
        },
    );
    m.insert(
        "session_search",
        Toolset {
            description: "Search and recall past conversations with summarization",
            tools: &["session_search"],
            includes: &[],
        },
    );
    m.insert(
        "clarify",
        Toolset {
            description: "Ask the user clarifying questions (multiple-choice or open-ended)",
            tools: &["clarify"],
            includes: &[],
        },
    );
    m.insert(
        "delegation",
        Toolset {
            description: "Spawn subagents with isolated context for complex subtasks",
            tools: &["delegate_task"],
            includes: &[],
        },
    );
    // Scenario-specific toolsets
    m.insert(
        "debugging",
        Toolset {
            description: "Debugging and troubleshooting toolkit",
            tools: &["terminal", "process"],
            includes: &["web", "file"],
        },
    );
    m.insert(
        "safe",
        Toolset {
            description: "Safe toolkit without terminal access",
            tools: &[],
            includes: &["web", "vision", "image_gen"],
        },
    );
    m.insert(
        "vision",
        Toolset {
            description: "Image analysis and vision tools",
            tools: &["vision_analyze"],
            includes: &[],
        },
    );
    m.insert(
        "image_gen",
        Toolset {
            description: "Creative generation tools (images)",
            tools: &["image_generate"],
            includes: &[],
        },
    );
    m.insert(
        "coding",
        Toolset {
            description: "Coding-focused toolset: files, terminal, search, web docs, skills, todo, delegate, vision, browser",
            tools: &[
                "web_search",
                "web_extract",
                "terminal",
                "process",
                "read_terminal",
                "close_terminal",
                "read_file",
                "write_file",
                "patch",
                "search_files",
                "vision_analyze",
                "skills_list",
                "skill_view",
                "skill_manage",
                "browser_navigate",
                "browser_snapshot",
                "browser_click",
                "browser_type",
                "browser_scroll",
                "browser_back",
                "browser_press",
                "browser_get_images",
                "browser_vision",
                "browser_console",
                "browser_cdp",
                "browser_dialog",
                "todo",
                "memory",
                "session_search",
                "clarify",
                "execute_code",
                "delegate_task",
            ],
            includes: &[],
        },
    );
    m.insert(
        "joey-cli",
        Toolset {
            description: "Full interactive CLI toolset - all default tools plus cronjob management",
            tools: CORE_TOOLS,
            includes: &[],
        },
    );
    m.insert(
        "joey-cron",
        Toolset {
            description: "Default cron toolset - same core tools as joey-cli; gated by `joey tools`",
            tools: CORE_TOOLS,
            includes: &[],
        },
    );
    m
});

/// Platform registry for auto-generated `joey-<platform>` toolsets
/// (toolsets.py:738-754). Empty by default; higher crates register platforms.
static PLATFORM_REGISTRY: Lazy<RwLock<HashSet<String>>> =
    Lazy::new(|| RwLock::new(HashSet::new()));

/// Register a gateway platform so `joey-<name>` resolves to the core tools.
pub fn register_platform(name: &str) {
    PLATFORM_REGISTRY.write().unwrap().insert(name.to_string());
}

fn platform_registered(name: &str) -> bool {
    PLATFORM_REGISTRY.read().unwrap().contains(name)
}

/// Resolve a toolset name (or `all`/`*`) to a flat, sorted tool-name list.
pub fn resolve(name: &str) -> Vec<String> {
    if name == "all" || name == "*" {
        let mut all = BTreeSet::new();
        for ts in TOOLSETS.keys() {
            for t in resolve(ts) {
                all.insert(t);
            }
        }
        return all.into_iter().collect();
    }
    let mut visited = BTreeSet::new();
    let mut out = BTreeSet::new();
    resolve_into(name, &mut visited, &mut out);
    out.into_iter().collect()
}

fn resolve_into(name: &str, visited: &mut BTreeSet<String>, out: &mut BTreeSet<String>) {
    if !visited.insert(name.to_string()) {
        return;
    }
    let Some(ts) = TOOLSETS.get(name) else {
        // Auto-generate a toolset for registered plugin platforms
        // (`joey-<name>` → core tools).
        if let Some(platform_name) = name.strip_prefix("joey-") {
            if platform_registered(platform_name) {
                for t in CORE_TOOLS {
                    out.insert((*t).to_string());
                }
            }
        }
        return;
    };
    for t in ts.tools {
        out.insert((*t).to_string());
    }
    for inc in ts.includes {
        resolve_into(inc, visited, out);
    }
}

/// Resolve multiple toolsets and merge their tools.
pub fn resolve_multiple(names: &[String]) -> Vec<String> {
    let mut all = BTreeSet::new();
    for n in names {
        for t in resolve(n) {
            all.insert(t);
        }
    }
    all.into_iter().collect()
}

/// All toolset names, sorted.
pub fn names() -> Vec<&'static str> {
    let mut n: Vec<_> = TOOLSETS.keys().copied().collect();
    n.sort_unstable();
    n
}

/// A toolset's description, if it exists.
pub fn description(name: &str) -> Option<&'static str> {
    TOOLSETS.get(name).map(|t| t.description)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_leaf() {
        assert_eq!(resolve("file"), vec!["patch", "read_file", "search_files", "write_file"]);
        assert_eq!(resolve("search"), vec!["web_search"]);
        assert_eq!(resolve("delegation"), vec!["delegate_task"]);
        assert_eq!(resolve("session_search"), vec!["session_search"]);
        assert_eq!(resolve("clarify"), vec!["clarify"]);
        assert_eq!(resolve("cronjob"), vec!["cronjob"]);
    }

    #[test]
    fn debugging_resolves_includes() {
        let tools = resolve("debugging");
        for t in ["terminal", "process", "web_search", "web_extract", "read_file", "write_file", "patch", "search_files"] {
            assert!(tools.contains(&t.to_string()), "debugging missing {}", t);
        }
    }

    #[test]
    fn safe_is_include_only() {
        let tools = resolve("safe");
        assert!(tools.contains(&"web_search".to_string()));
        assert!(tools.contains(&"vision_analyze".to_string()));
        assert!(tools.contains(&"image_generate".to_string()));
        assert!(!tools.contains(&"terminal".to_string()));
    }

    #[test]
    fn coding_membership_verbatim() {
        let tools = resolve("coding");
        assert_eq!(tools.len(), 32);
        for t in ["execute_code", "browser_cdp", "skill_manage", "clarify"] {
            assert!(tools.contains(&t.to_string()));
        }
        assert!(!tools.contains(&"cronjob".to_string()));
    }

    #[test]
    fn joey_cli_has_core() {
        let tools = resolve("joey-cli");
        assert_eq!(tools.len(), CORE_TOOLS.len());
        assert!(tools.contains(&"terminal".to_string()));
        assert!(tools.contains(&"cronjob".to_string()));
        assert!(tools.contains(&"kanban_show".to_string()));
        assert_eq!(
            description("joey-cli"),
            Some("Full interactive CLI toolset - all default tools plus cronjob management")
        );
    }

    #[test]
    fn file_description_verbatim() {
        assert_eq!(
            description("file"),
            Some("File manipulation tools: read, write, patch (with fuzzy matching), and search (content + files)")
        );
    }

    #[test]
    fn platform_mechanism() {
        assert!(resolve("joey-testplat").is_empty());
        register_platform("testplat");
        let tools = resolve("joey-testplat");
        assert_eq!(tools.len(), CORE_TOOLS.len());
    }

    #[test]
    fn all_is_union() {
        let all = resolve("all");
        assert!(all.contains(&"memory".to_string()));
        assert!(all.contains(&"web_search".to_string()));
        assert!(all.contains(&"vision_analyze".to_string()));
    }
}
