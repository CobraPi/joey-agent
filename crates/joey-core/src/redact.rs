//! Regex-based secret redaction for logs and tool output.
//!
//! Port of upstream `agent/redact.py`: applies pattern matching to mask API
//! keys, tokens, and credentials before they reach log files, verbose output,
//! or gateway logs.
//!
//! Short tokens (< 18 chars) are fully masked. Longer tokens preserve the
//! first 6 and last 4 characters for debuggability.

use fancy_regex::{Captures, Regex};
use once_cell::sync::Lazy;

/// Sensitive query-string parameter names (case-insensitive exact match).
pub const SENSITIVE_QUERY_PARAMS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "token",
    "api_key",
    "apikey",
    "client_secret",
    "password",
    "auth",
    "jwt",
    "session",
    "secret",
    "key",
    "code",            // OAuth authorization codes
    "signature",       // pre-signed URL signatures
    "x-amz-signature",
];

/// Sensitive form-urlencoded / JSON body key names (case-insensitive exact
/// match — "token_count" and "session_id" must NOT match).
pub const SENSITIVE_BODY_KEYS: &[&str] = &[
    "access_token",
    "refresh_token",
    "id_token",
    "token",
    "api_key",
    "apikey",
    "client_secret",
    "password",
    "auth",
    "jwt",
    "secret",
    "private_key",
    "authorization",
    "key",
];

/// Kill switch, snapshotted at first use so runtime env mutations (e.g. an
/// LLM-generated `export JOEY_REDACT_SECRETS=false`) cannot disable
/// redaction mid-session. ON by default. Users opt out via
/// `security.redact_secrets: false` in config.yaml (bridged to this env var
/// by the entrypoints) or `JOEY_REDACT_SECRETS=false` in `~/.joey/.env`.
static REDACT_ENABLED: Lazy<bool> = Lazy::new(|| {
    matches!(
        std::env::var("JOEY_REDACT_SECRETS")
            .unwrap_or_else(|_| "true".to_string())
            .to_lowercase()
            .as_str(),
        "1" | "true" | "yes" | "on"
    )
});

/// Whether redaction is globally enabled (snapshot value).
pub fn redaction_enabled() -> bool {
    *REDACT_ENABLED
}

// Known API key prefixes — match the prefix + contiguous token chars.
const PREFIX_PATTERNS: &[&str] = &[
    r"sk-[A-Za-z0-9_-]{10,}",        // OpenAI / OpenRouter / Anthropic (sk-ant-*)
    r"ghp_[A-Za-z0-9]{10,}",         // GitHub PAT (classic)
    r"github_pat_[A-Za-z0-9_]{10,}", // GitHub PAT (fine-grained)
    r"gho_[A-Za-z0-9]{10,}",         // GitHub OAuth access token
    r"ghu_[A-Za-z0-9]{10,}",         // GitHub user-to-server token
    r"ghs_[A-Za-z0-9]{10,}",         // GitHub server-to-server token
    r"ghr_[A-Za-z0-9]{10,}",         // GitHub refresh token
    r"xapp-\d+-[A-Za-z0-9-]{10,}",   // Slack app-level token
    r"xox[baprs]-[A-Za-z0-9-]{10,}", // Slack bot/app/user tokens
    r"AIza[A-Za-z0-9_-]{30,}",       // Google API keys
    r"pplx-[A-Za-z0-9]{10,}",        // Perplexity
    r"fal_[A-Za-z0-9_-]{10,}",       // Fal.ai
    r"fc-[A-Za-z0-9]{10,}",          // Firecrawl
    r"bb_live_[A-Za-z0-9_-]{10,}",   // BrowserBase
    r"gAAAA[A-Za-z0-9_=-]{20,}",     // Codex encrypted tokens
    r"AKIA[A-Z0-9]{16}",             // AWS Access Key ID
    r"sk_live_[A-Za-z0-9]{10,}",     // Stripe secret key (live)
    r"sk_test_[A-Za-z0-9]{10,}",     // Stripe secret key (test)
    r"rk_live_[A-Za-z0-9]{10,}",     // Stripe restricted key
    r"SG\.[A-Za-z0-9_-]{10,}",       // SendGrid API key
    r"hf_[A-Za-z0-9]{10,}",          // HuggingFace token
    r"r8_[A-Za-z0-9]{10,}",          // Replicate API token
    r"npm_[A-Za-z0-9]{10,}",         // npm access token
    r"pypi-[A-Za-z0-9_-]{10,}",      // PyPI API token
    r"dop_v1_[A-Za-z0-9]{10,}",      // DigitalOcean PAT
    r"doo_v1_[A-Za-z0-9]{10,}",      // DigitalOcean OAuth
    r"am_[A-Za-z0-9_-]{10,}",        // AgentMail API key
    r"sk_[A-Za-z0-9_]{10,}",         // ElevenLabs TTS key (sk_ underscore, not sk- dash)
    r"tvly-[A-Za-z0-9]{10,}",        // Tavily search API key
    r"exa_[A-Za-z0-9]{10,}",         // Exa search API key
    r"gsk_[A-Za-z0-9]{10,}",         // Groq Cloud API key
    r"syt_[A-Za-z0-9]{10,}",         // Matrix access token
    r"retaindb_[A-Za-z0-9]{10,}",    // RetainDB API key
    r"hsk-[A-Za-z0-9]{10,}",         // Hindsight API key
    r"mem0_[A-Za-z0-9]{10,}",        // Mem0 Platform API key
    r"brv_[A-Za-z0-9]{10,}",         // ByteRover API key
    r"xai-[A-Za-z0-9]{30,}",         // xAI (Grok) API key
    r"ntn_[A-Za-z0-9]{10,}",         // Notion internal integration token
    r"fw-[A-Za-z0-9]{30,}",          // Fireworks AI API key
    r"fw_[A-Za-z0-9]{30,}",          // Fireworks AI API key
    r"fpk_[A-Za-z0-9]{30,}",         // Fireworks AI project key
];

fn rx(pattern: &str) -> Regex {
    Regex::new(pattern).expect("redaction pattern must compile")
}

static PREFIX_RE: Lazy<Regex> = Lazy::new(|| {
    rx(&format!(
        r"(?<![A-Za-z0-9_-])({})(?![A-Za-z0-9_-])",
        PREFIX_PATTERNS.join("|")
    ))
});

/// Return the leading literal characters of a regex pattern — the substring
/// any match MUST contain, used for cheap pre-screening.
fn extract_literal_prefix(pattern: &str) -> String {
    let meta = ['[', '(', '\\', '.', '?', '*', '+', '|', '{', '^', '$'];
    match pattern.find(|c| meta.contains(&c)) {
        Some(i) => pattern[..i].to_string(),
        None => pattern.to_string(),
    }
}

static PREFIX_SUBSTRINGS: Lazy<Vec<String>> =
    Lazy::new(|| PREFIX_PATTERNS.iter().map(|p| extract_literal_prefix(p)).collect());

fn has_known_prefix_substring(text: &str) -> bool {
    PREFIX_SUBSTRINGS.iter().any(|p| text.contains(p.as_str()))
}

// ENV assignment patterns: KEY=value where KEY contains a secret-like name.
const SECRET_ENV_NAMES: &str = r"(?:API_?KEY|TOKEN|SECRET|PASSWORD|PASSWD|CREDENTIAL|AUTH)";
static ENV_ASSIGN_RE: Lazy<Regex> = Lazy::new(|| {
    rx(&format!(
        r#"([A-Z0-9_]{{0,50}}{SECRET_ENV_NAMES}[A-Z0-9_]{{0,50}})\s*=\s*(['"]?)(\S+)\2"#
    ))
});

// Lowercase / dotted / hyphenated config keys from config files.
const SECRET_CFG_NAMES: &str = r"(?:api[ _.\-]?key|token|secret|passwd|password|credential|auth)";
const CFG_VALUE: &str = r#"(['"]?)([^\s&]+?)\2(?=[\s&]|$)"#;

// Programmatic env lookups reference variable *names*, not secret values.
static ENV_LOOKUP_VALUE_RE: Lazy<Regex> =
    Lazy::new(|| rx(r"^(?:os\.(?:getenv|environ)|process\.env|\$ENV\{)"));

// Namespaced (dotted) key: the secret word may sit anywhere in a dotted path.
static CFG_DOTTED_RE: Lazy<Regex> = Lazy::new(|| {
    rx(&format!(
        r"(?i)((?:[A-Za-z0-9_\-]+\.)+[A-Za-z0-9_.\-]*{SECRET_CFG_NAMES}[A-Za-z0-9_.\-]*|[A-Za-z0-9_.\-]*{SECRET_CFG_NAMES}[A-Za-z0-9_.\-]*\.[A-Za-z0-9_.\-]+)={CFG_VALUE}"
    ))
});

// Line-anchored bare key: `password=…` / `export api_key=…` at start of line.
static CFG_ANCHORED_RE: Lazy<Regex> = Lazy::new(|| {
    rx(&format!(
        r"(?im)(^[ \t]*(?:export[ \t]+)?[A-Za-z0-9_\-]*{SECRET_CFG_NAMES}[A-Za-z0-9_\-]*)={CFG_VALUE}"
    ))
});

// Unquoted YAML / colon config (`password: secret`).
const YAML_CFG_NAMES: &str = r"(?:api[ _.\-]?key|token|secret|passwd|password|credential)";
static YAML_ASSIGN_RE: Lazy<Regex> = Lazy::new(|| {
    rx(&format!(
        r#"(?im)(^[ \t]*[A-Za-z0-9_.\-]*{YAML_CFG_NAMES}[A-Za-z0-9_.\-]*)(:[ \t]*)(?!['"])([^\s&]+)"#
    ))
});

// JSON field patterns: "apiKey": "value", "token": "value", etc.
const JSON_KEY_NAMES: &str = r"(?:api_?[Kk]ey|token|secret|password|access_token|refresh_token|auth_token|bearer|secret_value|raw_secret|secret_input|key_material)";
static JSON_FIELD_RE: Lazy<Regex> =
    Lazy::new(|| rx(&format!(r#"(?i)("{JSON_KEY_NAMES}")\s*:\s*"([^"]+)""#)));

// Authorization headers — any scheme (Bearer, Basic, Token, Digest, …) plus
// the bare-credential form, and Proxy-Authorization.
static AUTH_HEADER_RE: Lazy<Regex> =
    Lazy::new(|| rx(r#"(?i)((?:Proxy-)?Authorization:\s*)([A-Za-z][\w.+-]*\s+)?([^\s"']+)"#));

// API-key style auth headers carrying a single opaque value (no scheme word).
const SECRET_HEADER_NAMES: &str =
    r"(?:x-api-key|x-goog-api-key|api-key|apikey|x-api-token|x-auth-token|x-access-token)";
static SECRET_HEADER_RE: Lazy<Regex> =
    Lazy::new(|| rx(&format!(r"(?i)({SECRET_HEADER_NAMES}\s*:\s*)(\S+)")));

// Telegram bot tokens: bot<digits>:<token> or <digits>:<token>.
static TELEGRAM_RE: Lazy<Regex> = Lazy::new(|| rx(r"(bot)?(\d{8,}):([-A-Za-z0-9_]{30,})"));

// Private key blocks.
static PRIVATE_KEY_RE: Lazy<Regex> =
    Lazy::new(|| rx(r"-----BEGIN[A-Z ]*PRIVATE KEY-----[\s\S]*?-----END[A-Z ]*PRIVATE KEY-----"));

// Database connection strings: protocol://user:PASSWORD@host.
static DB_CONNSTR_RE: Lazy<Regex> = Lazy::new(|| {
    rx(r"(?i)((?:postgres(?:ql)?|mysql|mongodb(?:\+srv)?|redis|amqp)://[^:\s]+:)([^@\s]+)(@)")
});

// Bare-token credential in a web/transport URL: `scheme://TOKEN@host`.
static URL_BARE_TOKEN_RE: Lazy<Regex> = Lazy::new(|| {
    rx(r"(?i)((?:https?|wss?|git|ssh|ftp|ftps|sftp)://)([^\s:@/]{8,})(@[^\s]+)")
});

// JWT tokens: header.payload[.signature] — always start with "eyJ".
static JWT_RE: Lazy<Regex> =
    Lazy::new(|| rx(r"eyJ[A-Za-z0-9_-]{10,}(?:\.[A-Za-z0-9_=-]{4,}){0,2}"));

// E.164 phone numbers: +<country><number>, 7-15 digits.
static SIGNAL_PHONE_RE: Lazy<Regex> = Lazy::new(|| rx(r"(\+[1-9]\d{6,14})(?![A-Za-z0-9])"));

// URLs containing query strings.
static URL_WITH_QUERY_RE: Lazy<Regex> =
    Lazy::new(|| rx(r"(https?|wss?|ftp)://([^\s/?#]+)([^\s?#]*)\?([^\s#]+)(#\S*)?"));

// URLs containing userinfo — `scheme://user:password@host` for ANY scheme.
static URL_USERINFO_RE: Lazy<Regex> =
    Lazy::new(|| rx(r"(https?|wss?|ftp)://([^/\s:@]+):([^/\s@]+)@"));

// Strict provider-egress URL redaction.
static STRICT_URL_PARAM_RE: Lazy<Regex> =
    Lazy::new(|| rx(r#"([?#&;])([A-Za-z0-9_.~+%\-]+)=([^#&;\s"'<>]*)"#));
static STRICT_URL_USERINFO_RE: Lazy<Regex> =
    Lazy::new(|| rx(r"((?:[A-Za-z][A-Za-z0-9+.-]*:)?//)([^/\s?#@]+)@"));

// HTTP access-log request targets: `"POST /webhook?password=... HTTP/1.1"`.
static HTTP_REQUEST_TARGET_QUERY_RE: Lazy<Regex> = Lazy::new(|| {
    rx(r#"(?i)\b((?:GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS|TRACE|CONNECT)\s+[^ \t\r\n"']*?)\?([^ \t\r\n"']+)"#)
});

// Form-urlencoded body detection (whole-text k=v&k=v).
static FORM_BODY_RE: Lazy<Regex> = Lazy::new(|| {
    rx(r"^[A-Za-z_][A-Za-z0-9_.-]*=[^&\s]*(?:&[A-Za-z_][A-Za-z0-9_.-]*=[^&\s]*)+$")
});

fn group<'a>(caps: &'a Captures<'_>, i: usize) -> &'a str {
    caps.get(i).map(|m| m.as_str()).unwrap_or("")
}

fn sub_all(re: &Regex, text: &str, mut f: impl FnMut(&Captures) -> String) -> String {
    re.replace_all(text, |caps: &Captures| f(caps)).into_owned()
}

/// Mask a secret for display, preserving `head` and `tail` characters.
///
/// Canonical display-time redaction helper (`config`, `status`, `dump`, …).
/// Values shorter than `floor` are fully masked (returns `placeholder`);
/// falsy input returns `empty`.
pub fn mask_secret(value: &str, head: usize, tail: usize, floor: usize, placeholder: &str, empty: &str) -> String {
    if value.is_empty() {
        return empty.to_string();
    }
    let chars: Vec<char> = value.chars().collect();
    if chars.len() < floor {
        return placeholder.to_string();
    }
    let head_s: String = chars.iter().take(head).collect();
    let tail_s: String = chars[chars.len().saturating_sub(tail)..].iter().collect();
    format!("{}...{}", head_s, tail_s)
}

/// `mask_secret` with the upstream defaults (head 4, tail 4, floor 12, "***").
pub fn mask_secret_default(value: &str) -> String {
    mask_secret(value, 4, 4, 12, "***", "")
}

/// Mask a log token — conservative 18-char floor, preserves 6 prefix / 4 suffix.
fn mask_token(token: &str) -> String {
    if token.is_empty() {
        return "***".to_string();
    }
    mask_secret(token, 6, 4, 18, "***", "")
}

/// Redact a prefix-matched credential to a NON-REUSABLE sentinel.
///
/// The vendor prefix label is preserved for debuggability, never any of the
/// secret body: `«redacted:ghp_…»`. Used for file-read content so an agent
/// cannot write a truncated mask back as a "real" key.
fn mask_token_nonreusable(token: &str) -> String {
    if token.is_empty() {
        return "«redacted-secret»".to_string();
    }
    for sub in PREFIX_SUBSTRINGS.iter() {
        if !sub.is_empty() && token.starts_with(sub.as_str()) {
            return format!("«redacted:{}…»", sub);
        }
    }
    "«redacted-secret»".to_string()
}

/// Redact sensitive parameter values in a URL query string (`k=v&k=v`).
fn redact_query_string(query: &str) -> String {
    if query.is_empty() {
        return query.to_string();
    }
    let mut parts = Vec::new();
    for pair in query.split('&') {
        match pair.split_once('=') {
            None => parts.push(pair.to_string()),
            Some((key, _value)) => {
                if SENSITIVE_QUERY_PARAMS.contains(&key.to_lowercase().as_str()) {
                    parts.push(format!("{}=***", key));
                } else {
                    parts.push(pair.to_string());
                }
            }
        }
    }
    parts.join("&")
}

/// Scan text for URLs with query strings and redact sensitive params.
pub fn redact_url_query_params(text: &str) -> String {
    sub_all(&URL_WITH_QUERY_RE, text, |caps| {
        format!(
            "{}://{}{}?{}{}",
            group(caps, 1),
            group(caps, 2),
            group(caps, 3),
            redact_query_string(group(caps, 4)),
            group(caps, 5),
        )
    })
}

/// Strip `user:password@` from HTTP/WS/FTP URLs.
pub fn redact_url_userinfo(text: &str) -> String {
    sub_all(&URL_USERINFO_RE, text, |caps| {
        format!("{}://{}:***@", group(caps, 1), group(caps, 2))
    })
}

/// Redact sensitive query params in HTTP access-log request targets.
pub fn redact_http_request_target_query_params(text: &str) -> String {
    sub_all(&HTTP_REQUEST_TARGET_QUERY_RE, text, |caps| {
        format!("{}?{}", group(caps, 1), redact_query_string(group(caps, 2)))
    })
}

fn percent_decode_once(s: &str) -> String {
    // unquote_plus: '+' → ' ', %XX → byte. Invalid escapes pass through.
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit() =>
            {
                let hex = &s[i + 1..i + 3];
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Decode a URL parameter name for bounded, case-insensitive matching.
fn canonical_url_param_name(name: &str) -> String {
    let mut decoded = name.to_string();
    for _ in 0..3 {
        let next = percent_decode_once(&decoded);
        if next == decoded {
            break;
        }
        decoded = next;
    }
    decoded.to_lowercase().replace('-', "_")
}

/// Redact credentials from absolute, relative, and network URL references.
/// Stricter than display/log redaction — used only at secret-egress boundaries.
fn redact_strict_url_credentials(text: &str) -> String {
    let text = sub_all(&STRICT_URL_PARAM_RE, text, |caps| {
        if !SENSITIVE_QUERY_PARAMS.contains(&canonical_url_param_name(group(caps, 2)).as_str()) {
            return group(caps, 0).to_string();
        }
        format!("{}{}=***", group(caps, 1), group(caps, 2))
    });
    sub_all(&STRICT_URL_USERINFO_RE, &text, |caps| {
        let userinfo = group(caps, 2);
        match userinfo.split_once(':') {
            Some((username, _password)) => format!("{}{}:***@", group(caps, 1), username),
            None => format!("{}***@", group(caps, 1)),
        }
    })
}

/// Mask secrets in a CDP/browser endpoint URL before it is logged.
pub fn redact_cdp_url(value: &str) -> String {
    let text = redact_sensitive_text(value);
    if text.is_empty() {
        return text;
    }
    redact_url_userinfo(&redact_url_query_params(&text))
}

/// Redact sensitive values in a form-urlencoded body (whole-text `k=v&k=v`).
fn redact_form_body(text: &str) -> String {
    if text.is_empty() || text.contains('\n') || !text.contains('&') {
        return text.to_string();
    }
    let trimmed = text.trim();
    if !FORM_BODY_RE.is_match(trimmed).unwrap_or(false) {
        return text.to_string();
    }
    redact_query_string(trimmed)
}

/// Options for [`redact_sensitive_text_opts`].
#[derive(Debug, Clone, Copy, Default)]
pub struct RedactOptions {
    /// Redact even when the global kill switch is off (safety boundaries).
    pub force: bool,
    /// Skip the ENV-assignment / JSON-field / YAML passes (source code —
    /// avoids `MAX_TOKENS=…`, `"apiKey": "test"` false positives).
    pub code_file: bool,
    /// File *content* returned to the agent: prefix-matched credentials use
    /// the non-reusable `«redacted:…»` sentinel. Implies `code_file`.
    pub file_read: bool,
    /// Additionally redact credential-named query params and `user:pass@`
    /// userinfo at non-navigation egress boundaries.
    pub redact_url_credentials: bool,
}

/// Apply all redaction patterns to a block of text with default options.
pub fn redact_sensitive_text(text: &str) -> String {
    redact_sensitive_text_opts(text, RedactOptions::default())
}

/// Apply all redaction patterns to a block of text.
///
/// Safe to call on any string — non-matching text passes through unchanged.
/// Enabled by default; disable via `security.redact_secrets: false` /
/// `JOEY_REDACT_SECRETS=false` (snapshotted at init).
pub fn redact_sensitive_text_opts(text: &str, opts: RedactOptions) -> String {
    if text.is_empty() {
        return text.to_string();
    }
    if !(opts.force || *REDACT_ENABLED) {
        return text.to_string();
    }

    // file_read content shouldn't hit the source-code ENV/JSON
    // false-positive paths either (it's config/data, not log lines).
    let code_file = opts.code_file || opts.file_read;

    let mut text = text.to_string();

    // Known prefixes (sk-, ghp_, etc.) — gate on substring presence.
    if has_known_prefix_substring(&text) {
        let file_read = opts.file_read;
        text = sub_all(&PREFIX_RE, &text, |caps| {
            let tok = group(caps, 1);
            if file_read {
                mask_token_nonreusable(tok)
            } else {
                mask_token(tok)
            }
        });
    }

    if !code_file {
        // ENV assignments: OPENAI_API_KEY=***
        if text.contains('=') {
            let redact_env = |caps: &Captures| -> String {
                let (name, quote, value) = (group(caps, 1), group(caps, 2), group(caps, 3));
                // Programmatic env lookups reference variable *names*, not
                // secret values — leave code snippets intact.
                if ENV_LOOKUP_VALUE_RE.is_match(value).unwrap_or(false) {
                    return group(caps, 0).to_string();
                }
                format!("{}={}{}{}", name, quote, mask_token(value), quote)
            };
            text = sub_all(&ENV_ASSIGN_RE, &text, redact_env);
            // Lowercase/dotted config keys. Skip URLs entirely — web-URL
            // query params are intentionally passed through.
            if !text.contains("://") {
                text = sub_all(&CFG_DOTTED_RE, &text, redact_env);
                text = sub_all(&CFG_ANCHORED_RE, &text, redact_env);
            }
        }

        // JSON fields: "apiKey": "***"
        if text.contains(':') && text.contains('"') {
            text = sub_all(&JSON_FIELD_RE, &text, |caps| {
                let (key, value) = (group(caps, 1), group(caps, 2));
                if ENV_LOOKUP_VALUE_RE.is_match(value).unwrap_or(false) {
                    return group(caps, 0).to_string();
                }
                format!("{}: \"{}\"", key, mask_token(value))
            });
        }

        // Unquoted YAML / colon config: password: ***
        if text.contains(':') && !text.contains("://") {
            text = sub_all(&YAML_ASSIGN_RE, &text, |caps| {
                let (key, sep, value) = (group(caps, 1), group(caps, 2), group(caps, 3));
                if ENV_LOOKUP_VALUE_RE.is_match(value).unwrap_or(false) {
                    return group(caps, 0).to_string();
                }
                format!("{}{}{}", key, sep, mask_token(value))
            });
        }
    }

    // Authorization headers — any scheme, case-insensitive.
    if text.contains("uthorization") || text.contains("UTHORIZATION") {
        text = sub_all(&AUTH_HEADER_RE, &text, |caps| {
            format!("{}{}{}", group(caps, 1), group(caps, 2), mask_token(group(caps, 3)))
        });
    }

    // API-key style headers (x-api-key, api-key, …).
    if text.contains(':') {
        text = sub_all(&SECRET_HEADER_RE, &text, |caps| {
            format!("{}{}", group(caps, 1), mask_token(group(caps, 2)))
        });
    }

    // Telegram bot tokens.
    if text.contains(':') {
        text = sub_all(&TELEGRAM_RE, &text, |caps| {
            format!("{}{}:***", group(caps, 1), group(caps, 2))
        });
    }

    // Private key blocks.
    if text.contains("BEGIN") && text.contains("-----") {
        text = sub_all(&PRIVATE_KEY_RE, &text, |_| "[REDACTED PRIVATE KEY]".to_string());
    }

    // Database connection string passwords + bare-token URL userinfo.
    if text.contains("://") {
        if code_file {
            // A pure `{...}` password group is an f-string template
            // reference, not a literal credential — preserve it.
            text = sub_all(&DB_CONNSTR_RE, &text, |caps| {
                let pw = group(caps, 2);
                if pw.starts_with('{') && pw.ends_with('}') {
                    return group(caps, 0).to_string();
                }
                format!("{}***{}", group(caps, 1), group(caps, 3))
            });
        } else {
            text = sub_all(&DB_CONNSTR_RE, &text, |caps| {
                format!("{}***{}", group(caps, 1), group(caps, 3))
            });
        }

        text = sub_all(&URL_BARE_TOKEN_RE, &text, |caps| {
            format!("{}{}{}", group(caps, 1), mask_token(group(caps, 2)), group(caps, 3))
        });
    }

    // JWT tokens (eyJ… — base64-encoded JSON headers).
    if text.contains("eyJ") {
        text = sub_all(&JWT_RE, &text, |caps| mask_token(group(caps, 0)));
    }

    // NOTE: Web-URL redaction (query params + userinfo + access-log request
    // targets) is intentionally OFF by default — magic-link / OAuth-callback
    // workflows must survive. Known credential shapes inside URLs are still
    // caught above.
    if opts.redact_url_credentials {
        text = redact_strict_url_credentials(&text);
    }

    // Form-urlencoded bodies (only triggers on clean k=v&k=v inputs).
    if text.contains('&') && text.contains('=') {
        text = redact_form_body(&text);
    }

    // E.164 phone numbers (Signal, WhatsApp).
    if text.contains('+') {
        text = sub_all(&SIGNAL_PHONE_RE, &text, |caps| {
            let phone = group(caps, 1);
            let chars: Vec<char> = phone.chars().collect();
            if chars.len() <= 8 {
                format!(
                    "{}****{}",
                    chars[..2].iter().collect::<String>(),
                    chars[chars.len() - 2..].iter().collect::<String>()
                )
            } else {
                format!(
                    "{}****{}",
                    chars[..4].iter().collect::<String>(),
                    chars[chars.len() - 4..].iter().collect::<String>()
                )
            }
        });
    }

    text
}

/// Backward-compatible entry point used across the workspace: redact with
/// default options (log/display context).
pub fn redact_secrets(text: &str) -> String {
    redact_sensitive_text(text)
}

// Commands whose stdout is an environment-variable dump (KEY=value lines).
const ENV_DUMP_COMMANDS: &[&str] = &["env", "printenv", "set", "export", "declare"];

/// Return true if `command` dumps environment variables to stdout.
///
/// Detects `env`/`printenv`/`set`/`export`/`declare` as the first token of
/// any segment in a pipeline or sequence (`;` / `&&` / `||` / `|`).
pub fn is_env_dump_command(command: &str) -> bool {
    if command.is_empty() {
        return false;
    }
    for seg in command.split(|c| c == '|' || c == ';' || c == '&') {
        let seg = seg.trim();
        if seg.is_empty() {
            continue;
        }
        let tokens: Vec<String> = shlex::split(seg)
            .unwrap_or_else(|| seg.split_whitespace().map(str::to_string).collect());
        if let Some(first) = tokens.first() {
            if ENV_DUMP_COMMANDS.contains(&first.as_str()) {
                return true;
            }
        }
    }
    false
}

/// Redact secrets from terminal/process stdout.
///
/// Env-dump commands get the full ENV-assignment pass (`code_file=false`);
/// everything else uses `code_file=true` to avoid mangling source dumps.
pub fn redact_terminal_output(output: &str, command: Option<&str>, force: bool) -> String {
    if output.is_empty() {
        return output.to_string();
    }
    let code_file = !is_env_dump_command(command.unwrap_or(""));
    redact_sensitive_text_opts(
        output,
        RedactOptions {
            force,
            code_file,
            ..Default::default()
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn redact(s: &str) -> String {
        redact_sensitive_text_opts(s, RedactOptions { force: true, ..Default::default() })
    }

    #[test]
    fn prefix_family_masks_head6_tail4() {
        let key = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789";
        let r = redact(&format!("key is {} ok", key));
        assert_eq!(r, "key is sk-ant...6789 ok");

        let r = redact("token ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ123456 done");
        assert_eq!(r, "token ghp_AB...3456 done");

        // Short prefix-matched token (< 18 chars) fully masked.
        let r = redact("k = sk-abcdefghij x");
        assert_eq!(r, "k = *** x");

        // Boundary guard: embedded in an identifier — no match.
        let s = "notsk-abcdefghijklmnopqrstuvwx";
        assert_eq!(redact(s), s);
    }

    #[test]
    fn prefix_table() {
        for tok in [
            "AKIAABCDEFGHIJKLMNOP",
            "xoxb-123456789-abcdefghijk",
            "AIzaSyA-1234567890abcdefghijklmnopqrstu",
            "hf_abcdefghijklmnopqrstuv",
            "github_pat_ABCdef1234567890_ghijkl",
            "tvly-abcdefghijklmnop",
            "gsk_abcdefghijklmnopqrst",
            "xai-abcdefghijklmnopqrstuvwxyz0123456789",
            "SG.abcdefghijk-lmnopqrstuv",
            "npm_abcdefghijklmnop",
        ] {
            let r = redact(&format!("a {} b", tok));
            assert_ne!(r, format!("a {} b", tok), "should redact {}", tok);
            assert!(!r.contains(tok), "must not contain {}", tok);
        }
    }

    #[test]
    fn env_assignment_families() {
        // Uppercase env assignment. The ENV pass masks to head6/tail4, then
        // the anchored config-key pass re-masks the 13-char mask below its
        // 18-char floor — verified against upstream: the final output is ***.
        let r = redact("OPENROUTER_API_KEY=abc123def456ghi789jkl0");
        assert_eq!(r, "OPENROUTER_API_KEY=***");

        // Spaces around '=' collapse (replacement rebuilds from groups).
        let r = redact("MY_SECRET = supersecretvalue123456");
        assert_eq!(r, "MY_SECRET=***");

        // Quoted value keeps quotes.
        let r = redact("DB_PASSWORD=\"hunter2hunter2hunter2\"");
        assert_eq!(r, "DB_PASSWORD=\"***\"");

        // Programmatic env lookup left intact.
        let s = "API_KEY=os.getenv('OPENAI_API_KEY')";
        assert_eq!(redact(s), s);

        // Dotted config key.
        let r = redact("spring.datasource.password=hunter2hunter2hunter2");
        assert_eq!(r, "spring.datasource.password=hunter...ter2");

        // Anchored bare key at line start.
        let r = redact("password=verysecretpassword123");
        assert_eq!(r, "password=veryse...d123");

        // Mid-sentence bare key NOT matched.
        let s = "I have password=foo in prose";
        assert_eq!(redact(s), s);
    }

    #[test]
    fn json_and_yaml_fields() {
        let r = redact(r#"{"api_key": "abcdefghijklmnopqrstu"}"#);
        assert_eq!(r, r#"{"api_key": "abcdef...rstu"}"#);

        let r = redact("api_key: abcdefghijklmnopqrstu");
        assert_eq!(r, "api_key: abcdef...rstu");

        // Short YAML value fully masked.
        let r = redact("password: hunter2");
        assert_eq!(r, "password: ***");

        // Prose with keyword in the VALUE not matched.
        let s = "note: secret meeting soon";
        assert_eq!(redact(s), s);
    }

    #[test]
    fn code_file_skips_env_json_yaml() {
        let opts = RedactOptions { force: true, code_file: true, ..Default::default() };
        let s = "MAX_TOKENS=100000000000000000000";
        assert_eq!(redact_sensitive_text_opts(s, opts), s);
        let s = r#""apiKey": "test-fixture-value-xyz""#;
        assert_eq!(redact_sensitive_text_opts(s, opts), s);
    }

    #[test]
    fn auth_headers_any_scheme() {
        let r = redact("Authorization: Bearer abcdefghijklmnopqrstuvwx");
        assert_eq!(r, "Authorization: Bearer abcdef...uvwx");

        let r = redact("Authorization: Basic dXNlcjpwYXNzd29yZDEyMw==");
        assert!(r.starts_with("Authorization: Basic dXNlcj..."));

        let r = redact("Proxy-Authorization: token ghproxytoken1234567890");
        assert_eq!(r, "Proxy-Authorization: token ghprox...7890");

        // The YAML pass masks first, then the secret-header pass re-masks the
        // short mask — upstream ground truth is ***.
        let r = redact("x-api-key: my-secret-api-key-value-123");
        assert_eq!(r, "x-api-key: ***");
    }

    #[test]
    fn telegram_private_key_db_url() {
        let r = redact("bot123456789:AAAA-BBBB_cccc1234ddddEEEEffff56789");
        assert_eq!(r, "bot123456789:***");

        let r = redact("-----BEGIN RSA PRIVATE KEY-----\nabc\n-----END RSA PRIVATE KEY-----");
        assert_eq!(r, "[REDACTED PRIVATE KEY]");

        let r = redact("postgres://joey:hunter2@db.example.com/app");
        assert_eq!(r, "postgres://joey:***@db.example.com/app");

        // code_file preserves f-string templates.
        let opts = RedactOptions { force: true, code_file: true, ..Default::default() };
        let s = "postgresql://{user}:{password}@{host}/db";
        assert_eq!(redact_sensitive_text_opts(s, opts), s);

        // Bare token in userinfo.
        let r = redact("https://ghp1234567890abcdef@github.com/o/r.git");
        assert_eq!(r, "https://ghp123...cdef@github.com/o/r.git");

        // user:pass@ web URL passes through (per upstream #34029).
        let s = "https://user:pass@example.com/x";
        assert_eq!(redact(s), s);
    }

    #[test]
    fn jwt_phone_form_body() {
        let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.abcdEFGH1234";
        let r = redact(&format!("jwt {}", jwt));
        assert_eq!(r, "jwt eyJhbG...1234");

        let r = redact("call me at +14155552671 now");
        assert_eq!(r, "call me at +141****2671 now");
        let r = redact("+3712345 x");
        assert_eq!(r, "+3****45 x");

        let r = redact("username=joey&password=hunter2&scope=all");
        assert_eq!(r, "username=joey&password=***&scope=all");

        // Not a pure form body → untouched.
        let s = "see a=1&b=2 in the text";
        assert_eq!(redact(s), s);
    }

    #[test]
    fn file_read_sentinel() {
        let opts = RedactOptions { force: true, file_read: true, ..Default::default() };
        let r = redact_sensitive_text_opts("key: ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ123456", opts);
        assert_eq!(r, "key: «redacted:ghp_…»");
    }

    #[test]
    fn mask_secret_shapes() {
        assert_eq!(mask_secret_default("sk-proj-abcdef1234567890"), "sk-p...7890");
        assert_eq!(mask_secret_default("short"), "***");
        assert_eq!(mask_secret_default(""), "");
        assert_eq!(mask_secret("long-token", 6, 4, 18, "***", ""), "***");
    }

    #[test]
    fn env_dump_detection() {
        assert!(is_env_dump_command("env"));
        assert!(is_env_dump_command("printenv | grep KEY"));
        assert!(is_env_dump_command("cd /tmp && env"));
        assert!(!is_env_dump_command("cat .env"));
        assert!(!is_env_dump_command("echo hello"));
        assert!(!is_env_dump_command(""));
    }

    #[test]
    fn terminal_output_policy() {
        let dump = "MY_SERVICE_TOKEN=abc123randomstring999";
        // env-dump → ENV pass applies (then the anchored pass re-masks to ***,
        // matching upstream ground truth).
        let r = redact_terminal_output(dump, Some("env"), true);
        assert_eq!(r, "MY_SERVICE_TOKEN=***");
        // unknown command → code_file: opaque assignment left alone…
        let r = redact_terminal_output(dump, Some("cat config.py"), true);
        assert_eq!(r, dump);
        // …but prefix keys still masked.
        let r = redact_terminal_output("x sk-abcdefghijklmnopqrstuvwx", Some("cat f"), true);
        assert_eq!(r, "x sk-abc...uvwx");
    }

    #[test]
    fn leaves_plain_text() {
        assert_eq!(redact("hello world"), "hello world");
    }
}
