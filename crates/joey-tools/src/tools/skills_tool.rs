//! Skills tools: skills_list + skill_view — port of `tools/skills_tool.py`
//! (JSON envelopes, category filtering, `skills.external_dirs` +
//! disabled-skill config, linked-files discovery, and the file_path mode with
//! traversal protection).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde_json::{json, Map, Value};
use walkdir::WalkDir;

use crate::context::ToolContext;
use crate::guards::has_traversal_component;
use crate::pyjson::dumps;
use crate::registry::{Tool, ToolResult};

const MAX_NAME_LENGTH: usize = 100;
const MAX_DESCRIPTION_LENGTH: usize = 500;

/// A discovered skill.
pub struct SkillEntry {
    pub name: String,
    pub description: String,
    pub category: Option<String>,
    pub path: PathBuf,
}

fn skills_dir() -> PathBuf {
    joey_core::constants::skills_dir()
}

fn external_dirs(ctx: Option<&ToolContext>) -> Vec<PathBuf> {
    let Some(ctx) = ctx else { return Vec::new() };
    ctx.config()
        .get_str_list("skills.external_dirs")
        .into_iter()
        .map(|d| PathBuf::from(shellexpand::tilde(&d).to_string()))
        .filter(|p| p.is_dir())
        .collect()
}

fn disabled_skills(ctx: Option<&ToolContext>) -> Vec<String> {
    match ctx {
        Some(c) => c.config().get_str_list("skills.disabled"),
        None => Vec::new(),
    }
}

/// Extract frontmatter fields from a SKILL.md.
fn parse_frontmatter(text: &str) -> (Map<String, Value>, String) {
    let trimmed = text.trim_start();
    if !trimmed.starts_with("---") {
        return (Map::new(), text.to_string());
    }
    let after = &trimmed[3..];
    let Some(end) = after.find("\n---") else {
        return (Map::new(), text.to_string());
    };
    let front = &after[..end];
    let body = after[end + 4..].trim_start_matches('\n').to_string();
    let yaml: Value = serde_yaml::from_str::<serde_json::Value>(front).unwrap_or(Value::Null);
    let map = yaml.as_object().cloned().unwrap_or_default();
    (map, body)
}

fn category_from_path(skill_md: &Path, base: &Path) -> Option<String> {
    let rel = skill_md.parent()?.strip_prefix(base).ok()?;
    let parts: Vec<String> =
        rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    if parts.len() >= 2 {
        Some(parts[..parts.len() - 1].join("/"))
    } else {
        None
    }
}

/// Discover all skills across the local + external skill dirs (name-deduped,
/// local wins), with disabled-skill filtering.
pub fn discover_with(ctx: Option<&ToolContext>) -> Vec<SkillEntry> {
    let mut dirs = vec![skills_dir()];
    let bundled = joey_core::constants::bundled_skills_dir(None);
    if !dirs.contains(&bundled) {
        dirs.push(bundled);
    }
    dirs.extend(external_dirs(ctx));
    let disabled = disabled_skills(ctx);

    let mut out: Vec<SkillEntry> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        for entry in WalkDir::new(&dir).max_depth(6).into_iter().flatten() {
            if entry.file_name() != "SKILL.md" {
                continue;
            }
            let skill_md = entry.path().to_path_buf();
            // Skip skill support dirs.
            if skill_md.components().any(|c| {
                matches!(
                    c.as_os_str().to_str(),
                    Some("references") | Some("templates") | Some("assets") | Some("scripts") | Some(".hub")
                )
            }) {
                continue;
            }
            let Ok(content) = std::fs::read_to_string(&skill_md) else {
                continue;
            };
            let head: String = content.chars().take(4000).collect();
            let (frontmatter, body) = parse_frontmatter(&head);
            let dir_name = skill_md
                .parent()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "unknown".to_string());
            let name: String = frontmatter
                .get("name")
                .and_then(|n| n.as_str())
                .map(str::to_string)
                .unwrap_or(dir_name)
                .chars()
                .take(MAX_NAME_LENGTH)
                .collect();
            if !seen.insert(name.clone()) {
                continue;
            }
            if disabled.contains(&name) {
                continue;
            }
            let mut description = frontmatter
                .get("description")
                .and_then(|d| d.as_str())
                .map(str::to_string)
                .unwrap_or_default();
            if description.is_empty() {
                for line in body.trim().split('\n') {
                    let line = line.trim();
                    if !line.is_empty() && !line.starts_with('#') {
                        description = line.to_string();
                        break;
                    }
                }
            }
            if description.chars().count() > MAX_DESCRIPTION_LENGTH {
                let kept: String = description.chars().take(MAX_DESCRIPTION_LENGTH - 3).collect();
                description = format!("{}...", kept);
            }
            out.push(SkillEntry {
                name,
                description,
                category: category_from_path(&skill_md, &dir),
                path: skill_md,
            });
        }
    }
    out.sort_by(|a, b| {
        (a.category.clone().unwrap_or_default(), a.name.clone())
            .cmp(&(b.category.clone().unwrap_or_default(), b.name.clone()))
    });
    out
}

/// Discover skills without config context (system-prompt/CLI listings).
pub fn discover() -> Vec<SkillEntry> {
    discover_with(None)
}

fn parse_tags(v: Option<&Value>) -> Vec<String> {
    match v {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|i| i.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        Some(Value::String(s)) => s
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
        _ => Vec::new(),
    }
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
        "List available skills (name + description). Use skill_view(name) to load full content."
    }
    fn emoji(&self) -> &str {
        "📚"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "category": {
                    "type": "string",
                    "description": "Optional category filter to narrow results",
                }
            },
            "required": [],
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let category = args.get("category").and_then(|c| c.as_str()).map(str::to_string);
        let active_dir = skills_dir();
        if !active_dir.exists() {
            let _ = std::fs::create_dir_all(&active_dir);
            return ToolResult::Text(dumps(&json!({
                "success": true,
                "skills": [],
                "categories": [],
                "message": format!(
                    "No skills found. Skills directory created at {}/skills/",
                    joey_core::constants::display_joey_home()
                ),
            })));
        }
        let mut skills = discover_with(Some(ctx));
        if skills.is_empty() {
            return ToolResult::Text(dumps(&json!({
                "success": true,
                "skills": [],
                "categories": [],
                "message": "No skills found in skills/ directory.",
            })));
        }
        if let Some(cat) = &category {
            skills.retain(|s| s.category.as_deref() == Some(cat.as_str()));
        }
        let mut categories: Vec<String> =
            skills.iter().filter_map(|s| s.category.clone()).collect();
        categories.sort();
        categories.dedup();
        let skills_json: Vec<Value> = skills
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "description": s.description,
                    "category": s.category,
                })
            })
            .collect();
        ToolResult::Text(dumps(&json!({
            "success": true,
            "skills": skills_json,
            "categories": categories,
            "count": skills.len(),
            "hint": "Use skill_view(name) to see full content, tags, and linked files",
        })))
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
        "Skills allow for loading information about specific tasks and workflows, as well as scripts and templates. Load a skill's full content or access its linked files (references, templates, scripts). First call returns SKILL.md content plus a 'linked_files' dict showing available references/templates/scripts. To access those, call again with file_path parameter."
    }
    fn emoji(&self) -> &str {
        "📚"
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The skill name (use skills_list to see available skills). For plugin-provided skills, use the qualified form 'plugin:skill' (e.g. 'superpowers:writing-plans').",
                },
                "file_path": {
                    "type": "string",
                    "description": "OPTIONAL: Path to a linked file within the skill (e.g., 'references/api.md', 'templates/config.yaml', 'scripts/validate.py'). Omit to get the main SKILL.md content.",
                },
            },
            "required": ["name"],
        })
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let file_path = args.get("file_path").and_then(|v| v.as_str()).map(str::to_string);

        // Reject traversal/absolute names before any path joins.
        if has_traversal_component(&name) || Path::new(&name).is_absolute() {
            return ToolResult::Text(dumps(&json!({
                "success": false,
                "error": format!("Invalid skill name '{}': absolute paths and '..' traversal are not allowed.", name),
                "hint": "Use a skill name or relative path within the skills directory.",
            })));
        }

        let skills = discover_with(Some(ctx));
        let candidates: Vec<&SkillEntry> = skills
            .iter()
            .filter(|s| {
                s.name == name
                    || s.path
                        .parent()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy() == name.as_str())
                        .unwrap_or(false)
            })
            .collect();

        if candidates.len() > 1 {
            let paths: Vec<String> =
                candidates.iter().map(|c| c.path.to_string_lossy().into_owned()).collect();
            return ToolResult::Text(dumps(&json!({
                "success": false,
                "error": format!(
                    "Ambiguous skill name '{}': {} skills match across your local skills dir and external_dirs. Refusing to guess — load one explicitly by its categorized path.",
                    name,
                    candidates.len()
                ),
                "matches": paths,
                "hint": "Pass the full relative path instead of the bare name (e.g., 'category/skill-name'), or rename one of the colliding skills so each name is unique.",
            })));
        }

        let Some(skill) = candidates.first() else {
            let available: Vec<String> = skills.iter().take(20).map(|s| s.name.clone()).collect();
            return ToolResult::Text(dumps(&json!({
                "success": false,
                "error": format!("Skill '{}' not found.", name),
                "available_skills": available,
                "hint": "Use skills_list to see all available skills",
            })));
        };

        let skill_md = skill.path.clone();
        let skill_dir = skill_md.parent().map(|p| p.to_path_buf());
        let content = match std::fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(e) => {
                return ToolResult::Text(dumps(&json!({
                    "success": false,
                    "error": format!("Failed to read skill '{}': {}", name, e),
                })))
            }
        };
        let (frontmatter, _body) = parse_frontmatter(&content);

        // ── file_path mode ────────────────────────────────────────────
        if let (Some(fp), Some(dir)) = (&file_path, &skill_dir) {
            if has_traversal_component(fp) {
                return ToolResult::Text(dumps(&json!({
                    "success": false,
                    "error": "Path traversal ('..') is not allowed.",
                    "hint": "Use a relative path within the skill directory",
                })));
            }
            let target_file = dir.join(fp);
            // Verify resolved path stays within the skill directory.
            let resolved_ok = target_file
                .canonicalize()
                .ok()
                .zip(dir.canonicalize().ok())
                .map(|(t, d)| t.starts_with(&d))
                .unwrap_or(target_file.starts_with(dir));
            if !resolved_ok {
                return ToolResult::Text(dumps(&json!({
                    "success": false,
                    "error": "Path escapes allowed directory",
                    "hint": "Use a relative path within the skill directory",
                })));
            }
            if !target_file.exists() {
                let mut available: Map<String, Value> = Map::new();
                let mut buckets: indexmap::IndexMap<&str, Vec<String>> = indexmap::IndexMap::new();
                for key in ["references", "templates", "assets", "scripts", "other"] {
                    buckets.insert(key, Vec::new());
                }
                for f in WalkDir::new(dir).into_iter().flatten() {
                    if !f.path().is_file() || f.file_name() == "SKILL.md" {
                        continue;
                    }
                    let rel = f
                        .path()
                        .strip_prefix(dir)
                        .map(|r| r.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let bucket = if rel.starts_with("references/") {
                        "references"
                    } else if rel.starts_with("templates/") {
                        "templates"
                    } else if rel.starts_with("assets/") {
                        "assets"
                    } else if rel.starts_with("scripts/") {
                        "scripts"
                    } else if matches!(
                        f.path().extension().and_then(|e| e.to_str()),
                        Some("md") | Some("py") | Some("yaml") | Some("yml") | Some("json")
                            | Some("tex") | Some("sh")
                    ) {
                        "other"
                    } else {
                        continue;
                    };
                    buckets.get_mut(bucket).unwrap().push(rel);
                }
                for (k, v) in buckets {
                    if !v.is_empty() {
                        available.insert(k.to_string(), json!(v));
                    }
                }
                return ToolResult::Text(dumps(&json!({
                    "success": false,
                    "error": format!("File '{}' not found in skill '{}'.", fp, name),
                    "available_files": Value::Object(available),
                    "hint": "Use one of the available file paths listed above",
                })));
            }
            return match std::fs::read_to_string(&target_file) {
                Ok(file_content) => ToolResult::Text(dumps(&json!({
                    "success": true,
                    "name": name,
                    "file": fp,
                    "content": file_content,
                    "file_type": target_file
                        .extension()
                        .map(|e| format!(".{}", e.to_string_lossy()))
                        .unwrap_or_default(),
                }))),
                Err(_) => {
                    let size = std::fs::metadata(&target_file).map(|m| m.len()).unwrap_or(0);
                    ToolResult::Text(dumps(&json!({
                        "success": true,
                        "name": name,
                        "file": fp,
                        "content": format!(
                            "[Binary file: {}, size: {} bytes]",
                            target_file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                            size
                        ),
                        "is_binary": true,
                    })))
                }
            };
        }

        // ── Main SKILL.md mode ────────────────────────────────────────
        let mut reference_files: Vec<String> = Vec::new();
        let mut template_files: Vec<String> = Vec::new();
        let mut asset_files: Vec<String> = Vec::new();
        let mut script_files: Vec<String> = Vec::new();
        if let Some(dir) = &skill_dir {
            let refs = dir.join("references");
            if refs.exists() {
                for f in WalkDir::new(&refs).max_depth(1).into_iter().flatten() {
                    if f.path().is_file()
                        && f.path().extension().and_then(|e| e.to_str()) == Some("md")
                    {
                        if let Ok(rel) = f.path().strip_prefix(dir) {
                            reference_files.push(rel.to_string_lossy().into_owned());
                        }
                    }
                }
            }
            let templates = dir.join("templates");
            if templates.exists() {
                for f in WalkDir::new(&templates).into_iter().flatten() {
                    if f.path().is_file() {
                        if let Ok(rel) = f.path().strip_prefix(dir) {
                            template_files.push(rel.to_string_lossy().into_owned());
                        }
                    }
                }
            }
            let assets = dir.join("assets");
            if assets.exists() {
                for f in WalkDir::new(&assets).into_iter().flatten() {
                    if f.path().is_file() {
                        if let Ok(rel) = f.path().strip_prefix(dir) {
                            asset_files.push(rel.to_string_lossy().into_owned());
                        }
                    }
                }
            }
            let scripts = dir.join("scripts");
            if scripts.exists() {
                for f in WalkDir::new(&scripts).max_depth(1).into_iter().flatten() {
                    if f.path().is_file()
                        && matches!(
                            f.path().extension().and_then(|e| e.to_str()),
                            Some("py") | Some("sh") | Some("bash") | Some("js") | Some("ts")
                                | Some("rb")
                        )
                    {
                        if let Ok(rel) = f.path().strip_prefix(dir) {
                            script_files.push(rel.to_string_lossy().into_owned());
                        }
                    }
                }
            }
        }
        let mut linked_files: Map<String, Value> = Map::new();
        if !reference_files.is_empty() {
            linked_files.insert("references".into(), json!(reference_files));
        }
        if !template_files.is_empty() {
            linked_files.insert("templates".into(), json!(template_files));
        }
        if !asset_files.is_empty() {
            linked_files.insert("assets".into(), json!(asset_files));
        }
        if !script_files.is_empty() {
            linked_files.insert("scripts".into(), json!(script_files));
        }

        // tags / related_skills: metadata.joey.* first, then top-level.
        let joey_meta = frontmatter
            .get("metadata")
            .and_then(|m| m.as_object())
            .and_then(|m| m.get("joey"))
            .and_then(|h| h.as_object())
            .cloned()
            .unwrap_or_default();
        let tags = {
            let t = parse_tags(joey_meta.get("tags"));
            if t.is_empty() {
                parse_tags(frontmatter.get("tags"))
            } else {
                t
            }
        };
        let related_skills = {
            let t = parse_tags(joey_meta.get("related_skills"));
            if t.is_empty() {
                parse_tags(frontmatter.get("related_skills"))
            } else {
                t
            }
        };

        let rel_path = skill_md
            .strip_prefix(skills_dir())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| {
                skill_md
                    .parent()
                    .and_then(|p| p.parent())
                    .and_then(|pp| skill_md.strip_prefix(pp).ok())
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_else(|| skill_md.file_name().unwrap_or_default().to_string_lossy().into_owned())
            });
        let skill_name = frontmatter
            .get("name")
            .and_then(|n| n.as_str())
            .map(str::to_string)
            .unwrap_or_else(|| {
                skill_dir
                    .as_ref()
                    .and_then(|d| d.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| name.clone())
            });

        let mut result = Map::new();
        result.insert("success".into(), json!(true));
        result.insert("name".into(), json!(skill_name));
        result.insert(
            "description".into(),
            json!(frontmatter.get("description").and_then(|d| d.as_str()).unwrap_or("")),
        );
        result.insert("tags".into(), json!(tags));
        result.insert("related_skills".into(), json!(related_skills));
        result.insert("content".into(), json!(content));
        result.insert("path".into(), json!(rel_path));
        result.insert(
            "skill_dir".into(),
            match &skill_dir {
                Some(d) => json!(d.to_string_lossy()),
                None => Value::Null,
            },
        );
        result.insert(
            "linked_files".into(),
            if linked_files.is_empty() { Value::Null } else { Value::Object(linked_files.clone()) },
        );
        result.insert(
            "usage_hint".into(),
            if linked_files.is_empty() {
                Value::Null
            } else {
                json!("To view linked files, call skill_view(name, file_path) where file_path is e.g. 'references/api.md' or 'assets/config.yaml'")
            },
        );
        ToolResult::Text(dumps(&Value::Object(result)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joey_core::Config;

    struct HomeCtx {
        _guard: joey_core::constants::HomeOverrideGuard,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    /// The joey-home override is process-global; serialize tests that use it.
    fn setup_home() -> (ToolContext, HomeCtx, PathBuf) {
        let lock = crate::test_env_lock();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_path_buf();
        let guard = joey_core::constants::HomeOverrideGuard::new(home.clone());
        std::mem::forget(dir);
        let ctx = ToolContext::new(std::env::temp_dir(), Config::defaults(), "s");
        (ctx, HomeCtx { _guard: guard, _lock: lock }, home)
    }

    fn make_skill(home: &Path, cat: Option<&str>, name: &str, desc: &str) -> PathBuf {
        let dir = match cat {
            Some(c) => home.join("skills").join(c).join(name),
            None => home.join("skills").join(name),
        };
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {}\ndescription: {}\n---\n\n# {}\nBody text.\n", name, desc, name),
        )
        .unwrap();
        dir
    }

    fn parse(r: &ToolResult) -> Value {
        serde_json::from_str(&r.to_content_string()).unwrap()
    }

    #[tokio::test]
    async fn list_envelope_and_category_filter() {
        let (ctx, _g, home) = setup_home();
        make_skill(&home, None, "alpha", "first skill");
        make_skill(&home, Some("mlops"), "beta", "second skill");
        let v = parse(&SkillsList.execute(json!({}), &ctx).await);
        assert_eq!(v["success"], true);
        assert_eq!(v["count"], 2);
        assert_eq!(v["hint"], "Use skill_view(name) to see full content, tags, and linked files");
        assert_eq!(v["categories"], json!(["mlops"]));

        let filtered = parse(&SkillsList.execute(json!({"category": "mlops"}), &ctx).await);
        assert_eq!(filtered["count"], 1);
        assert_eq!(filtered["skills"][0]["name"], "beta");
    }

    #[tokio::test]
    async fn list_empty_dir_message() {
        let (ctx, _g, home) = setup_home();
        let v = parse(&SkillsList.execute(json!({}), &ctx).await);
        assert_eq!(v["success"], true);
        assert!(v["message"].as_str().unwrap().starts_with("No skills found."));
        assert!(home.join("skills").exists());
    }

    #[tokio::test]
    async fn view_full_envelope_and_linked_files() {
        let (ctx, _g, home) = setup_home();
        let dir = make_skill(&home, None, "gamma", "third");
        std::fs::create_dir_all(dir.join("references")).unwrap();
        std::fs::write(dir.join("references/api.md"), "ref content").unwrap();
        let v = parse(&SkillView.execute(json!({"name": "gamma"}), &ctx).await);
        assert_eq!(v["success"], true);
        assert_eq!(v["name"], "gamma");
        assert_eq!(v["description"], "third");
        assert!(v["content"].as_str().unwrap().contains("# gamma"));
        assert_eq!(v["linked_files"]["references"], json!(["references/api.md"]));
        assert!(v["usage_hint"].as_str().unwrap().starts_with("To view linked files"));

        // file_path mode returns the linked file.
        let f = parse(
            &SkillView
                .execute(json!({"name": "gamma", "file_path": "references/api.md"}), &ctx)
                .await,
        );
        assert_eq!(f["success"], true);
        assert_eq!(f["file"], "references/api.md");
        assert_eq!(f["content"], "ref content");

        // Missing file lists available files.
        let miss = parse(
            &SkillView.execute(json!({"name": "gamma", "file_path": "references/nope.md"}), &ctx).await,
        );
        assert_eq!(miss["success"], false);
        assert_eq!(miss["error"], "File 'references/nope.md' not found in skill 'gamma'.");
        assert_eq!(miss["available_files"]["references"], json!(["references/api.md"]));

        // Traversal rejected.
        let trav = parse(
            &SkillView.execute(json!({"name": "gamma", "file_path": "../../etc/passwd"}), &ctx).await,
        );
        assert_eq!(trav["error"], "Path traversal ('..') is not allowed.");
    }

    #[tokio::test]
    async fn view_not_found_lists_available() {
        let (ctx, _g, home) = setup_home();
        make_skill(&home, None, "delta", "d");
        let v = parse(&SkillView.execute(json!({"name": "missing"}), &ctx).await);
        assert_eq!(v["success"], false);
        assert_eq!(v["error"], "Skill 'missing' not found.");
        assert_eq!(v["available_skills"], json!(["delta"]));
        assert_eq!(v["hint"], "Use skills_list to see all available skills");
    }

    #[tokio::test]
    async fn disabled_skills_filtered() {
        let (_ctx, _g, home) = setup_home();
        make_skill(&home, None, "epsilon", "e");
        // Build a ctx whose config disables the skill via a config file.
        let cfg_path = home.join("config.yaml");
        std::fs::write(&cfg_path, "skills:\n  disabled:\n    - epsilon\n").unwrap();
        let cfg = Config::load_from(cfg_path).unwrap();
        let ctx = ToolContext::new(std::env::temp_dir(), cfg, "s2");
        let v = parse(&SkillsList.execute(json!({}), &ctx).await);
        // The only skill is disabled → empty listing message.
        assert_eq!(v["message"], "No skills found in skills/ directory.");
    }
}
