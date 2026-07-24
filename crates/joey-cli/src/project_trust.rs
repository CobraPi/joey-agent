//! Project trust model (port of pi's `packages/coding-agent/src/core/project-trust.ts`).
//!
//! Security model that prompts the user before loading project-local resources
//! from untrusted directories. Prevents malicious `.joey/` settings, skills,
//! or hooks from auto-executing in untrusted projects.
//!
//! Trust state is persisted in `~/.joey/trust.json` — a map of project paths
//! to trust decisions. Once trusted, a project won't prompt again.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The trust store, persisted at `~/.joey/trust.json`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TrustStore {
    /// path → trust entry
    #[serde(default)]
    pub projects: HashMap<String, TrustEntry>,
}

/// A trust decision for a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustEntry {
    /// When the decision was made (Unix timestamp).
    pub decided_at: i64,
    /// The trust level.
    pub level: TrustLevel,
}

/// Level of trust granted to a project directory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TrustLevel {
    /// Fully trusted — all project resources are loaded.
    Trusted,
    /// Session-only trust (re-prompts next session).
    SessionOnly,
    /// Not trusted — project resources are ignored.
    Untrusted,
}

impl TrustLevel {
    pub fn is_trusted(&self) -> bool {
        matches!(self, TrustLevel::Trusted | TrustLevel::SessionOnly)
    }
}

/// What kind of trust-requiring resources exist in this project.
#[derive(Debug, Clone, Default)]
pub struct ProjectResources {
    pub has_joey_dir: bool,
    pub has_skills: bool,
    pub has_hooks: bool,
    pub has_mcp_config: bool,
    pub has_soul_md: bool,
}

impl ProjectResources {
    /// Whether any trust-requiring resources are present.
    pub fn requires_trust_prompt(&self) -> bool {
        self.has_joey_dir
            || self.has_skills
            || self.has_hooks
            || self.has_mcp_config
            || self.has_soul_md
    }

    /// Human-readable summary of found resources.
    pub fn summary(&self) -> Vec<String> {
        let mut items = Vec::new();
        if self.has_joey_dir {
            items.push(".joey/ directory".to_string());
        }
        if self.has_skills {
            items.push("project skills".to_string());
        }
        if self.has_hooks {
            items.push("hooks config".to_string());
        }
        if self.has_mcp_config {
            items.push("MCP server config".to_string());
        }
        if self.has_soul_md {
            items.push("SOUL.md personality".to_string());
        }
        items
    }
}

impl TrustStore {
    /// Load from `~/.joey/trust.json`.
    pub fn load() -> Self {
        let path = trust_store_path();
        match std::fs::read_to_string(&path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
            Err(_) => Self::default(),
        }
    }

    /// Save to `~/.joey/trust.json`.
    pub fn save(&self) -> std::io::Result<()> {
        let path = trust_store_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        std::fs::write(&path, json)
    }

    /// Check the trust level for a project path.
    pub fn get(&self, path: &str) -> Option<&TrustEntry> {
        self.projects.get(path)
    }

    /// Set the trust level for a project path.
    pub fn set(&mut self, path: &str, level: TrustLevel) {
        self.projects.insert(
            path.to_string(),
            TrustEntry {
                decided_at: chrono::Utc::now().timestamp(),
                level,
            },
        );
    }

    /// Remove the trust entry for a project path.
    pub fn remove(&mut self, path: &str) {
        self.projects.remove(path);
    }

    /// Check whether a project is trusted (trusted or session-only).
    pub fn is_trusted(&self, path: &str) -> bool {
        self.get(path)
            .map(|e| e.level.is_trusted())
            .unwrap_or(false)
    }
}

/// Scan a project directory for trust-requiring resources.
pub fn scan_project(cwd: &Path) -> ProjectResources {
    ProjectResources {
        has_joey_dir: cwd.join(".joey").is_dir(),
        has_skills: cwd.join(".joey").join("skills").exists()
            || cwd.join(".agents").join("skills").exists()
            || cwd.join(".pi").join("skills").exists(),
        has_hooks: has_hooks_config(cwd),
        has_mcp_config: cwd.join(".joey").join("mcp.json").exists()
            || cwd.join(".mcp.json").exists(),
        has_soul_md: cwd.join("SOUL.md").is_file(),
    }
}

/// Check if the project has hooks defined in a local config.
fn has_hooks_config(cwd: &Path) -> bool {
    // Check for hooks in .joey/config.yaml or a local hooks file
    let local_config = cwd.join(".joey").join("config.yaml");
    if local_config.exists() {
        if let Ok(content) = std::fs::read_to_string(&local_config) {
            if content.contains("hooks:") {
                return true;
            }
        }
    }
    false
}

/// The prompt to show the user when a project requires trust.
pub fn trust_prompt(cwd: &Path, resources: &ProjectResources) -> String {
    let items = resources.summary();
    let items_str = if items.is_empty() {
        "project resources".to_string()
    } else {
        items.join(", ")
    };
    format!(
        "Trust project folder?\n{}\n\nThis allows Joey to load {} from this project.",
        cwd.display(),
        items_str
    )
}

/// Resolve the trust store path.
fn trust_store_path() -> PathBuf {
    joey_core::joey_home().join("trust.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_store_roundtrip() {
        let mut store = TrustStore::default();
        store.set("/tmp/project", TrustLevel::Trusted);
        store.set("/tmp/other", TrustLevel::Untrusted);
        assert!(store.is_trusted("/tmp/project"));
        assert!(!store.is_trusted("/tmp/other"));
        assert!(!store.is_trusted("/tmp/unknown"));

        store.remove("/tmp/project");
        assert!(!store.is_trusted("/tmp/project"));
    }

    #[test]
    fn trust_levels() {
        assert!(TrustLevel::Trusted.is_trusted());
        assert!(TrustLevel::SessionOnly.is_trusted());
        assert!(!TrustLevel::Untrusted.is_trusted());
    }

    #[test]
    fn resources_summary() {
        let r = ProjectResources {
            has_joey_dir: true,
            has_skills: true,
            has_hooks: false,
            has_mcp_config: true,
            has_soul_md: false,
        };
        assert!(r.requires_trust_prompt());
        let summary = r.summary();
        assert!(summary.contains(&".joey/ directory".to_string()));
        assert!(summary.contains(&"project skills".to_string()));
        assert!(summary.contains(&"MCP server config".to_string()));
    }

    #[test]
    fn empty_resources_no_prompt() {
        let r = ProjectResources::default();
        assert!(!r.requires_trust_prompt());
        assert!(r.summary().is_empty());
    }

    #[test]
    fn trust_prompt_includes_resources() {
        let cwd = Path::new("/tmp/myproject");
        let r = ProjectResources {
            has_joey_dir: true,
            has_skills: false,
            has_hooks: false,
            has_mcp_config: false,
            has_soul_md: false,
        };
        let prompt = trust_prompt(cwd, &r);
        assert!(prompt.contains("/tmp/myproject"));
        assert!(prompt.contains(".joey/ directory"));
    }
}
