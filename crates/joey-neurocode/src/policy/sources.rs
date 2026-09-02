//! Policy instruction-file sources (spec 023, FR-003).
//!
//! Discovers instruction files (`JOEY.md`, `AGENTS.md`, `CLAUDE.md`,
//! `.cursorrules`, `.github/copilot-instructions.md`), parses their
//! directive lines into [`PolicyBinding`]s, and honors `applyTo:` glob
//! scoping to demote bindings to the [`PolicyLayer::ScopedRule`] layer.

use std::path::{Path, PathBuf};

use super::{PolicyBinding, PolicyLayer};
use walkdir::WalkDir;

/// Directory names that are never traversed during discovery.
const SKIPPED_DIRS: [&str; 6] = [".git", "node_modules", "target", "dist", "build", ".venv"];

/// Instruction-file basenames discovered at any depth.
const INSTRUCTION_FILE_NAMES: [&str; 3] = ["JOEY.md", "AGENTS.md", "CLAUDE.md"];

/// Discover policy instruction files under `root` (FR-003).
///
/// Walks up to depth 4 for files named exactly `JOEY.md`, `AGENTS.md`, or
/// `CLAUDE.md` (skipping `.git`, `node_modules`, `target`, `dist`, `build`,
/// `.venv` directories; `.github` is entered), plus `root/.cursorrules` and
/// `root/.github/copilot-instructions.md` when present. Only paths inside
/// `root` are accepted. Output is sorted by path string for determinism.
pub fn discover(root: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = Vec::new();

    for entry in WalkDir::new(root)
        .max_depth(4)
        .into_iter()
        .filter_entry(|e| {
            if e.depth() == 0 {
                return true;
            }
            if e.file_type().is_dir() {
                let name = e.file_name().to_string_lossy();
                !SKIPPED_DIRS.contains(&name.as_ref())
            } else {
                true
            }
        })
    {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if INSTRUCTION_FILE_NAMES.contains(&name.as_ref()) {
            let path = entry.into_path();
            // Relative-safety: only accept paths inside root.
            if path.strip_prefix(root).is_ok() {
                found.push(path);
            }
        }
    }

    let extras = [
        root.join(".cursorrules"),
        root.join(".github").join("copilot-instructions.md"),
    ];
    for path in extras {
        if path.is_file() && path.strip_prefix(root).is_ok() && !found.contains(&path) {
            found.push(path);
        }
    }

    found.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    found
}

/// Glob matching primitive used for `applyTo:` scoping (FR-003).
///
/// `**` matches any sequence including `/`, `*` matches any sequence
/// excluding `/`, `?` matches exactly one non-`/` character; everything
/// else is literal. The match is anchored to the full string. If the
/// pattern contains no `/`, it additionally matches the path's basename.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = path.chars().collect();
    if match_here(&p, &s) {
        return true;
    }
    if !pattern.contains('/') {
        let basename = path.rsplit('/').next().unwrap_or(path);
        let b: Vec<char> = basename.chars().collect();
        return match_here(&p, &b);
    }
    false
}

/// Recursive glob matcher over char slices (no regex crate).
fn match_here(p: &[char], s: &[char]) -> bool {
    if p.is_empty() {
        return s.is_empty();
    }
    if p[0] == '*' {
        if p.len() > 1 && p[1] == '*' {
            // `**` matches any sequence, including '/'.
            let rest = &p[2..];
            for i in 0..=s.len() {
                if match_here(rest, &s[i..]) {
                    return true;
                }
            }
            return false;
        }
        // `*` matches any sequence excluding '/'.
        let rest = &p[1..];
        for i in 0..=s.len() {
            if match_here(rest, &s[i..]) {
                return true;
            }
            if i < s.len() && s[i] == '/' {
                break;
            }
        }
        return false;
    }
    if p[0] == '?' {
        return !s.is_empty() && s[0] != '/' && match_here(&p[1..], &s[1..]);
    }
    !s.is_empty() && s[0] == p[0] && match_here(&p[1..], &s[1..])
}

/// Parse one instruction file into [`PolicyBinding`]s (FR-003).
///
/// Line-based state machine: a line whose trimmed form starts with
/// `applyTo:` (optionally preceded by a `- ` / `* ` list prefix) sets the
/// current glob scope. Each subsequent `- ` / `* ` directive line emits a
/// binding whose `applies_to` is the current glob (or the file's default
/// glob), and whose layer is [`PolicyLayer::ScopedRule`] when a non-`**`
/// glob is active. If the file contains no directive lines at all, a single
/// binding is emitted from the first non-empty non-heading, non-applyTo
/// line. Unreadable files yield an empty vec.
pub fn parse_instruction_file(path: &Path, root: &Path, layer: PolicyLayer) -> Vec<PolicyBinding> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };

    let mut bindings: Vec<PolicyBinding> = Vec::new();
    let mut current_glob: Option<String> = None;
    let mut saw_directive = false;
    let mut fallback: Option<String> = None;

    for raw in text.lines() {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let after_prefix = strip_list_prefix(trimmed);

        // `applyTo:` scope line (with or without a list prefix).
        if let Some(rest) = after_prefix.strip_prefix("applyTo:") {
            let value = rest.trim();
            current_glob = Some(strip_surrounding_quotes(value));
            continue;
        }

        // Directive line: starts with `- ` or `* `.
        if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
            saw_directive = true;
            let glob = current_glob
                .clone()
                .unwrap_or_else(|| default_glob(path, root));
            let effective_layer = match current_glob.as_deref() {
                Some(g) if g != "**" => PolicyLayer::ScopedRule,
                _ => layer.clone(),
            };
            bindings.push(PolicyBinding {
                layer: effective_layer,
                source_path: path.to_path_buf(),
                applies_to: vec![glob],
                directive: after_prefix.trim().to_string(),
                conflicts_with: Vec::new(),
            });
        } else if fallback.is_none() && !trimmed.starts_with('#') {
            // First non-empty, non-heading, non-applyTo line: prose fallback.
            fallback = Some(trimmed.to_string());
        }
    }

    if !saw_directive {
        if let Some(directive) = fallback {
            bindings.push(PolicyBinding {
                layer,
                source_path: path.to_path_buf(),
                applies_to: vec![default_glob(path, root)],
                directive,
                conflicts_with: Vec::new(),
            });
        }
    }

    bindings
}

/// Strip a leading `- ` or `* ` list prefix, if present.
fn strip_list_prefix(s: &str) -> &str {
    if let Some(rest) = s.strip_prefix("- ") {
        rest
    } else if let Some(rest) = s.strip_prefix("* ") {
        rest
    } else {
        s
    }
}

/// Strip one matching pair of surrounding single or double quotes.
fn strip_surrounding_quotes(s: &str) -> String {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// Default `applies_to` glob for a file: `**` at the root, else
/// `<subdir>/**` for nested files (forward slashes).
fn default_glob(path: &Path, root: &Path) -> String {
    match path.parent() {
        Some(parent) if parent != root => match parent.strip_prefix(root) {
            Ok(rel) if !rel.as_os_str().is_empty() => {
                format!("{}/**", rel.to_string_lossy().replace('\\', "/"))
            }
            _ => "**".to_string(),
        },
        _ => "**".to_string(),
    }
}

/// Collect every policy binding from all instruction files under `root`
/// (FR-003).
///
/// Base layers: `JOEY.md`/`AGENTS.md`/`CLAUDE.md` at the root and both
/// `.cursorrules` and `.github/copilot-instructions.md` map to
/// [`PolicyLayer::Repository`]; nested instruction files map to
/// [`PolicyLayer::Module`]. Bindings are sorted by
/// `(source_path, directive)` for determinism.
pub fn collect_policies(root: &Path) -> Vec<PolicyBinding> {
    let mut bindings: Vec<PolicyBinding> = Vec::new();
    for path in discover(root) {
        let layer = base_layer(&path, root);
        bindings.extend(parse_instruction_file(&path, root, layer));
    }
    bindings.sort_by(|a, b| {
        a.source_path
            .to_string_lossy()
            .cmp(&b.source_path.to_string_lossy())
            .then_with(|| a.directive.cmp(&b.directive))
    });
    bindings
}

/// Base [`PolicyLayer`] for a discovered instruction file.
fn base_layer(path: &Path, root: &Path) -> PolicyLayer {
    let rel = path.strip_prefix(root).unwrap_or(path);
    if rel == Path::new(".cursorrules") || rel == Path::new(".github/copilot-instructions.md") {
        return PolicyLayer::Repository;
    }
    match rel.file_name().and_then(|f| f.to_str()) {
        Some("JOEY.md") | Some("AGENTS.md") | Some("CLAUDE.md") => {
            if rel.parent() == Some(Path::new("")) {
                PolicyLayer::Repository
            } else {
                PolicyLayer::Module
            }
        }
        _ => PolicyLayer::Repository,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn glob_match_vectors() {
        assert!(glob_match("**", "a/b/c.rs"));
        assert!(glob_match("src/**", "src/x/y.rs"));
        assert!(!glob_match("src/**", "lib/x.rs"));
        assert!(glob_match("*.rs", "main.rs"));
        // No-slash pattern also matches the basename.
        assert!(glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("docs/**/*.md", "docs/a/b.md"));
        assert!(!glob_match("docs/**/*.md", "docs/b.md"));
        assert!(glob_match("?bc", "abc"));
        assert!(!glob_match("?bc", "ab"));
        assert!(!glob_match("a*b", "a/b"));
    }

    #[test]
    fn discover_finds_and_sorts_instruction_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("AGENTS.md"), "# Root\n").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/AGENTS.md"), "# Sub\n").unwrap();
        fs::create_dir_all(root.join(".github")).unwrap();
        fs::write(root.join(".github/copilot-instructions.md"), "# Copilot\n").unwrap();
        let found = discover(root);
        let rels: Vec<String> = found
            .iter()
            .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rels,
            vec![
                ".github/copilot-instructions.md".to_string(),
                "AGENTS.md".to_string(),
                "sub/AGENTS.md".to_string(),
            ]
        );
    }

    #[test]
    fn parse_instruction_file_applies_scoped_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = root.join("AGENTS.md");
        fs::write(
            &file,
            "# Title\n- Always run cargo fmt\n- applyTo: \"src/**/*.rs\"\n- Never use unwrap in src\n",
        )
        .unwrap();
        let bindings = parse_instruction_file(&file, root, PolicyLayer::Repository);
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].layer, PolicyLayer::Repository);
        assert_eq!(bindings[0].applies_to, vec!["**".to_string()]);
        assert_eq!(bindings[0].directive, "Always run cargo fmt");
        assert_eq!(bindings[1].layer, PolicyLayer::ScopedRule);
        assert_eq!(bindings[1].applies_to, vec!["src/**/*.rs".to_string()]);
        assert_eq!(bindings[1].directive, "Never use unwrap in src");
    }

    #[test]
    fn parse_instruction_file_prose_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = root.join("AGENTS.md");
        fs::write(&file, "Keep functions short.\n").unwrap();
        let bindings = parse_instruction_file(&file, root, PolicyLayer::Repository);
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].directive, "Keep functions short.");
        assert_eq!(bindings[0].applies_to, vec!["**".to_string()]);
    }

    #[test]
    fn nested_file_default_glob_is_subdir() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("sub")).unwrap();
        let file = root.join("sub/AGENTS.md");
        fs::write(&file, "- Prefer explicit types\n").unwrap();
        let bindings = parse_instruction_file(&file, root, PolicyLayer::Module);
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].applies_to, vec!["sub/**".to_string()]);
    }

    #[test]
    fn collect_policies_maps_layers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("AGENTS.md"), "- Root rule\n").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub/AGENTS.md"), "- Sub rule\n").unwrap();
        let bindings = collect_policies(root);
        assert_eq!(bindings.len(), 2);
        let by_directive = |d: &str| bindings.iter().find(|b| b.directive == d).unwrap();
        assert_eq!(by_directive("Root rule").layer, PolicyLayer::Repository);
        assert_eq!(by_directive("Sub rule").layer, PolicyLayer::Module);
    }
}
