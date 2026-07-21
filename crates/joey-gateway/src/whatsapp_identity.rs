//! Shared helpers for canonicalising WhatsApp sender identity (port of
//! upstream `gateway/whatsapp_identity.py`).
//!
//! WhatsApp's bridge can surface the same human under two different JID
//! shapes within a single conversation:
//!
//! - LID form: `999999999999999@lid`
//! - Phone form: `15551234567@s.whatsapp.net`
//!
//! Both the authorisation path and the session-key path need to collapse
//! these aliases to a single stable identity. This module is the single
//! source of truth for that resolution.
//!
//! Public helpers:
//!
//! - [`normalize_whatsapp_identifier`] — strip JID/LID/device/plus syntax
//!   down to the bare numeric identifier.
//! - [`canonical_whatsapp_identifier`] — walk the bridge's
//!   `lid-mapping-*.json` files and return a stable canonical identity
//!   across phone/LID variants.
//! - [`expand_whatsapp_aliases`] — return the full alias set for an
//!   identifier.

use std::collections::{BTreeSet, VecDeque};

/// WhatsApp JIDs are numeric (or plus-prefixed numeric) with optional `@`,
/// `.` and `:` separators (upstream `_SAFE_IDENTIFIER_RE`,
/// `^[A-Za-z0-9@.+\-]+$` with ASCII-only classes).
fn is_safe_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '+' | '-'))
}

/// Strip WhatsApp JID/LID syntax down to its stable numeric identifier.
///
/// Accepts any of the identifier shapes the WhatsApp bridge may emit:
/// `"60123456789@s.whatsapp.net"`, `"60123456789:47@s.whatsapp.net"`,
/// `"60123456789@lid"`, or a bare `"+60123456789"` / `"60123456789"`.
/// Returns just the numeric identifier (`"60123456789"`) suitable for
/// equality comparisons.
///
/// Mirrors upstream exactly: trim, remove the first `+` (anywhere), then
/// take the prefix before the first `:`, then before the first `@`.
pub fn normalize_whatsapp_identifier(value: &str) -> String {
    let trimmed = value.trim();
    let without_plus: String = match trimmed.find('+') {
        Some(idx) => {
            let mut s = String::with_capacity(trimmed.len().saturating_sub(1));
            s.push_str(&trimmed[..idx]);
            s.push_str(&trimmed[idx + 1..]);
            s
        }
        None => trimmed.to_string(),
    };
    let before_colon = without_plus.split(':').next().unwrap_or("");
    let before_at = before_colon.split('@').next().unwrap_or("");
    before_at.to_string()
}

/// Mirror Python's `str(value or "")` for the JSON payload of a
/// `lid-mapping-*.json` file (the bridge writes plain JSON strings; the
/// other shapes are defensive).
fn json_value_to_py_str(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => {
            // Python: `0 or ""` is falsy -> "".
            if n.as_f64() == Some(0.0) {
                String::new()
            } else {
                n.to_string()
            }
        }
        // Python: `True or ""` -> "True"; every other shape here is falsy or
        // unrealistic for a mapping file.
        serde_json::Value::Bool(true) => "True".to_string(),
        _ => String::new(),
    }
}

/// Resolve WhatsApp phone/LID aliases via bridge session mapping files.
///
/// Returns the set of all identifiers transitively reachable through the
/// bridge's `<JOEY_HOME>/whatsapp/session/lid-mapping-*.json` files,
/// starting from `identifier`. The result always includes the normalized
/// input itself (when it passes the safe-identifier gate), so callers can
/// safely membership-check against the return value.
///
/// Returns an empty set if `identifier` normalizes to empty.
pub fn expand_whatsapp_aliases(identifier: &str) -> BTreeSet<String> {
    let normalized = normalize_whatsapp_identifier(identifier);
    let mut resolved: BTreeSet<String> = BTreeSet::new();
    if normalized.is_empty() {
        return resolved;
    }

    let session_dir =
        joey_core::constants::joey_dir("platforms/whatsapp/session", "whatsapp/session");
    let mut queue: VecDeque<String> = VecDeque::new();
    queue.push_back(normalized);

    while let Some(current) = queue.pop_front() {
        if current.is_empty() || resolved.contains(&current) {
            continue;
        }
        // Defense-in-depth: reject identifiers that could sneak path
        // separators / traversal segments into the `lid-mapping-{current}`
        // filename below (mirrors upstream `_SAFE_IDENTIFIER_RE` gate).
        if !is_safe_identifier(&current) {
            continue;
        }

        resolved.insert(current.clone());
        for suffix in ["", "_reverse"] {
            let mapping_path = session_dir.join(format!("lid-mapping-{current}{suffix}.json"));
            if !mapping_path.exists() {
                continue;
            }
            let mapped = match std::fs::read_to_string(&mapping_path) {
                Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(value) => normalize_whatsapp_identifier(&json_value_to_py_str(&value)),
                    Err(err) => {
                        tracing::debug!(
                            "whatsapp_identity: failed to read {}: {}",
                            mapping_path.display(),
                            err
                        );
                        continue;
                    }
                },
                Err(err) => {
                    tracing::debug!(
                        "whatsapp_identity: failed to read {}: {}",
                        mapping_path.display(),
                        err
                    );
                    continue;
                }
            };
            if !mapped.is_empty() && !resolved.contains(&mapped) {
                queue.push_back(mapped);
            }
        }
    }

    resolved
}

/// Return a stable WhatsApp sender identity across phone-JID/LID variants.
///
/// Reads the bridge's `whatsapp/session/lid-mapping-*.json` files, walks
/// the mapping transitively, and picks the shortest (then lexicographically
/// smallest) alias as the canonical identity —
/// `min(aliases, key=lambda c: (len(c), c))` upstream.
/// [`crate::session::build_session_key`] uses this for both WhatsApp DM
/// chat_ids and WhatsApp group participant ids.
///
/// Returns an empty string if `identifier` normalizes to empty. If no
/// mapping files exist yet (fresh bridge install), returns the normalized
/// input unchanged.
pub fn canonical_whatsapp_identifier(identifier: &str) -> String {
    let normalized = normalize_whatsapp_identifier(identifier);
    if normalized.is_empty() {
        return String::new();
    }

    // `expand_whatsapp_aliases` always includes `normalized` itself, so the
    // min below degrades gracefully when no lid-mapping files are present.
    // (The one unreachable-in-practice exception: an identifier failing the
    // safe-identifier gate yields an empty set — upstream would raise a
    // ValueError from min(); the port degrades to the normalized input.)
    let aliases = expand_whatsapp_aliases(&normalized);
    aliases
        .into_iter()
        .min_by(|a, b| {
            a.chars()
                .count()
                .cmp(&b.chars().count())
                .then_with(|| a.cmp(b))
        })
        .unwrap_or(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::lock_home;
    use joey_core::constants::HomeOverrideGuard;

    #[test]
    fn normalize_strips_jid_lid_device_and_plus() {
        assert_eq!(normalize_whatsapp_identifier("60123456789@s.whatsapp.net"), "60123456789");
        assert_eq!(normalize_whatsapp_identifier("999999999999999@lid"), "999999999999999");
        assert_eq!(normalize_whatsapp_identifier("60123456789:47@s.whatsapp.net"), "60123456789");
        assert_eq!(normalize_whatsapp_identifier("+60123456789"), "60123456789");
        assert_eq!(normalize_whatsapp_identifier("60123456789"), "60123456789");
        assert_eq!(normalize_whatsapp_identifier(""), "");
        assert_eq!(normalize_whatsapp_identifier("   "), "");
    }

    #[test]
    fn canonical_without_mapping_returns_normalized() {
        let _l = lock_home();
        let home = tempfile::tempdir().unwrap();
        let _guard = HomeOverrideGuard::new(home.path().to_path_buf());
        assert_eq!(canonical_whatsapp_identifier("60123456789@lid"), "60123456789");
        assert_eq!(canonical_whatsapp_identifier(""), "");
    }

    #[test]
    fn canonical_walks_lid_mapping_files() {
        let _l = lock_home();
        let home = tempfile::tempdir().unwrap();
        let mapping_dir = home.path().join("whatsapp").join("session");
        std::fs::create_dir_all(&mapping_dir).unwrap();
        std::fs::write(
            mapping_dir.join("lid-mapping-999999999999999.json"),
            serde_json::to_string("15551234567@s.whatsapp.net").unwrap(),
        )
        .unwrap();
        let _guard = HomeOverrideGuard::new(home.path().to_path_buf());

        assert_eq!(canonical_whatsapp_identifier("999999999999999@lid"), "15551234567");
        assert_eq!(
            canonical_whatsapp_identifier("15551234567@s.whatsapp.net"),
            "15551234567"
        );
        let aliases = expand_whatsapp_aliases("999999999999999@lid");
        assert!(aliases.contains("999999999999999"));
        assert!(aliases.contains("15551234567"));
    }
}
