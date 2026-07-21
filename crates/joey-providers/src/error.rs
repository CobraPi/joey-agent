//! Provider error taxonomy (port of `agent/error_classifier.py`
//! `_classify_by_status` / `_classify_400` / `_classify_402`) and the
//! jittered-backoff helper (port of `agent/retry_utils.py`).

use std::time::Duration;

/// A classified provider error. The agent loop uses the classification to
/// decide retry/backoff/compression/failover.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("authentication failed: {0}")]
    Auth(String),
    #[error("rate limited (retry after {retry_after:?}): {message}")]
    RateLimit {
        message: String,
        retry_after: Option<Duration>,
    },
    /// Billing / credit exhaustion (upstream `FailoverReason.billing`).
    #[error("billing/credits exhausted: {0}")]
    Billing(String),
    /// Provider-side overload (upstream `FailoverReason.overloaded`): the
    /// credential is fine, the server is busy — back off and retry.
    #[error("provider overloaded: {0}")]
    Overloaded(String),
    /// 413 — retryable after compressing the conversation.
    #[error("payload too large (413): {0}")]
    PayloadTooLarge(String),
    /// Context-window overflow — retryable after compression.
    #[error("context overflow: {0}")]
    ContextOverflow(String),
    #[error("model not found: {0}")]
    ModelNotFound(String),
    /// Malformed request — deterministic, never retried (upstream
    /// `FailoverReason.format_error`).
    #[error("request format error: {0}")]
    FormatError(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("connection error: {0}")]
    Connection(String),
    /// A stream that yielded zero events/chunks (upstream `EmptyStreamError`,
    /// chat_completion_helpers.py:2968-2980) — retryable.
    #[error("empty stream: {0}")]
    EmptyStream(String),
    /// Retryable server-side error (5xx and equivalents).
    #[error("provider server error: {0}")]
    ServerError(String),
    /// Unclassified HTTP status. 404s that matched no known pattern land
    /// here and stay retryable (error_classifier.py:1008-1018).
    #[error("provider returned status {status}: {message}")]
    Status { status: u16, message: String },
    #[error("response parse error: {0}")]
    Parse(String),
    #[error("{0}")]
    Other(String),
}

impl ProviderError {
    /// Whether the agent loop should retry the same request.
    pub fn is_retryable(&self) -> bool {
        match self {
            ProviderError::RateLimit { .. }
            | ProviderError::Overloaded(_)
            | ProviderError::PayloadTooLarge(_)
            | ProviderError::ContextOverflow(_)
            | ProviderError::Timeout(_)
            | ProviderError::Connection(_)
            | ProviderError::EmptyStream(_)
            | ProviderError::ServerError(_) => true,
            // Generic 404 with no model-not-found signal → unknown, retryable
            // (error_classifier.py:1008-1018). Other unclassified 5xx retry.
            ProviderError::Status { status, .. } => {
                *status == 404 || (500..600).contains(status)
            }
            _ => false,
        }
    }

    /// Whether the loop should compress the conversation before retrying
    /// (upstream `should_compress`: 413 payload_too_large + context_overflow).
    pub fn should_compress(&self) -> bool {
        matches!(
            self,
            ProviderError::PayloadTooLarge(_) | ProviderError::ContextOverflow(_)
        )
    }

    /// Whether the loop should activate a fallback model/credential.
    pub fn should_failover(&self) -> bool {
        matches!(
            self,
            ProviderError::Auth(_)
                | ProviderError::Billing(_)
                | ProviderError::ModelNotFound(_)
                | ProviderError::FormatError(_)
                | ProviderError::RateLimit { .. }
        )
    }

    /// The server-advised retry delay, if the error carried one.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ProviderError::RateLimit { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// Classify an HTTP status + body into a provider error. Port of
    /// `error_classifier._classify_by_status` (error_classifier.py:924-1156).
    pub fn from_status(status: u16, body: &str, retry_after: Option<Duration>) -> Self {
        let msg: String = body.chars().take(2000).collect();
        let lower = msg.to_lowercase();
        let has = |patterns: &[&str]| patterns.iter().any(|p| lower.contains(p));

        match status {
            401 => ProviderError::Auth(msg),
            403 => {
                // OpenRouter 403 "key limit exceeded" is actually billing;
                // other providers also use 403 for plan/credit exhaustion
                // (error_classifier.py:952-974).
                if lower.contains("key limit exceeded")
                    || lower.contains("spending limit")
                    || has(BILLING_PATTERNS)
                {
                    ProviderError::Billing(msg)
                } else {
                    ProviderError::Auth(msg)
                }
            }
            402 => classify_402(&lower, msg),
            404 => {
                if has(BILLING_PATTERNS) {
                    ProviderError::Billing(msg)
                } else if has(PROVIDER_POLICY_BLOCKED_PATTERNS) {
                    // Account-level policy block — deterministic (1002-1007).
                    ProviderError::FormatError(msg)
                } else if has(MODEL_NOT_FOUND_PATTERNS) {
                    ProviderError::ModelNotFound(msg)
                } else {
                    // Generic 404 — wrong endpoint path / proxy glitch:
                    // unknown, retryable (1008-1018).
                    ProviderError::Status { status, message: msg }
                }
            }
            408 => ProviderError::Timeout(msg),
            413 => ProviderError::PayloadTooLarge(msg),
            429 => {
                // Server-wide overload reuses 429 on some providers (Z.AI):
                // back off on the same key rather than rotate (1027-1040).
                if has(OVERLOADED_PATTERNS) {
                    ProviderError::Overloaded(msg)
                } else {
                    ProviderError::RateLimit {
                        message: msg,
                        retry_after,
                    }
                }
            }
            400 => classify_400(&lower, msg),
            500 | 502 => {
                // Some gateways return request-validation errors as 5xx —
                // deterministic; fail fast (error_classifier.py:1074-1091).
                if has(REQUEST_VALIDATION_PATTERNS) {
                    ProviderError::FormatError(msg)
                } else if has(EMPTY_PROVIDER_RESPONSE_PATTERNS) {
                    ProviderError::ServerError(msg)
                } else if has(CONTEXT_OVERFLOW_PATTERNS) {
                    ProviderError::ContextOverflow(msg)
                } else {
                    ProviderError::ServerError(msg)
                }
            }
            503 | 529 => {
                if has(EMPTY_PROVIDER_RESPONSE_PATTERNS) {
                    ProviderError::ServerError(msg)
                } else if has(CONTEXT_OVERFLOW_PATTERNS) {
                    ProviderError::ContextOverflow(msg)
                } else {
                    ProviderError::Overloaded(msg)
                }
            }
            s if (400..500).contains(&s) => ProviderError::FormatError(msg),
            s if (500..600).contains(&s) => ProviderError::ServerError(msg),
            _ => ProviderError::Status { status, message: msg },
        }
    }
}

/// Disambiguate 402: transient usage limit vs billing exhaustion
/// (`_classify_402`, error_classifier.py:1159-1185).
fn classify_402(lower: &str, msg: String) -> ProviderError {
    let has_usage_limit = USAGE_LIMIT_PATTERNS.iter().any(|p| lower.contains(p));
    let has_transient = USAGE_LIMIT_TRANSIENT_SIGNALS.iter().any(|p| lower.contains(p));
    if has_usage_limit && has_transient {
        ProviderError::RateLimit {
            message: msg,
            retry_after: None,
        }
    } else {
        ProviderError::Billing(msg)
    }
}

/// Classify 400 Bad Request (`_classify_400`, error_classifier.py:1188-1353).
/// Bucket order matters — mirrored from upstream. The generic-400-plus-large-
/// session heuristic needs conversation size, which this layer doesn't have;
/// those 400s fall through to `FormatError` (deliberate adaptation).
fn classify_400(lower: &str, msg: String) -> ProviderError {
    let has = |patterns: &[&str]| patterns.iter().any(|p| lower.contains(p));
    // Request-validation errors must be checked BEFORE context overflow —
    // "Unsupported parameter: 'max_tokens' …" contains the bare "max_tokens"
    // overflow pattern (error_classifier.py:1245-1271). Upstream excludes the
    // bare "invalid_request_error" code here (OpenAI stamps it on genuine
    // overflows too).
    if REQUEST_VALIDATION_PATTERNS
        .iter()
        .filter(|p| **p != "invalid_request_error")
        .any(|p| lower.contains(p))
    {
        return ProviderError::FormatError(msg);
    }
    // Empty-provider-response advisories mention "max_tokens" as a possible
    // cause — they must not enter compression (1273-1282).
    if has(EMPTY_PROVIDER_RESPONSE_PATTERNS) {
        return ProviderError::ServerError(msg);
    }
    if has(CONTEXT_OVERFLOW_PATTERNS) {
        return ProviderError::ContextOverflow(msg);
    }
    if has(PROVIDER_POLICY_BLOCKED_PATTERNS) {
        return ProviderError::FormatError(msg);
    }
    if has(MODEL_NOT_FOUND_PATTERNS) {
        return ProviderError::ModelNotFound(msg);
    }
    // Some providers return rate-limit / billing errors as 400 (1306-1321).
    if has(RATE_LIMIT_PATTERNS) {
        return ProviderError::RateLimit {
            message: msg,
            retry_after: None,
        };
    }
    if has(BILLING_PATTERNS) {
        return ProviderError::Billing(msg);
    }
    ProviderError::FormatError(msg)
}

impl From<reqwest::Error> for ProviderError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_timeout() {
            ProviderError::Timeout(e.to_string())
        } else if e.is_connect() {
            ProviderError::Connection(e.to_string())
        } else {
            ProviderError::Other(e.to_string())
        }
    }
}

// ── Message-pattern tables (error_classifier.py:100-340) ─────────────────────

/// Billing exhaustion (not transient rate limit) — error_classifier.py:104-124.
pub(crate) const BILLING_PATTERNS: &[&str] = &[
    "insufficient credits",
    "insufficient_quota",
    "insufficient balance",
    "credit balance",
    "credits exhausted",
    "credits have been exhausted",
    "no usable credits",
    "top up your credits",
    "payment required",
    "billing hard limit",
    "exceeded your current quota",
    "account is deactivated",
    "plan does not include",
    "out of extra usage",
    "out of funds",
    "run out of funds",
    "balance_depleted",
    "model_not_supported_on_free_tier",
    "not available on the free tier",
];

/// Transient rate limiting — error_classifier.py:146-162.
pub(crate) const RATE_LIMIT_PATTERNS: &[&str] = &[
    "rate limit",
    "rate_limit",
    "too many requests",
    "throttled",
    "requests per minute",
    "tokens per minute",
    "requests per day",
    "try again in",
    "please retry after",
    "resource_exhausted",
    "rate increased too quickly",
    "throttlingexception",
    "too many concurrent requests",
    "servicequotaexceededexception",
];

/// Provider-side overload — error_classifier.py:174-187.
pub(crate) const OVERLOADED_PATTERNS: &[&str] = &[
    "overloaded",
    "temporarily overloaded",
    "service is temporarily overloaded",
    "service may be temporarily overloaded",
    "server is overloaded",
    "server overloaded",
    "service overloaded",
    "service is overloaded",
    "upstream overloaded",
    "currently overloaded",
    "at capacity",
    "over capacity",
];

/// Usage-limit disambiguation — error_classifier.py:190-195.
const USAGE_LIMIT_PATTERNS: &[&str] = &["usage limit", "quota", "limit exceeded", "key limit exceeded"];

/// Signals a usage limit is transient — error_classifier.py:198-207.
const USAGE_LIMIT_TRANSIENT_SIGNALS: &[&str] = &[
    "try again",
    "retry",
    "resets at",
    "reset in",
    "wait",
    "requests remaining",
    "periodic",
    "window",
];

/// Context overflow — error_classifier.py:261-301.
pub(crate) const CONTEXT_OVERFLOW_PATTERNS: &[&str] = &[
    "context length",
    "context size",
    "maximum context",
    "token limit",
    "too many tokens",
    "reduce the length",
    "exceeds the limit",
    "context window",
    "prompt is too long",
    "prompt exceeds max length",
    // NOTE: bare "max_tokens" is load-bearing upstream — kept.
    "max_tokens",
    "maximum number of tokens",
    "exceeds the max_model_len",
    "max_model_len",
    "prompt length",
    "input is too long",
    "maximum model length",
    "context length exceeded",
    "truncating input",
    "slot context",
    "n_ctx_slot",
    "超过最大长度",
    "上下文长度",
    "tokens in request more than max tokens allowed",
    "max input token",
    "input token",
    "exceeds the maximum number of input tokens",
];

/// Model not found — error_classifier.py:304-322.
pub(crate) const MODEL_NOT_FOUND_PATTERNS: &[&str] = &[
    "is not a valid model",
    "invalid model",
    "model not found",
    "model_not_found",
    "does not exist",
    "no such model",
    "unknown model",
    "unsupported model",
    "no endpoints found that support tool use",
];

/// Deterministic request-validation failures — error_classifier.py:333-340.
pub(crate) const REQUEST_VALIDATION_PATTERNS: &[&str] = &[
    "unknown parameter",
    "unsupported parameter",
    "unrecognized request argument",
    "invalid_request_error",
    "unknown_parameter",
    "unsupported_parameter",
];

/// OpenRouter account-policy blocks — error_classifier.py:359-363.
const PROVIDER_POLICY_BLOCKED_PATTERNS: &[&str] = &[
    "no endpoints available matching your guardrail",
    "no endpoints available matching your data policy",
    "no endpoints found matching your data policy",
];

/// Empty-provider-response advisories — error_classifier.py:439-445.
const EMPTY_PROVIDER_RESPONSE_PATTERNS: &[&str] = &[
    "returned an empty response",
    "empty response despite retries",
    "provider returned an empty response",
    "model returning empty responses",
    "empty response stream",
];

// ── Backoff (retry_utils.py:36-74) ──────────────────────────────────────────

/// API-error retry backoff: base 2.0s, max 60.0s, jitter uniform in
/// [0, 0.5·delay) — the variant the conversation loop uses for API errors
/// (conversation_loop.py:4318: `jittered_backoff(retry_count, base_delay=2.0,
/// max_delay=60.0)`).
pub fn jittered_backoff(attempt: u32) -> Duration {
    jittered_backoff_with(attempt, 2.0, 60.0)
}

/// The default upstream `jittered_backoff` parameters (base 5s, max 120s) —
/// used by the loop's non-API-error retries.
pub fn jittered_backoff_slow(attempt: u32) -> Duration {
    jittered_backoff_with(attempt, 5.0, 120.0)
}

/// Jittered exponential backoff: `min(base·2^(attempt-1), max) + U[0, 0.5·delay)`
/// with a real RNG (retry_utils.py:36-74). `attempt` is 1-based.
pub fn jittered_backoff_with(attempt: u32, base_delay: f64, max_delay: f64) -> Duration {
    use rand::Rng;
    let exponent = attempt.saturating_sub(1);
    let delay = if exponent >= 63 || base_delay <= 0.0 {
        max_delay
    } else {
        (base_delay * 2f64.powi(exponent as i32)).min(max_delay)
    };
    let jitter_ratio = 0.5;
    let jitter = rand::thread_rng().gen_range(0.0..(jitter_ratio * delay).max(f64::MIN_POSITIVE));
    Duration::from_secs_f64(delay + jitter)
}

/// Parse a Retry-After header value: float seconds, capped at 600s
/// (conversation_loop.py:4309-4317). Non-numeric values are ignored.
pub fn parse_retry_after(raw: &str) -> Option<Duration> {
    let secs: f64 = raw.trim().parse().ok()?;
    if !secs.is_finite() || secs < 0.0 {
        return None;
    }
    Some(Duration::from_secs_f64(secs.min(600.0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_table() {
        // 401 → auth, non-retryable, failover
        let e = ProviderError::from_status(401, "unauthorized", None);
        assert!(matches!(e, ProviderError::Auth(_)));
        assert!(!e.is_retryable() && e.should_failover());

        // 403 billing pattern → billing
        let e = ProviderError::from_status(403, "key limit exceeded", None);
        assert!(matches!(e, ProviderError::Billing(_)));
        // 403 plain → auth
        let e = ProviderError::from_status(403, "forbidden", None);
        assert!(matches!(e, ProviderError::Auth(_)));

        // 402 transient usage limit → rate limit; plain → billing
        let e = ProviderError::from_status(402, "usage limit reached, try again in 5 minutes", None);
        assert!(matches!(e, ProviderError::RateLimit { .. }));
        let e = ProviderError::from_status(402, "insufficient balance", None);
        assert!(matches!(e, ProviderError::Billing(_)));

        // 404 with model pattern → model_not_found (non-retryable)
        let e = ProviderError::from_status(404, "The model `foo` does not exist", None);
        assert!(matches!(e, ProviderError::ModelNotFound(_)));
        assert!(!e.is_retryable());
        // 404 without pattern → unknown, retryable
        let e = ProviderError::from_status(404, "not found", None);
        assert!(matches!(e, ProviderError::Status { status: 404, .. }));
        assert!(e.is_retryable());

        // 408 → timeout retryable
        let e = ProviderError::from_status(408, "request timeout", None);
        assert!(matches!(e, ProviderError::Timeout(_)));
        assert!(e.is_retryable());

        // 413 → retryable + should_compress
        let e = ProviderError::from_status(413, "payload too large", None);
        assert!(matches!(e, ProviderError::PayloadTooLarge(_)));
        assert!(e.is_retryable() && e.should_compress());

        // 429 overload body → overloaded (no rotate); plain → rate limit
        let e = ProviderError::from_status(429, "service is temporarily overloaded", None);
        assert!(matches!(e, ProviderError::Overloaded(_)));
        let e = ProviderError::from_status(429, "slow down", Some(Duration::from_secs(3)));
        assert!(matches!(e, ProviderError::RateLimit { .. }));
        assert_eq!(e.retry_after(), Some(Duration::from_secs(3)));

        // 400 buckets
        let e = ProviderError::from_status(
            400,
            "Unsupported parameter: 'max_tokens' is not supported with this model.",
            None,
        );
        assert!(matches!(e, ProviderError::FormatError(_)), "validation beats overflow");
        let e = ProviderError::from_status(400, "prompt is too long: 250000 tokens", None);
        assert!(matches!(e, ProviderError::ContextOverflow(_)));
        assert!(e.is_retryable() && e.should_compress());
        let e = ProviderError::from_status(400, "rate limit exceeded, try again in 20s", None);
        assert!(matches!(e, ProviderError::RateLimit { .. }));
        let e = ProviderError::from_status(400, "bad request", None);
        assert!(matches!(e, ProviderError::FormatError(_)));
        assert!(!e.is_retryable());

        // 5xx request-validation fail-fast (502 gateway)
        let e = ProviderError::from_status(502, "unknown parameter: reasoning", None);
        assert!(matches!(e, ProviderError::FormatError(_)));
        assert!(!e.is_retryable());
        // Generic 5xx retryable
        let e = ProviderError::from_status(500, "internal", None);
        assert!(matches!(e, ProviderError::ServerError(_)));
        assert!(e.is_retryable());
        let e = ProviderError::from_status(529, "overloaded_error", None);
        assert!(matches!(e, ProviderError::Overloaded(_)));
    }

    #[test]
    fn backoff_bounds_with_real_jitter() {
        // attempt 1 (base 2, max 60): delay=2, jitter ∈ [0,1) → [2,3)
        for _ in 0..50 {
            let d = jittered_backoff(1).as_secs_f64();
            assert!((2.0..3.0).contains(&d), "attempt 1 out of bounds: {d}");
        }
        // attempt 6: 2*2^5=64 → capped 60, jitter ∈ [0,30) → [60,90)
        for _ in 0..50 {
            let d = jittered_backoff(6).as_secs_f64();
            assert!((60.0..90.0).contains(&d), "attempt 6 out of bounds: {d}");
        }
        // slow variant attempt 1: [5, 7.5)
        for _ in 0..50 {
            let d = jittered_backoff_slow(1).as_secs_f64();
            assert!((5.0..7.5).contains(&d), "slow attempt 1 out of bounds: {d}");
        }
        // jitter is actually random (two draws differ eventually)
        let a = jittered_backoff(3);
        let mut differs = false;
        for _ in 0..20 {
            if jittered_backoff(3) != a {
                differs = true;
                break;
            }
        }
        assert!(differs, "jitter looks deterministic");
    }

    #[test]
    fn retry_after_parsing() {
        assert_eq!(parse_retry_after("2.5"), Some(Duration::from_secs_f64(2.5)));
        assert_eq!(parse_retry_after("1200"), Some(Duration::from_secs(600)), "capped at 600s");
        assert_eq!(parse_retry_after("soon"), None);
        assert_eq!(parse_retry_after("-3"), None);
    }
}
