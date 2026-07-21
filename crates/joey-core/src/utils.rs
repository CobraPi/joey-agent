//! Small shared utilities (port of upstream `utils.py`).

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The upstream truthy-string set (`utils.TRUTHY_STRINGS`).
pub const TRUTHY_STRINGS: [&str; 4] = ["1", "true", "yes", "on"];

/// Port of upstream `is_truthy_value` for string inputs: lowercased,
/// trimmed membership in [`TRUTHY_STRINGS`].
pub fn is_truthy_str(value: &str) -> bool {
    TRUTHY_STRINGS.contains(&value.trim().to_lowercase().as_str())
}

/// Port of upstream `env_bool(key, default)`.
///
/// Upstream reads `os.getenv(key, "")` — an *unset* variable becomes the
/// empty string, which is not `None`, so `is_truthy_value` never consults
/// `default`: the result is simply "is the value truthy". The `default`
/// parameter is retained for signature fidelity but — exactly like
/// upstream — has no effect.
pub fn env_bool(name: &str, _default: bool) -> bool {
    is_truthy_str(&std::env::var(name).unwrap_or_default())
}

/// Port of upstream `env_var_enabled(name, default="")`: the `default` is a
/// *string* substituted when the variable is unset, then truthiness-tested.
pub fn env_var_enabled(name: &str, default: &str) -> bool {
    is_truthy_str(&std::env::var(name).unwrap_or_else(|_| default.to_string()))
}

/// Loose bool parsing helper retained for config coercion call sites.
pub fn parse_bool(v: &str) -> Option<bool> {
    match v.trim().to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" | "" => Some(false),
        _ => None,
    }
}

pub fn env_int(name: &str, default: i64) -> i64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

pub fn env_float(name: &str, default: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(default)
}

#[cfg(unix)]
fn preserve_owner(path: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.uid(), m.gid()))
}

#[cfg(unix)]
fn restore_owner(path: &Path, owner: Option<(u32, u32)>) {
    if let Some((uid, gid)) = owner {
        let _ = std::os::unix::fs::chown(path, Some(uid), Some(gid));
    }
}

#[cfg(unix)]
fn fsync_dir(dir: &Path) {
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
}

/// Atomically replace the file at `path` with `contents`.
///
/// Port of upstream `atomic_replace` + the `atomic_json_write`/`atomic_yaml_write`
/// write discipline (utils.py:91-208, 252-285):
///   - writes to a temp file in the destination directory,
///   - fsyncs the temp file before rename (+ best-effort directory fsync),
///   - resolves a symlink target first so the symlink survives (the real
///     file is replaced in place),
///   - preserves the destination's permission bits and (on unix) uid/gid,
///   - falls back to copy+fsync+unlink on `EXDEV`/`EBUSY` (cross-device or
///     bind-mounted destinations).
pub fn atomic_replace(path: &Path, contents: &[u8]) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating parent dir for {}", path.display()))?;

    // Resolve symlinks so os.replace targets the real file (upstream #16743).
    let real_target: PathBuf = if path
        .symlink_metadata()
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    };

    let existing_perms = std::fs::metadata(&real_target).ok().map(|m| m.permissions());
    #[cfg(unix)]
    let existing_owner = preserve_owner(&real_target);

    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("creating temp file in {}", dir.display()))?;
    tmp.write_all(contents)?;
    tmp.flush()?;
    // fsync the temp file before rename so the rename never publishes
    // unsynced data (upstream mkstemp + os.fsync discipline).
    tmp.as_file().sync_all().ok();

    let tmp_path = tmp.into_temp_path();
    if let Some(perms) = existing_perms.clone() {
        let _ = std::fs::set_permissions(&tmp_path, perms);
    }

    match std::fs::rename(&tmp_path, &real_target) {
        Ok(()) => {
            // Keep the TempPath from trying to delete the (now-moved) file.
            std::mem::forget(tmp_path);
        }
        Err(err) => {
            // EXDEV (cross-device) / EBUSY fallback: copy + fsync + unlink.
            let exdev = err.raw_os_error() == Some(libc::EXDEV as i32)
                || err.raw_os_error() == Some(libc::EBUSY as i32);
            if !exdev {
                return Err(err)
                    .with_context(|| format!("renaming into place: {}", real_target.display()));
            }
            std::fs::copy(&tmp_path, &real_target)
                .with_context(|| format!("copy fallback to {}", real_target.display()))?;
            if let Ok(f) = std::fs::File::open(&real_target) {
                let _ = f.sync_all();
            }
            // TempPath drop removes the source.
        }
    }

    if let Some(perms) = existing_perms {
        let _ = std::fs::set_permissions(&real_target, perms);
    }
    #[cfg(unix)]
    {
        restore_owner(&real_target, existing_owner);
        fsync_dir(dir);
    }
    Ok(())
}

/// Atomically write pretty-printed JSON to `path` (2-space indent, no
/// trailing newline — matches upstream `json.dump(indent=2)`).
pub fn atomic_json_write<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    atomic_replace(path, &bytes)
}

/// Atomically write YAML to `path`.
///
/// Note: upstream uses a PyYAML `IndentDumper` that indents sequence items
/// two spaces under their parent key; `serde_yaml` emits indentless
/// sequences and offers no knob for this, so the sequence layout differs
/// (both are valid YAML and parse identically).
pub fn atomic_yaml_write<T: serde::Serialize>(path: &Path, value: &T) -> Result<()> {
    let s = serde_yaml::to_string(value)?;
    atomic_replace(path, s.as_bytes())
}

/// Extract the hostname from a base URL, lowercased ("" on parse failure).
/// A trailing dot (FQDN form) is stripped. Used for provider detection.
pub fn base_url_hostname(base_url: &str) -> String {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{}", trimmed)
    };
    match url::Url::parse(&with_scheme) {
        Ok(u) => u
            .host_str()
            .unwrap_or("")
            .trim_end_matches('.')
            .to_lowercase(),
        Err(_) => String::new(),
    }
}

/// Truncate `s` to at most `max_chars` characters, marking elision in the
/// middle so both the head and tail stay visible.
pub fn truncate_middle(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let marker = format!("\n... [{} chars elided] ...\n", count - max_chars);
    let keep = max_chars / 2;
    let head: String = s.chars().take(keep).collect();
    let tail: String = s
        .chars()
        .skip(count.saturating_sub(max_chars - keep))
        .collect();
    format!("{}{}{}", head, marker, tail)
}

/// Truncate the tail of `s` to at most `max_chars` characters with a marker.
pub fn truncate_tail(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_string();
    }
    let head: String = s.chars().take(max_chars).collect();
    format!("{}\n... [truncated {} chars]", head, count - max_chars)
}

/// Rough token estimate: ~4 chars/token with ceiling division, so short
/// non-empty texts never estimate as 0 (port of upstream
/// `agent/model_metadata.estimate_tokens_rough`: `(len(text) + 3) // 4`).
pub fn estimate_tokens(text: &str) -> usize {
    let n = text.chars().count();
    if n == 0 {
        return 0;
    }
    (n + 3) / 4
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_extraction() {
        assert_eq!(base_url_hostname("https://openrouter.ai/api/v1"), "openrouter.ai");
        assert_eq!(base_url_hostname("api.openai.com/v1"), "api.openai.com");
        assert_eq!(base_url_hostname("https://example.com./v1"), "example.com");
        assert_eq!(base_url_hostname(""), "");
    }

    #[test]
    fn env_bool_table() {
        // Upstream getenv(key, "") semantics: unset → False regardless of default.
        std::env::remove_var("JOEY_TEST_EB_UNSET");
        assert!(!env_bool("JOEY_TEST_EB_UNSET", true));
        assert!(!env_bool("JOEY_TEST_EB_UNSET", false));

        for (val, expect) in [
            ("1", true),
            ("true", true),
            ("TRUE", true),
            ("yes", true),
            ("on", true),
            ("0", false),
            ("false", false),
            ("off", false),
            ("no", false),
            ("", false),
            ("2", false),
            ("enabled", false),
        ] {
            std::env::set_var("JOEY_TEST_EB_VAL", val);
            assert_eq!(env_bool("JOEY_TEST_EB_VAL", false), expect, "value {:?}", val);
            assert_eq!(env_bool("JOEY_TEST_EB_VAL", true), expect, "value {:?}", val);
        }
        std::env::remove_var("JOEY_TEST_EB_VAL");

        // env_var_enabled honors a *string* default when unset.
        std::env::remove_var("JOEY_TEST_EB_UNSET2");
        assert!(env_var_enabled("JOEY_TEST_EB_UNSET2", "1"));
        assert!(!env_var_enabled("JOEY_TEST_EB_UNSET2", ""));
    }

    #[test]
    fn estimate_tokens_ceiling() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        assert_eq!(estimate_tokens(&"x".repeat(400)), 100);
    }

    #[test]
    fn atomic_write_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.json");
        atomic_json_write(&p, &serde_json::json!({"a": 1})).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.ends_with('\n'), "no trailing newline (upstream json.dump)");
        let back: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back["a"], 1);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_replace_preserves_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        std::fs::write(&real, "old").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        atomic_replace(&link, b"new").unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink(), "symlink survives");
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
    }
}
