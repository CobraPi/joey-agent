//! Feature 034 (US7) — pass-by-reference payload store (research D2,
//! data-model.md "PersistedPayload"). Plain files under
//! `~/.joey/payloads/<sanitize_key(session_id)>-<fnv1a64(session_id)>/`,
//! mirroring the scratchpad precedent (scratchpad_tool.rs). Private to
//! joey-tools: tools reach it via `crate::payload_store`.

use std::io;
use std::path::{Path, PathBuf};

/// FNV-1a 64-bit of the raw session key (collision-proofing suffix),
/// rendered as 8 hex chars (low 32 bits — the dirname grammar pins an
/// 8-hex-char suffix).
fn fnv1a64(s: &str) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", (hash & 0xffff_ffff) as u32)
}

/// Sanitize a session key to `[A-Za-z0-9._-]`, collapsing runs.
fn sanitize_key(key: &str) -> String {
    let mut out = String::new();
    let mut prev_replaced = false;
    for c in key.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            out.push(c);
            prev_replaced = false;
        } else if !prev_replaced {
            out.push('-');
            prev_replaced = true;
        }
    }
    out
}

/// Per-session payload directory: `~/.joey/payloads/<sanitize>-<fnv1a64>`.
pub fn payloads_dir(session_id: &str) -> PathBuf {
    joey_core::constants::joey_home()
        .join("payloads")
        .join(format!("{}-{}", sanitize_key(session_id), fnv1a64(session_id)))
}

/// Deterministic payload file name: `<tool>-<fnv1a64(content)>.txt`.
fn payload_file_name(tool_name: &str, content: &str) -> String {
    format!("{}-{}.txt", sanitize_key(tool_name), fnv1a64(content))
}

/// Persist `content` for `tool_name` under the session's payload dir and
/// return the resulting file path. Idempotent for identical content
/// (deterministic name, overwrite).
pub fn persist_payload(session_id: &str, tool_name: &str, content: &str) -> io::Result<PathBuf> {
    let dir = payloads_dir(session_id);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(payload_file_name(tool_name, content));
    std::fs::write(&path, content)?;
    Ok(path)
}

/// Read a persisted payload's current disk content.
pub fn read_payload(path: &Path) -> io::Result<String> {
    std::fs::read_to_string(path)
}

/// FR-012: if `path` lives under another session's payload dir, copy it into
/// the current session's dir and return the new path; otherwise return the
/// path unchanged (current session or plain external file).
pub fn copy_payload_into_current_session(path: &Path, current_session_id: &str) -> io::Result<PathBuf> {
    let current_dir = payloads_dir(current_session_id);
    if path.parent() == Some(current_dir.as_path()) {
        return Ok(path.to_path_buf());
    }
    let payloads_root = joey_core::constants::joey_home().join("payloads");
    let under_root = path.parent().map(|p| p.starts_with(&payloads_root)).unwrap_or(false);
    if under_root {
        std::fs::create_dir_all(&current_dir)?;
        let dest = current_dir.join(path.file_name().expect("payload file name"));
        std::fs::copy(path, &dest)?;
        Ok(dest)
    } else {
        Ok(path.to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_key_replaces_unsafe_chars() {
        let san = sanitize_key("a b/c");
        assert!(san.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')));
        assert!(!san.contains('/'));
    }

    #[test]
    fn fnv1a64_is_deterministic_8_hex() {
        let h1 = fnv1a64("a b/c");
        let h2 = fnv1a64("a b/c");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 8);
        assert!(h1.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn fnv1a64_known_single_char() {
        let h = fnv1a64("a");
        assert_eq!(h.len(), 8);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn payload_file_name_is_deterministic() {
        assert_eq!(payload_file_name("terminal", "x"), format!("terminal-{}.txt", fnv1a64("x")));
    }
}
