//! Skills tools: skills_list + skill_view (port of `tools/skills_tool.py`).
//!
//! Skills follow the Agent Skills convention: one directory per skill with a
//! `SKILL.md` holding YAML frontmatter (`name`, `description`) + a markdown body.

use std::path::PathBuf;

use async_trait::async_trait;
use serde_json::{json, Value};
use walkdir::WalkDir;

use crate::context::ToolContext;
use crate::registry::{tool_error, Tool, ToolResult};

/// A discovered skill.
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
}

fn skills_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![joey_core::constants::skills_dir()];
    // Bundled skills shipped alongside the binary, if any.
    let bundled = joey_core::constants::bundled_skills_dir(None);
    if bundled != dirs[0] {
        dirs.push(bundled);
    }
    dirs
}

/// Discover all skills across the active + bundled skill dirs.
pub fn discover() -> Vec<SkillEntry> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in skills_dirs() {
        if !dir.exists() {
            continue;
        }
        for entry in WalkDir::new(&dir).max_depth(4).into_iter().flatten() {
            if entry.file_name() != "SKILL.md" {
                continue;
            }
            let path = entry.path().to_path_buf();
            if let Some(skill) = parse_skill(&path) {
                if seen.insert(skill.name.clone()) {
                    out.push(skill);
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn parse_skill(path: &PathBuf) -> Option<SkillEntry> {
    let text = std::fs::read_to_string(path).ok()?;
    let (name, description) = parse_frontmatter(&text);
    let name = name.unwrap_or_else(|| {
        path.parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    Some(SkillEntry {
        name,
        description: description.unwrap_or_default(),
        path: path.clone(),
    })
}

/// Extract `name:` and `description:` from a YAML frontmatter block.
fn parse_frontmatter(text: &str) -> (Option<String>, Option<String>) {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return (None, None);
    }
    let after = &trimmed[3..];
    let Some(end) = after.find("\n---") else {
        return (None, None);
    };
    let front = &after[..end];
    let yaml: Value = serde_yaml::from_str(front).unwrap_or(Value::Null);
    let name = yaml.get("name").and_then(|v| v.as_str()).map(str::to_string);
    let desc = yaml.get("description").and_then(|v| v.as_str()).map(str::to_string);
    (name, desc)
}

pub struct SkillsList;

#[async_trait]
impl Tool for SkillsList {
    fn name(&self) -> &str {
        "skills_list"
    }
    fn toolset(&self) -> &str {
        "skills"
    }
    fn description(&self) -> &str {
        "List available skills (name + description). Load one with skill_view before acting."
    }
    fn emoji(&self) -> &str {
        "📚"
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {"category": {"type": "string"}}})
    }
    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        let skills = discover();
        if skills.is_empty() {
            return ToolResult::Text("No skills installed.".to_string());
        }
        let mut out = String::from("Available skills:\n");
        for s in skills {
            out.push_str(&format!("- {}: {}\n", s.name, s.description));
        }
        ToolResult::Text(out)
    }
}

pub struct SkillView;

#[async_trait]
impl Tool for SkillView {
    fn name(&self) -> &str {
        "skill_view"
    }
    fn toolset(&self) -> &str {
        "skills"
    }
    fn description(&self) -> &str {
        "Read a skill's full SKILL.md content by name, then follow its instructions."
    }
    fn emoji(&self) -> &str {
        "📚"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"name": {"type": "string"}},
            "required": ["name"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
            return tool_error("missing required parameter: name");
        };
        let skills = discover();
        let Some(skill) = skills.iter().find(|s| s.name == name) else {
            return tool_error(format!("skill '{}' not found", name));
        };
        match std::fs::read_to_string(&skill.path) {
            Ok(content) => ToolResult::Text(content),
            Err(e) => tool_error(format!("cannot read skill: {}", e)),
        }
    }
}
