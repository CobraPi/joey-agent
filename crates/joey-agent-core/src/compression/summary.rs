//! The auxiliary summary call for context compression (port of the
//! compression slice of `agent/auxiliary_client.py`: `call_llm(task=
//! "compression", ...)` resolution + timeout + the summary-failure
//! classification helpers from `agent/context_compressor.py`).

use std::time::Duration;

use async_trait::async_trait;
use joey_core::Config;
use joey_providers::{build_client, Message, ProviderClient, ProviderError, ProviderRequest};

/// Default auxiliary timeout when config has none (auxiliary_client.py:6356).
const DEFAULT_AUX_TIMEOUT: f64 = 30.0;
/// Bounded floor for config-derived compression timeouts (#54915,
/// auxiliary_client.py:6367).
const COMPRESSION_TIMEOUT_FLOOR_SECONDS: f64 = 300.0;

// ── Summary-failure classification (context_compressor.py:41-78, 2467-2508) ──

/// Failure classes driving the `_generate_summary` recovery paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryFailureClass {
    /// 404/503 / "model_not_found" / "does not exist" / "no available channel"
    /// → immediate main-model fallback, no cooldown.
    ModelNotFound,
    /// 408/429/502/504 / "timeout" / "timed out" → escalating cooldown ladder.
    Timeout,
    /// Malformed/non-JSON response bodies ("expecting value") → 30s cooldown.
    JsonDecode,
    /// Transient network/stream-close errors → 30s cooldown + abort (the
    /// session is preserved rather than degraded, #29559/#25585).
    StreamClosed,
    /// Non-retryable auth/permission/quota (401/402/403, missing credential,
    /// permanent quota markers) → abort, session preserved.
    AccessOrQuota,
    /// Everything else → 60s cooldown.
    Other,
}

/// context_compressor.py:41-49.
const SUMMARY_PERMANENT_QUOTA_MARKERS: &[&str] = &[
    "insufficient_quota",
    "quota exceeded",
    "quota_exceeded",
    "out of funds",
    "out of credits",
    "out of credit",
    "out of extra usage",
];

/// context_compressor.py:51-54.
const SUMMARY_MISSING_CREDENTIAL_MARKERS: &[&str] = &["no api key was found", "no api key found"];

/// Non-retryable summary auth, permission, or quota errors
/// (`_is_summary_access_or_quota_error`): rate limits are excluded FIRST;
/// auth classes are always terminal; billing needs a permanent-quota marker.
pub fn is_summary_access_or_quota_error(e: &ProviderError) -> bool {
    if matches!(e, ProviderError::RateLimit { .. }) {
        return false;
    }
    if matches!(e, ProviderError::Auth(_)) {
        return true;
    }
    let err_text = e.to_string().to_lowercase();
    if SUMMARY_MISSING_CREDENTIAL_MARKERS.iter().any(|m| err_text.contains(m)) {
        return true;
    }
    if matches!(e, ProviderError::Status { status: 401..=403, .. }) {
        return true;
    }
    SUMMARY_PERMANENT_QUOTA_MARKERS.iter().any(|m| err_text.contains(m))
}

/// Classify a summary-call failure into its `_generate_summary` recovery
/// class (context_compressor.py:2467-2606 branch conditions, mapped onto the
/// port's [`ProviderError`] taxonomy: 502 ⇒ timeout class, 503/overload ⇒
/// model-unavailable class — matching the upstream status buckets).
pub fn classify_summary_failure(e: &ProviderError) -> SummaryFailureClass {
    if is_summary_access_or_quota_error(e) {
        return SummaryFailureClass::AccessOrQuota;
    }
    let err_text = e.to_string().to_lowercase();
    let is_model_not_found = matches!(e, ProviderError::ModelNotFound(_))
        || matches!(e, ProviderError::Status { status: 404 | 503, .. })
        || matches!(e, ProviderError::Overloaded(_))
        || err_text.contains("model_not_found")
        || err_text.contains("does not exist")
        || err_text.contains("no available channel");
    let is_timeout = matches!(e, ProviderError::Timeout(_))
        || matches!(e, ProviderError::RateLimit { .. })
        || matches!(e, ProviderError::ServerError(_))
        || matches!(e, ProviderError::Status { status: 408 | 429 | 502 | 504, .. })
        || err_text.contains("timeout")
        || err_text.contains("timed out");
    let is_json_decode =
        matches!(e, ProviderError::Parse(_)) || err_text.contains("expecting value");
    let is_streaming_closed =
        matches!(e, ProviderError::Connection(_) | ProviderError::EmptyStream(_));

    // Precedence mirrors the upstream branch ordering: timeout beats the
    // streaming-closed short rung; model-not-found beats both for the
    // fallback reason (evaluated before cooldowns there).
    if is_timeout && !is_model_not_found {
        SummaryFailureClass::Timeout
    } else if is_model_not_found {
        SummaryFailureClass::ModelNotFound
    } else if is_json_decode {
        SummaryFailureClass::JsonDecode
    } else if is_streaming_closed {
        SummaryFailureClass::StreamClosed
    } else {
        SummaryFailureClass::Other
    }
}

// ── The summary backend ─────────────────────────────────────────────────

/// Abstraction over the auxiliary summary LLM call, so the compressor can be
/// driven by a scripted backend in tests.
#[async_trait]
pub trait SummaryBackend: Send + Sync {
    /// Generate the summary text for `prompt` (sent as a single user
    /// message, non-streaming, no max_tokens — the output cap must never
    /// truncate a summary).
    async fn generate(
        &self,
        prompt: &str,
        model_override: Option<&str>,
    ) -> Result<String, ProviderError>;

    /// Whether a provider/credential is actually available.
    fn has_provider(&self) -> bool;

    /// The resolved aux model id (for feasibility warnings).
    fn resolved_model(&self) -> String;

    /// A human-readable provider label (for feasibility warnings).
    fn resolved_provider(&self) -> String;

    /// The aux model's context length (catalog resolution).
    fn aux_context_length(&self) -> i64;
}

/// The real backend: resolves the compression aux runtime from config
/// (`auxiliary.compression.*`) with the upstream priority chain —
///
/// 1. `auxiliary.compression.base_url` (+api_key) → custom endpoint
/// 2. `auxiliary.compression.provider` (non-auto) → that provider, with
///    `auxiliary.compression.model`, else the provider profile's
///    `default_aux_model`, else the main model
/// 3. "auto" → the main runtime (main provider + main model — auxiliary
///    tasks run on the user's chat model, auxiliary_client.py `_resolve_auto`
///    step 1)
///
/// with the configured timeout (`auxiliary.compression.timeout`, default
/// 120s) floored at 300s for compression (#54915).
pub struct AuxSummaryBackend {
    client: Option<ProviderClient>,
    model: String,
    provider_label: String,
    timeout: Duration,
    aux_context_length: i64,
}

impl AuxSummaryBackend {
    /// Build from config + the main runtime (provider, model, base_url,
    /// api_key). `aux_context_config` is `auxiliary.compression.context_length`
    /// when set.
    pub fn from_config(
        config: &Config,
        main_provider: &str,
        main_model: &str,
        main_base_url: &str,
        main_api_key: Option<&str>,
    ) -> Self {
        let cfg_provider = config.get_str("auxiliary.compression.provider", "auto");
        let mut cfg_model = config.get_str("auxiliary.compression.model", "");
        // 'auto' is a sentinel meaning "inherit", not a literal model id
        // (auxiliary_client.py:6265-6273).
        if cfg_model.eq_ignore_ascii_case("auto") {
            cfg_model = String::new();
        }
        let cfg_base_url = config.get_str("auxiliary.compression.base_url", "");
        let cfg_api_key = config.get_str("auxiliary.compression.api_key", "");
        let cfg_timeout = config.get_f64("auxiliary.compression.timeout", DEFAULT_AUX_TIMEOUT);
        // Config-derived compression timeouts are floored at 300s (#54915).
        let timeout = Duration::from_secs_f64(cfg_timeout.max(COMPRESSION_TIMEOUT_FLOOR_SECONDS));
        let aux_context_config = config
            .get("auxiliary.compression.context_length")
            .and_then(joey_core::config::value_as_i64)
            .filter(|v| *v > 0);

        let (client, model, provider_label) = if !cfg_base_url.trim().is_empty() {
            // Explicit custom endpoint.
            let model = if cfg_model.is_empty() { main_model.to_string() } else { cfg_model };
            let key = if cfg_api_key.trim().is_empty() { None } else { Some(cfg_api_key) };
            let client = build_client("custom", cfg_base_url.trim(), &model, key).ok();
            (client, model, "custom".to_string())
        } else if !cfg_provider.trim().is_empty()
            && !cfg_provider.trim().eq_ignore_ascii_case("auto")
        {
            // Explicit provider.
            let key = if cfg_api_key.trim().is_empty() { None } else { Some(cfg_api_key) };
            let client = build_client(cfg_provider.trim(), "", &cfg_model, key).ok();
            let model = if !cfg_model.is_empty() {
                cfg_model
            } else {
                // Provider default aux model, else the main model.
                client
                    .as_ref()
                    .map(|c| c.profile().default_aux_model.to_string())
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| main_model.to_string())
            };
            (client, model, cfg_provider.trim().to_string())
        } else {
            // "auto": the main runtime — same provider, same model.
            let model = if cfg_model.is_empty() { main_model.to_string() } else { cfg_model };
            let client = build_client(
                main_provider,
                main_base_url,
                &model,
                main_api_key.map(str::to_string),
            )
            .ok();
            let label = client
                .as_ref()
                .map(|c| c.profile().name.to_string())
                .unwrap_or_else(|| main_provider.to_string());
            (client, model, label)
        };

        let aux_context_length =
            super::catalog::get_model_context_length(&model, aux_context_config);

        Self { client, model, provider_label, timeout, aux_context_length }
    }
}

#[async_trait]
impl SummaryBackend for AuxSummaryBackend {
    async fn generate(
        &self,
        prompt: &str,
        model_override: Option<&str>,
    ) -> Result<String, ProviderError> {
        let Some(client) = &self.client else {
            return Err(ProviderError::Other(
                "No LLM provider configured for task=compression".to_string(),
            ));
        };
        let model = model_override.unwrap_or(&self.model).to_string();
        // A single user-role message, non-streaming, NO max_tokens: the
        // output cap must never truncate a summary
        // (context_compressor.py:2359-2379).
        let req = ProviderRequest::new(model, vec![Message::user(prompt)]);
        match tokio::time::timeout(self.timeout, client.complete(&req)).await {
            Ok(result) => result.map(|resp| resp.content),
            Err(_) => Err(ProviderError::Timeout(format!(
                "compression summary call timed out after {:.0}s",
                self.timeout.as_secs_f64()
            ))),
        }
    }

    fn has_provider(&self) -> bool {
        self.client.as_ref().map(|c| c.has_credentials()).unwrap_or(false)
    }

    fn resolved_model(&self) -> String {
        self.model.clone()
    }

    fn resolved_provider(&self) -> String {
        self.provider_label.clone()
    }

    fn aux_context_length(&self) -> i64 {
        self.aux_context_length
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_or_quota_classification() {
        // Rate limits are never access errors.
        let e = ProviderError::RateLimit { message: "quota exceeded".into(), retry_after: None };
        assert!(!is_summary_access_or_quota_error(&e));
        // Auth always is.
        assert!(is_summary_access_or_quota_error(&ProviderError::Auth("bad key".into())));
        // Billing needs a permanent quota marker.
        assert!(is_summary_access_or_quota_error(&ProviderError::Billing(
            "insufficient_quota".into()
        )));
        assert!(!is_summary_access_or_quota_error(&ProviderError::Billing(
            "credit balance low".into()
        )));
        // Missing-credential text markers.
        assert!(is_summary_access_or_quota_error(&ProviderError::Other(
            "No API key was found for provider".into()
        )));
    }

    #[test]
    fn failure_class_table() {
        assert_eq!(
            classify_summary_failure(&ProviderError::Timeout("timed out".into())),
            SummaryFailureClass::Timeout
        );
        assert_eq!(
            classify_summary_failure(&ProviderError::ModelNotFound("does not exist".into())),
            SummaryFailureClass::ModelNotFound
        );
        assert_eq!(
            classify_summary_failure(&ProviderError::Connection("connection reset".into())),
            SummaryFailureClass::StreamClosed
        );
        assert_eq!(
            classify_summary_failure(&ProviderError::Parse("expecting value".into())),
            SummaryFailureClass::JsonDecode
        );
        assert_eq!(
            classify_summary_failure(&ProviderError::Auth("401".into())),
            SummaryFailureClass::AccessOrQuota
        );
        assert_eq!(
            classify_summary_failure(&ProviderError::FormatError("bad request".into())),
            SummaryFailureClass::Other
        );
    }
}
