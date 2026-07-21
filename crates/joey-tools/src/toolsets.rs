//! Toolset registry (port of `toolsets.py`), rebranded `hermes-*` → `joey-*`.
//!
//! A toolset is a named bundle of tools; toolsets may include other toolsets.
//! `resolve` expands includes recursively (with cycle detection) to a flat,
//! sorted tool-name list.

use std::collections::{BTreeSet, HashMap};

use once_cell::sync::Lazy;

/// A toolset definition.
pub struct Toolset {
    pub description: &'static str,
    pub tools: &'static [&'static str],
    pub includes: &'static [&'static str],
}

/// The core tool list shared by the CLI and every platform bundle
/// (upstream `_HERMES_CORE_TOOLS`, trimmed to the tools this port implements).
pub const CORE_TOOLS: &[&str] = &[
    "web_search",
    "web_extract",
    "terminal",
    "process",
    "read_file",
    "write_file",
    "patch",
    "search_files",
    "todo",
    "memory",
    "session_search",
    "clarify",
    "skills_list",
    "skill_view",
    "delegate_task",
    "cronjob",
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
        "file",
        Toolset {
            description: "File manipulation: read, write, patch, search",
            tools: &["read_file", "write_file", "patch", "search_files"],
            includes: &[],
        },
    );
    m.insert(
        "terminal",
        Toolset {
            description: "Terminal/command execution and process management",
            tools: &["terminal", "process"],
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
            description: "Persistent memory across sessions",
            tools: &["memory"],
            includes: &[],
        },
    );
    m.insert(
        "skills",
        Toolset {
            description: "Access and manage skill documents",
            tools: &["skills_list", "skill_view"],
            includes: &[],
        },
    );
    m.insert(
        "coding",
        Toolset {
            description: "Coding-focused toolset: files, terminal, search, web, todo, memory",
            tools: &[
                "web_search", "web_extract", "terminal", "process", "read_file", "write_file",
                "patch", "search_files", "todo", "memory", "session_search", "clarify",
                "skills_list", "skill_view", "delegate_task",
            ],
            includes: &[],
        },
    );
    // Rebranded platform bundle: joey-cli (was hermes-cli).
    m.insert(
        "joey-cli",
        Toolset {
            description: "Full interactive CLI toolset",
            tools: CORE_TOOLS,
            includes: &[],
        },
    );
    m.insert(
        "joey-cron",
        Toolset {
            description: "Default cron toolset — same core tools as joey-cli",
            tools: CORE_TOOLS,
            includes: &[],
        },
    );
    m
});

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
    }

    #[test]
    fn joey_cli_has_core() {
        let tools = resolve("joey-cli");
        assert!(tools.contains(&"terminal".to_string()));
        assert!(tools.contains(&"read_file".to_string()));
    }

    #[test]
    fn all_is_union() {
        let all = resolve("all");
        assert!(all.contains(&"memory".to_string()));
        assert!(all.contains(&"web_search".to_string()));
    }
}
