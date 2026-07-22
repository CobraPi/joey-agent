//! File-safety guards shared by the file tools — ports of:
//! * the device-path blocklist (`tools/file_tools.py:139-148, 442-515`)
//! * the binary-extension set (`tools/binary_extensions.py`)
//! * the credential/internal read block (`agent/file_safety.get_read_block_error`)
//! * the sensitive-write-path refusal (`tools/file_tools.py:569-623`)
//! * the internal-display-text write refusal (`tools/file_tools.py:854-925`)
//! * ANSI stripping (`tools/ansi_strip.py`)

use once_cell::sync::Lazy;
use regex::Regex;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Device path blocklist
// ---------------------------------------------------------------------------

const BLOCKED_DEVICE_PATHS: &[&str] = &[
    // Infinite output — never reach EOF
    "/dev/zero",
    "/dev/random",
    "/dev/urandom",
    "/dev/full",
    // Blocks waiting for input
    "/dev/stdin",
    "/dev/tty",
    "/dev/console",
    // Nonsensical to read
    "/dev/stdout",
    "/dev/stderr",
    // fd aliases
    "/dev/fd/0",
    "/dev/fd/1",
    "/dev/fd/2",
];

const PROC_FD_SUFFIXES: &[&str] = &["/fd/0", "/fd/1", "/fd/2"];
const PROC_LEAK_SUFFIXES: &[&str] = &[
    "/environ",
    "/cmdline",
    "/maps",
    "/smaps",
    "/smaps_rollup",
    "/numa_maps",
    "/mem",
    "/auxv",
    "/pagemap",
];

fn normalize_path_str(path: &str) -> String {
    let expanded = shellexpand::tilde(path).to_string();
    // Lexical normalization (os.path.normpath analog): collapse `.`/`..`.
    let mut parts: Vec<&str> = Vec::new();
    let absolute = expanded.starts_with('/');
    for comp in expanded.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                if !parts.is_empty() && *parts.last().unwrap() != ".." {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    if absolute {
        format!("/{}", joined)
    } else if joined.is_empty() {
        ".".to_string()
    } else {
        joined
    }
}

fn is_blocked_device_path(normalized: &str) -> bool {
    if BLOCKED_DEVICE_PATHS.contains(&normalized) {
        return true;
    }
    if normalized.starts_with("/proc/")
        && PROC_FD_SUFFIXES.iter().any(|s| normalized.ends_with(s))
    {
        return true;
    }
    if normalized.starts_with("/proc/")
        && PROC_LEAK_SUFFIXES.iter().any(|s| normalized.ends_with(s))
    {
        return true;
    }
    false
}

/// Port of `_is_blocked_device`: literal path first, then each symlink hop,
/// then the fully-resolved path.
pub fn is_blocked_device(filepath: &str, base_dir: Option<&Path>) -> bool {
    let mut expanded = shellexpand::tilde(filepath).to_string();
    if let Some(base) = base_dir {
        if !Path::new(&expanded).is_absolute() {
            expanded = base.join(&expanded).to_string_lossy().into_owned();
        }
    }
    let normalized = normalize_path_str(&expanded);
    if is_blocked_device_path(&normalized) {
        return true;
    }

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut current = PathBuf::from(&normalized);
    for _ in 0..20 {
        let Ok(target) = std::fs::read_link(&current) else {
            break;
        };
        let target_abs = if target.is_absolute() {
            target
        } else {
            current.parent().unwrap_or(Path::new("/")).join(target)
        };
        let target_norm = normalize_path_str(&target_abs.to_string_lossy());
        if is_blocked_device_path(&target_norm) {
            return true;
        }
        if !seen.insert(target_norm.clone()) {
            break;
        }
        current = PathBuf::from(target_norm);
    }

    if let Ok(resolved) = std::fs::canonicalize(&normalized) {
        if is_blocked_device_path(&normalize_path_str(&resolved.to_string_lossy())) {
            return true;
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Binary extensions (tools/binary_extensions.py)
// ---------------------------------------------------------------------------

pub const BINARY_EXTENSIONS: &[&str] = &[
    // Images
    ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".ico", ".webp", ".tiff", ".tif",
    // Videos
    ".mp4", ".mov", ".avi", ".mkv", ".webm", ".wmv", ".flv", ".m4v", ".mpeg", ".mpg",
    // Audio
    ".mp3", ".wav", ".ogg", ".flac", ".aac", ".m4a", ".wma", ".aiff", ".opus",
    // Archives
    ".zip", ".tar", ".gz", ".bz2", ".7z", ".rar", ".xz", ".z", ".tgz", ".iso",
    // Executables/binaries
    ".exe", ".dll", ".so", ".dylib", ".bin", ".o", ".a", ".obj", ".lib",
    ".app", ".msi", ".deb", ".rpm",
    // Documents (exclude .pdf — text-based, agents may want to inspect)
    ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".odt", ".ods", ".odp",
    // Fonts
    ".ttf", ".otf", ".woff", ".woff2", ".eot",
    // Bytecode / VM artifacts
    ".pyc", ".pyo", ".class", ".jar", ".war", ".ear", ".node", ".wasm", ".rlib",
    // Database files
    ".sqlite", ".sqlite3", ".db", ".mdb", ".idx",
    // Design / 3D
    ".psd", ".ai", ".eps", ".sketch", ".fig", ".xd", ".blend", ".3ds", ".max",
    // Flash
    ".swf", ".fla",
    // Lock/profiling data
    ".lockb", ".dat", ".data",
];

/// Port of `has_binary_extension` — pure string check, no I/O.
pub fn has_binary_extension(path: &str) -> bool {
    match path.rfind('.') {
        Some(dot) => BINARY_EXTENSIONS.contains(&path[dot..].to_lowercase().as_str()),
        None => false,
    }
}

pub const IMAGE_EXTENSIONS: &[&str] = &[".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".ico"];

pub fn is_image_extension(path: &str) -> bool {
    match path.rfind('.') {
        Some(dot) => IMAGE_EXTENSIONS.contains(&path[dot..].to_lowercase().as_str()),
        None => false,
    }
}

// ---------------------------------------------------------------------------
// Read block (agent/file_safety.get_read_block_error)
// ---------------------------------------------------------------------------

const BLOCKED_PROJECT_ENV_BASENAMES: &[&str] = &[
    ".env",
    ".env.local",
    ".env.development",
    ".env.production",
    ".env.test",
    ".env.staging",
    ".envrc",
];

/// Port of `get_read_block_error` — internal cache / credential-store /
/// project-env read denial. Not a security boundary; defense-in-depth.
pub fn get_read_block_error(path: &str) -> Option<String> {
    let expanded = shellexpand::tilde(path).to_string();
    let resolved = std::fs::canonicalize(&expanded)
        .unwrap_or_else(|_| PathBuf::from(normalize_path_str(&expanded)));

    let joey_dirs: Vec<PathBuf> = {
        let mut dirs = vec![joey_core::constants::joey_home()];
        let root = joey_core::constants::default_root();
        if !dirs.contains(&root) {
            dirs.push(root);
        }
        dirs.into_iter()
            .map(|d| std::fs::canonicalize(&d).unwrap_or(d))
            .collect()
    };

    // Skills .hub: prompt-injection carriers.
    for hd in &joey_dirs {
        for blocked in [hd.join("skills/.hub/index-cache"), hd.join("skills/.hub")] {
            if resolved.starts_with(&blocked) {
                return Some(format!(
                    "Access denied: {} is an internal Joey cache file and cannot be read directly to prevent prompt injection. Use the skills_list or skill_view tools instead.",
                    path
                ));
            }
        }
    }

    // Credential / secret stores.
    let credential_file_names = [
        "auth.json",
        "auth.lock",
        ".anthropic_oauth.json",
        ".env",
        "webhook_subscriptions.json",
        "auth/google_oauth.json",
        "cache/bws_cache.json",
    ];
    for hd in &joey_dirs {
        for name in credential_file_names {
            let blocked = hd.join(name);
            let blocked = std::fs::canonicalize(&blocked).unwrap_or(blocked);
            if resolved == blocked {
                return Some(format!(
                    "Access denied: {} is a Joey credential store and cannot be read directly. Provider tools consume these credentials through internal channels. (Defense-in-depth — not a security boundary; the terminal tool can still bypass.)",
                    path
                ));
            }
        }
    }

    // mcp-tokens/: anything inside is OAuth token material.
    for hd in &joey_dirs {
        let mcp_tokens = hd.join("mcp-tokens");
        let mcp_tokens = std::fs::canonicalize(&mcp_tokens).unwrap_or(mcp_tokens);
        if resolved == mcp_tokens {
            return Some(format!(
                "Access denied: {} is the Joey MCP token directory and cannot be read directly. (Defense-in-depth — not a security boundary; the terminal tool can still bypass.)",
                path
            ));
        }
        if resolved.starts_with(&mcp_tokens) {
            return Some(format!(
                "Access denied: {} is a Joey MCP token file and cannot be read directly. (Defense-in-depth — not a security boundary; the terminal tool can still bypass.)",
                path
            ));
        }
    }

    // Project-local secret-bearing env files anywhere on disk.
    if let Some(name) = resolved.file_name().and_then(|n| n.to_str()) {
        if BLOCKED_PROJECT_ENV_BASENAMES.contains(&name.to_lowercase().as_str()) {
            return Some(format!(
                "Access denied: {} is a secret-bearing environment file and cannot be read to prevent credential leakage. If you need to check the file structure, read .env.example instead. (Defense-in-depth — not a security boundary; the terminal tool can still bypass.)",
                path
            ));
        }
    }

    None
}

// ---------------------------------------------------------------------------
// Sensitive write paths (tools/file_tools.py:569-623)
// ---------------------------------------------------------------------------

const SENSITIVE_PATH_PREFIXES: &[&str] =
    &["/etc/", "/boot/", "/usr/lib/systemd/", "/private/etc/", "/private/var/"];
const SENSITIVE_EXACT_PATHS: &[&str] = &["/var/run/docker.sock", "/run/docker.sock"];

/// Port of `_check_sensitive_path` — returns an error message if the path
/// targets a sensitive system location or the Joey config file.
pub fn check_sensitive_path(filepath: &str, resolved: &Path) -> Option<String> {
    let resolved_str = resolved.to_string_lossy();
    let normalized = normalize_path_str(filepath);
    let err = format!(
        "Refusing to write to sensitive system path: {}\nUse the terminal tool with sudo if you need to modify system files.",
        filepath
    );
    for prefix in SENSITIVE_PATH_PREFIXES {
        if resolved_str.starts_with(prefix) || normalized.starts_with(prefix) {
            return Some(err);
        }
    }
    if SENSITIVE_EXACT_PATHS.contains(&resolved_str.as_ref())
        || SENSITIVE_EXACT_PATHS.contains(&normalized.as_str())
    {
        return Some(err);
    }
    // Prevent agents from modifying the Joey config file directly.
    let config_path = joey_core::constants::config_path();
    let config_resolved = std::fs::canonicalize(&config_path).unwrap_or(config_path);
    let config_str = config_resolved.to_string_lossy();
    if resolved_str == config_str || normalized == config_str {
        return Some(format!(
            "Refusing to write to Joey config file: {}\nAgent cannot modify security-sensitive configuration. Edit ~/.joey/config.yaml directly or use 'joey config' instead.",
            filepath
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// Internal display-text write refusal (tools/file_tools.py:796-925)
// ---------------------------------------------------------------------------

pub const READ_DEDUP_STATUS_MESSAGE: &str = "File unchanged since last read. The content from the earlier read_file result in this conversation is still current — refer to that instead of re-reading.";

fn is_internal_file_status_text(content: &str) -> bool {
    let stripped = content.trim();
    if stripped.is_empty() {
        return false;
    }
    if stripped == READ_DEDUP_STATUS_MESSAGE {
        return true;
    }
    stripped.contains(READ_DEDUP_STATUS_MESSAGE)
        && stripped.len() <= 2 * READ_DEDUP_STATUS_MESSAGE.len()
}

fn looks_like_read_file_line_numbered_content(content: &str) -> bool {
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() < 2 {
        return false;
    }
    let mut numbered: Vec<u64> = Vec::new();
    for line in &lines {
        let stripped = line.trim_start();
        if let Some((prefix, _rest)) = stripped.split_once('|') {
            if !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit()) {
                if let Ok(n) = prefix.parse::<u64>() {
                    numbered.push(n);
                }
            }
        }
    }
    if numbered.len() < 2 {
        return false;
    }
    if (numbered.len() as f64) / (lines.len() as f64) < 0.6 {
        return false;
    }
    let consecutive_pairs = numbered.windows(2).filter(|w| w[1] == w[0] + 1).count();
    consecutive_pairs >= numbered.len() - 1
}

/// Port of `_is_internal_file_tool_content`.
pub fn is_internal_file_tool_content(content: &str) -> bool {
    is_internal_file_status_text(content) || looks_like_read_file_line_numbered_content(content)
}

// ---------------------------------------------------------------------------
// ANSI stripping (tools/ansi_strip.py)
// ---------------------------------------------------------------------------

static ANSI_ESCAPE_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?s)\x1b(?:\[[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]|\][\s\S]*?(?:\x07|\x1b\\)|[PX^_][\s\S]*?(?:\x1b\\)|[\x20-\x2f]+[\x30-\x7e]|[\x30-\x7e])|\u{9b}[\x30-\x3f]*[\x20-\x2f]*[\x40-\x7e]|\u{9d}[\s\S]*?(?:\x07|\u{9c})|[\u{80}-\u{9f}]",
    )
    .unwrap()
});

static HAS_ESCAPE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[\x1b\u{80}-\u{9f}]").unwrap());

/// Port of `strip_ansi`.
pub fn strip_ansi(text: &str) -> String {
    if text.is_empty() || !HAS_ESCAPE.is_match(text) {
        return text.to_string();
    }
    ANSI_ESCAPE_RE.replace_all(text, "").into_owned()
}

/// Port of `has_traversal_component` (tools/path_security.py).
pub fn has_traversal_component(path_str: &str) -> bool {
    Path::new(path_str).components().any(|c| matches!(c, std::path::Component::ParentDir))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_paths_blocked() {
        assert!(is_blocked_device("/dev/stdin", None));
        assert!(is_blocked_device("/dev/urandom", None));
        assert!(is_blocked_device("/proc/self/environ", None));
        assert!(is_blocked_device("/proc/1234/maps", None));
        assert!(is_blocked_device("/proc/self/fd/0", None));
        assert!(!is_blocked_device("/tmp/normal.txt", None));
    }

    #[test]
    fn binary_extension_guard() {
        assert!(has_binary_extension("photo.PNG"));
        assert!(has_binary_extension("doc.docx"));
        assert!(!has_binary_extension("notes.txt"));
        assert!(!has_binary_extension("notebook.ipynb"));
        assert!(!has_binary_extension("paper.pdf"));
        assert!(!has_binary_extension("Makefile"));
    }

    #[test]
    fn env_files_read_blocked() {
        let err = get_read_block_error("/some/project/.env").unwrap();
        assert!(err.contains("secret-bearing environment file"));
        assert!(get_read_block_error("/some/project/.env.example").is_none());
        assert!(get_read_block_error("/some/project/notes.txt").is_none());
    }

    #[test]
    fn sensitive_write_paths() {
        let err = check_sensitive_path("/etc/passwd", Path::new("/etc/passwd")).unwrap();
        assert!(err.starts_with("Refusing to write to sensitive system path: /etc/passwd\n"));
        assert!(check_sensitive_path("/tmp/x", Path::new("/tmp/x")).is_none());
    }

    #[test]
    fn internal_content_detection() {
        assert!(is_internal_file_tool_content(READ_DEDUP_STATUS_MESSAGE));
        assert!(is_internal_file_tool_content(&format!("Note: {}", READ_DEDUP_STATUS_MESSAGE)));
        assert!(is_internal_file_tool_content("1|line one\n2|line two\n3|line three\n"));
        assert!(!is_internal_file_tool_content("1|value\n"));
        assert!(!is_internal_file_tool_content("normal file content\nwith lines\n"));
    }

    #[test]
    fn ansi_stripped() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m plain"), "red plain");
        assert_eq!(strip_ansi("clean"), "clean");
        assert_eq!(strip_ansi("\x1b]0;title\x07body"), "body");
    }

    #[test]
    fn traversal_detection() {
        assert!(has_traversal_component("../etc/passwd"));
        assert!(has_traversal_component("a/../b"));
        assert!(!has_traversal_component("a/b/c"));
    }
}
