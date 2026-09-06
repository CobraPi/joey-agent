//! spec-kit extension-hook discovery (spec FR-004 / contracts/hooks.md).
//!
//! Reads `<repo_root>/.specify/extensions.yml` and flattens hook entries in
//! canonical hook-point order. Per contract rule 1, any parse failure or
//! unreadable/absent file is skipped silently.

use joey_core::Config;
use std::collections::BTreeMap;
use std::path::Path;

/// The twenty canonical hook points: before_/after_ for each of the ten
/// workflow commands.
pub const HOOK_POINTS: &[&str] = &[
    "before_specify",
    "after_specify",
    "before_clarify",
    "after_clarify",
    "before_plan",
    "after_plan",
    "before_constitution",
    "after_constitution",
    "before_checklist",
    "after_checklist",
    "before_tasks",
    "after_tasks",
    "before_analyze",
    "after_analyze",
    "before_implement",
    "after_implement",
    "before_converge",
    "after_converge",
    "before_taskstoissues",
    "after_taskstoissues",
];

fn default_true() -> bool {
    true
}

/// A single hook entry from `.specify/extensions.yml`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct HookEntry {
    pub extension: String,
    pub command: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub condition: Option<String>,
}

/// Parsed shape of `.specify/extensions.yml` (only the `hooks` mapping is
/// consumed; unknown top-level keys are ignored).
#[derive(Debug, Default, serde::Deserialize)]
pub struct ExtensionsFile {
    #[serde(default)]
    pub hooks: BTreeMap<String, Vec<HookEntry>>,
}

/// Parse an extensions file body. ANY error — malformed YAML, non-mapping
/// YAML — yields the empty default (silent skip, contract rule 1).
pub fn parse_extensions(raw: &str) -> ExtensionsFile {
    serde_yaml::from_str::<ExtensionsFile>(raw).unwrap_or_default()
}

/// Discover hooks under `repo_root/.specify/extensions.yml`, flattened in
/// HOOK_POINTS order. Entries with `enabled: false` are filtered out;
/// entries with a non-empty `condition` are KEPT but flagged (see
/// [`is_executable`]). Unknown hook-point keys in the file are ignored.
pub fn discover(repo_root: &Path) -> Vec<(String, HookEntry)> {
    let path = repo_root.join(".specify/extensions.yml");
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(_) => return Vec::new(), // absent or unreadable: no hooks
    };
    let file = parse_extensions(&raw);
    let mut out = Vec::new();
    for point in HOOK_POINTS {
        if let Some(entries) = file.hooks.get(*point) {
            for entry in entries {
                if entry.enabled {
                    out.push((point.to_string(), entry.clone()));
                }
            }
        }
    }
    out
}

/// Convenience wrapper: the enabled hook entries registered for `point`.
pub fn hooks_for(repo_root: &Path, point: &str) -> Vec<HookEntry> {
    discover(repo_root)
        .into_iter()
        .filter(|(p, _)| p == point)
        .map(|(_, e)| e)
        .collect()
}

/// A hook is directly executable only when it carries no condition
/// expression (conditions are never interpreted or evaluated by us; they
/// are surfaced to the model, which decides).
pub fn is_executable(h: &HookEntry) -> bool {
    h.condition.is_none() || h.condition.as_deref() == Some("")
}

/// Normalize a dotted command name to its slash-command form: dots become
/// hyphens (`speckit.git.commit` → `speckit-git-commit`). No leading slash —
/// the caller adds it.
pub fn slash_form(command: &str) -> String {
    command.replace('.', "-")
}

/// Whether config permits speckit hooks at all (both default to true).
pub fn config_allows(config: &Config) -> bool {
    config.get_bool("speckit.enabled", true) && config.get_bool("speckit.hooks", true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_extensions(root: &Path, body: &str) {
        let dir = root.join(".specify");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("extensions.yml"), body).unwrap();
    }

    #[test]
    fn hook_points_enumerates_all_twenty() {
        assert_eq!(HOOK_POINTS.len(), 20);
        for name in crate::speckit_bodies::COMMAND_NAMES {
            assert!(
                HOOK_POINTS.contains(&format!("before_{name}").as_str()),
                "missing before_{name}"
            );
            assert!(
                HOOK_POINTS.contains(&format!("after_{name}").as_str()),
                "missing after_{name}"
            );
        }
    }

    #[test]
    fn discover_filters_disabled_entries() {
        let dir = tempfile::tempdir().unwrap();
        write_extensions(
            dir.path(),
            r"hooks:
  before_plan:
    - extension: git
      command: speckit.git.commit
    - extension: ci
      command: speckit.ci.check
      enabled: false
  after_plan:
    - extension: docs
      command: speckit.docs.update
",
        );
        let found = discover(dir.path());
        let commands: Vec<&str> = found.iter().map(|(_, e)| e.command.as_str()).collect();
        assert!(commands.contains(&"speckit.git.commit"));
        assert!(commands.contains(&"speckit.docs.update"));
        assert!(!commands.contains(&"speckit.ci.check"));
        // Order follows HOOK_POINTS: before_plan precedes after_plan.
        let bp = found.iter().position(|(p, _)| p == "before_plan").unwrap();
        let ap = found.iter().position(|(p, _)| p == "after_plan").unwrap();
        assert!(bp < ap);
        assert_eq!(hooks_for(dir.path(), "before_plan").len(), 1);
    }

    #[test]
    fn invalid_yaml_yields_empty_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        write_extensions(dir.path(), ":~bad");
        assert!(discover(dir.path()).is_empty());
    }

    #[test]
    fn missing_file_yields_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(discover(dir.path()).is_empty());
        assert!(hooks_for(dir.path(), "before_specify").is_empty());
    }

    #[test]
    fn optional_defaults_false() {
        let dir = tempfile::tempdir().unwrap();
        write_extensions(
            dir.path(),
            "hooks:\n  before_tasks:\n    - extension: x\n      command: x.y\n",
        );
        let (_, entry) = discover(dir.path()).into_iter().next().unwrap();
        assert!(!entry.optional);
        assert!(entry.enabled); // defaults true
        assert_eq!(entry.description, "");
        assert_eq!(entry.prompt, "");
    }

    #[test]
    fn condition_entries_kept_but_not_executable() {
        let dir = tempfile::tempdir().unwrap();
        write_extensions(
            dir.path(),
            "hooks:\n  before_implement:\n    - extension: bench\n      command: speckit.bench.run\n      condition: repo_has_benchmarks\n",
        );
        let found = discover(dir.path());
        assert_eq!(found.len(), 1, "conditioned entry is kept");
        let entry = &found[0].1;
        assert_eq!(entry.condition.as_deref(), Some("repo_has_benchmarks"));
        assert!(!is_executable(entry));
    }

    #[test]
    fn slash_form_normalizes_dots() {
        assert_eq!(slash_form("speckit.git.commit"), "speckit-git-commit");
        assert_eq!(slash_form("speckit.lint"), "speckit-lint");
        assert!(!slash_form("speckit.git.commit").starts_with('/'));
    }

    #[test]
    fn config_allows_defaults_true() {
        assert!(config_allows(&Config::defaults()));
    }
}
