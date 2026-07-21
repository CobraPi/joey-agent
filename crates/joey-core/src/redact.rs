//! Secret redaction for logs and tool output (port of `agent/redact.py` core).
//!
//! Best-effort scrubbing of common credential shapes. Applied before tool
//! output enters the transcript or logs.

use once_cell::sync::Lazy;
use regex::Regex;

struct Pattern {
    re: Regex,
    replacement: &'static str,
}

static PATTERNS: Lazy<Vec<Pattern>> = Lazy::new(|| {
    let specs: &[(&str, &str)] = &[
        // Anthropic keys / OAuth tokens
        (r"sk-ant-[A-Za-z0-9_\-]{20,}", "sk-ant-***REDACTED***"),
        // OpenAI-style keys
        (r"sk-[A-Za-z0-9]{20,}", "sk-***REDACTED***"),
        // OpenRouter
        (r"sk-or-[A-Za-z0-9_\-]{20,}", "sk-or-***REDACTED***"),
        // GitHub tokens
        (r"gh[pousr]_[A-Za-z0-9]{20,}", "gh_***REDACTED***"),
        // AWS access key ids
        (r"AKIA[0-9A-Z]{16}", "AKIA***REDACTED***"),
        // Slack tokens
        (r"xox[baprs]-[A-Za-z0-9\-]{10,}", "xox-***REDACTED***"),
        // Bearer tokens in headers
        (
            r"(?i)(authorization:\s*bearer\s+)[A-Za-z0-9._\-]{12,}",
            "${1}***REDACTED***",
        ),
        // Generic KEY=secret / TOKEN=secret assignments
        (
            r#"(?i)((?:api[_-]?key|secret|token|password)\s*[=:]\s*)["']?[A-Za-z0-9._\-]{12,}["']?"#,
            "${1}***REDACTED***",
        ),
    ];
    specs
        .iter()
        .filter_map(|(pat, rep)| {
            Regex::new(pat).ok().map(|re| Pattern {
                re,
                replacement: rep,
            })
        })
        .collect()
});

/// Scrub secret-shaped substrings from `text`.
pub fn redact_secrets(text: &str) -> String {
    let mut out = text.to_string();
    for p in PATTERNS.iter() {
        out = p.re.replace_all(&out, p.replacement).into_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_anthropic_key() {
        let s = "key is sk-ant-api03-abcdefghijklmnopqrstuvwxyz012345 ok";
        let r = redact_secrets(s);
        assert!(!r.contains("abcdefghijklmnop"));
        assert!(r.contains("REDACTED"));
    }

    #[test]
    fn redacts_env_assignment() {
        let r = redact_secrets("OPENROUTER_API_KEY=sk-or-v1-0123456789abcdef0123");
        assert!(r.contains("REDACTED"));
    }

    #[test]
    fn leaves_plain_text() {
        assert_eq!(redact_secrets("hello world"), "hello world");
    }
}
