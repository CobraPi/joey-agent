//! GitHub Copilot embeddings backend (REMOTE, provider-following).
//!
//! `POST {base_url}/embeddings` — the Copilot models API's OpenAI-compatible
//! embeddings endpoint (NO `/v1` prefix). Activated by the joey-cli wiring
//! when `model.provider` selects a Copilot wire and `neurocode.rag.backend`
//! is `auto` (an explicit backend value always wins). Auth reuses
//! `joey_providers::copilot::CopilotAuth` — the SAME token exchange
//! (`api.github.com/copilot_internal/v2/token`, cached with expiry margin)
//! the chat provider uses, so embeddings and chat share one credential
//! lifecycle; a pinned custom endpoint (COPILOT_API_BASE_URL /
//! AI_USAGE_HUD_BASE_URL off-githubcopilot host) skips the exchange and
//! sends the raw credential to the proxy, exactly like chat traffic.
//!
//! Wire shape reuses the OpenAI-compat request/response types: the upstream
//! REQUIRES `input` as a JSON array (string inputs are rejected with 400).
//! Order restoration via the `index` field and client-side L2 normalization
//! mirror [`crate::embed::openai_compat`]. Errors lift into the same
//! structural taxonomy (401/403→AuthRejected, 429→RateLimited, other
//! non-2xx/transport→Unreachable, shape violations→MalformedResponse).
//!
//! Joey-native extension — upstream Hermes has no Copilot embeddings path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use joey_providers::copilot::{custom_endpoint, CopilotAuth};

use crate::embed::local_onnx::InputKind;
use crate::embed::openai_compat::{
    l2_normalize, map_send_error, map_status, prefixed_input, EmbeddingsRequest,
    EmbeddingsResponse,
};
use crate::embed::profiles::{self, EmbedProfile, Pooling, TEXT_EMBEDDING_3_SMALL};
use crate::embed::{BackendKind, EmbedError, EmbedPrefixes, EmbedderInfo, EmbeddingBackend};

/// Wire path appended to the resolved base URL — NO `/v1` prefix (the
/// Copilot models API serves `/embeddings` directly).
pub const COPILOT_EMBEDDINGS_PATH: &str = "/embeddings";

/// Default upstream base (mirrors joey-providers' COPILOT_BASE_URL).
pub const DEFAULT_COPILOT_BASE_URL: &str = "https://api.githubcopilot.com";

const AUTH_HINT: &str = "Copilot embeddings: no GitHub credential available — \
     set neurocode.rag.api_key, export COPILOT_GITHUB_TOKEN/GH_TOKEN/\
     GITHUB_TOKEN, or run `gh auth login`";

/// Static Copilot request headers — the agent-turn, non-vision subset of
/// joey-providers `copilot::request_headers`, kept local so this module
/// stays a drop-in sibling of the other HTTP backends.
fn copilot_request_headers() -> Vec<(&'static str, String)> {
    vec![
        ("Editor-Version", "vscode/1.104.1".to_string()),
        ("User-Agent", "JoeyAgent/1.0".to_string()),
        ("Copilot-Integration-Id", "vscode-chat".to_string()),
        ("Openai-Intent", "conversation-edits".to_string()),
        ("x-initiator", "agent".to_string()),
    ]
}

/// Pure base-URL resolution (unit-testable): `COPILOT_API_BASE_URL` wins,
/// then `AI_USAGE_HUD_BASE_URL`, then the upstream default. Trailing `/`
/// trimmed; empty/whitespace values skipped. Any host is accepted here
/// (unlike `custom_endpoint()`, which only returns off-githubcopilot
/// hosts) — a githubcopilot.com override simply re-points the exchange-
/// served base.
pub fn base_url_from_env(
    copilot_api_base_url: Option<&str>,
    ai_usage_hud_base_url: Option<&str>,
) -> String {
    let pick = |v: Option<&str>| {
        v.map(|s| s.trim().trim_end_matches('/').to_string())
            .filter(|s| !s.is_empty())
    };
    pick(copilot_api_base_url)
        .or_else(|| pick(ai_usage_hud_base_url))
        .unwrap_or_else(|| DEFAULT_COPILOT_BASE_URL.to_string())
}

/// Base URL for this process (env → default). Applied at construction;
/// when the token exchange returns an `endpoints.api` base that wins for
/// the REQUEST (see [`CopilotEmbeddings::embed`]).
pub fn resolve_base_url() -> String {
    base_url_from_env(
        std::env::var("COPILOT_API_BASE_URL").ok().as_deref(),
        std::env::var("AI_USAGE_HUD_BASE_URL").ok().as_deref(),
    )
}

/// Profile for a Copilot-served model: a KNOWN profile keeps its prefixes
/// and dim (e.g. a proxy serving `nomic-embed-text-v1.5`); any unknown
/// model id uses the text-embedding-3-small profile — NEVER the nomic
/// default, whose `search_query:`/`search_document:` prefixes would
/// corrupt instruction-free OpenAI-style embedding inputs.
pub fn profile_for(model: &str) -> &'static EmbedProfile {
    profiles::lookup(model).unwrap_or(&TEXT_EMBEDDING_3_SMALL)
}

/// Process-wide `CopilotAuth` cache keyed by (raw token, pinned endpoint):
/// the CLI wiring constructs backends per call, and re-exchanging the
/// GitHub token for every search would add a network round-trip each
/// time. `CopilotAuth` itself caches the exchanged token until near
/// expiry, so one instance per credential serves the whole process.
fn shared_auth(raw_token: String, pinned_endpoint: Option<String>) -> Arc<CopilotAuth> {
    static CACHE: OnceLock<Mutex<HashMap<(String, Option<String>), Arc<CopilotAuth>>>> =
        OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let key = (raw_token, pinned_endpoint);
    if let Ok(guard) = cache.lock() {
        if let Some(hit) = guard.get(&key) {
            return Arc::clone(hit);
        }
    }
    let auth = Arc::new(match key.1.clone() {
        Some(endpoint) => CopilotAuth::with_endpoint(key.0.clone(), endpoint),
        None => CopilotAuth::new(key.0.clone()),
    });
    if let Ok(mut guard) = cache.lock() {
        guard.insert(key, Arc::clone(&auth));
    }
    auth
}

/// GitHub Copilot embeddings backend (see module docs). Construct via
/// [`Self::documents`] (indexing) / [`Self::query`] (search); the endpoint
/// is env-resolved unless pinned, the credential comes from
/// `neurocode.rag.api_key` or the standard Copilot token sources.
pub struct CopilotEmbeddings {
    http: reqwest::Client,
    /// Construction-time endpoint (env → default, or the pinned custom
    /// endpoint) — the consent-gate/descriptor base. Requests prefer the
    /// exchange-supplied `endpoints.api` base when one is returned.
    base_url: String,
    model: String,
    auth: Arc<CopilotAuth>,
    profile: &'static EmbedProfile,
    kind: InputKind,
}

impl CopilotEmbeddings {
    fn with_kind(
        api_key: String,
        model: String,
        timeout_secs: i64,
        kind: InputKind,
    ) -> Result<Self, EmbedError> {
        // Credential: explicit api_key wins; else the standard Copilot
        // token sources (env vars, then `gh auth token` — a LOCAL
        // operation; no network at construction time).
        let raw_token = if api_key.trim().is_empty() {
            joey_providers::copilot::resolve_copilot_token()
                .map(|(token, _)| token)
                .unwrap_or_default()
        } else {
            api_key.trim().to_string()
        };
        let pinned = custom_endpoint();
        let base_url = pinned.clone().unwrap_or_else(resolve_base_url);
        let auth = shared_auth(raw_token, pinned);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(1) as u64))
            .build()
            .map_err(|e| EmbedError::Other(format!("http client build: {}", e)))?;
        let model = {
            let m = model.trim().to_string();
            if m.is_empty() {
                crate::config::DEFAULT_COPILOT_MODEL.to_string()
            } else {
                m
            }
        };
        let profile = profile_for(&model);
        Ok(Self { http, base_url, model, auth, profile, kind })
    }

    /// Adapter applying the profile DOCUMENT prefix (indexing pipelines).
    pub fn documents(
        api_key: String,
        model: String,
        timeout_secs: i64,
    ) -> Result<Self, EmbedError> {
        Self::with_kind(api_key, model, timeout_secs, InputKind::Document)
    }

    /// Adapter applying the profile QUERY prefix (search legs).
    pub fn query(api_key: String, model: String, timeout_secs: i64) -> Result<Self, EmbedError> {
        Self::with_kind(api_key, model, timeout_secs, InputKind::Query)
    }

    /// The kind this adapter embeds as (for diagnostics).
    pub fn input_kind(&self) -> InputKind {
        self.kind
    }

    /// The full request URL for a given base: `{base}/embeddings`.
    fn endpoint(base: &str) -> String {
        format!("{}{}", base.trim_end_matches('/'), COPILOT_EMBEDDINGS_PATH)
    }

    /// Resolve (token, request base) for one call: the exchange result
    /// (cached by `CopilotAuth`) wins; on exchange failure fall back to
    /// the raw credential + construction-time base, mirroring the chat
    /// provider's deliberate raw-token fallback.
    async fn token_and_base(&self) -> Result<(String, String), EmbedError> {
        if !self.auth.has_raw_token() {
            return Err(EmbedError::AuthRejected(AUTH_HINT.to_string()));
        }
        match self.auth.credentials(&self.http).await {
            Ok(creds) if !creds.token.is_empty() => Ok((
                creds.token,
                if creds.base_url.is_empty() {
                    self.base_url.clone()
                } else {
                    creds.base_url
                },
            )),
            _ => Ok((self.auth.raw_token().to_string(), self.base_url.clone())),
        }
    }

    /// Test-only constructor with an explicit base + prebuilt auth (the
    /// stub-server tests pin the endpoint and use a pinned-endpoint
    /// `CopilotAuth`, which skips the network exchange entirely).
    #[cfg(test)]
    fn from_parts(
        base_url: String,
        auth: Arc<CopilotAuth>,
        model: String,
        timeout_secs: i64,
        kind: InputKind,
    ) -> Result<Self, EmbedError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(1) as u64))
            .build()
            .map_err(|e| EmbedError::Other(format!("http client build: {}", e)))?;
        let model = {
            let m = model.trim().to_string();
            if m.is_empty() {
                crate::config::DEFAULT_COPILOT_MODEL.to_string()
            } else {
                m
            }
        };
        let profile = profile_for(&model);
        Ok(Self { http, base_url, model, auth, profile, kind })
    }
}

#[async_trait::async_trait]
impl EmbeddingBackend for CopilotEmbeddings {
    async fn embed(&self, batch: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let (token, base) = self.token_and_base().await?;
        let prefixed: Vec<String> = batch
            .iter()
            .map(|t| prefixed_input(self.profile, t, self.kind))
            .collect();
        let body = EmbeddingsRequest { model: &self.model, input: &prefixed };
        let url = Self::endpoint(&base);
        let mut req = self.http.post(&url).json(&body);
        for (name, value) in copilot_request_headers() {
            req = req.header(name, value);
        }
        req = req.bearer_auth(&token);
        let resp = req.send().await.map_err(|e| map_send_error(e, &url))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(map_status(status, &url));
        }
        let parsed: EmbeddingsResponse =
            resp.json().await.map_err(|e| map_send_error(e, &url))?;

        // Order preservation via the `index` field — identical contract to
        // the OpenAI-compat backend (count/bounds/duplicate violations are
        // MalformedResponse).
        let mut slots: Vec<Option<Vec<f32>>> = vec![None; batch.len()];
        for item in parsed.data {
            if item.index >= batch.len() {
                return Err(EmbedError::MalformedResponse(format!(
                    "index {} out of range for {} inputs",
                    item.index,
                    batch.len()
                )));
            }
            if slots[item.index].is_some() {
                return Err(EmbedError::MalformedResponse(format!(
                    "duplicate index {} in response",
                    item.index
                )));
            }
            slots[item.index] = Some(item.embedding);
        }
        let missing = slots.iter().filter(|s| s.is_none()).count();
        let mut out: Vec<Vec<f32>> = slots.into_iter().collect::<Option<Vec<_>>>().ok_or_else(|| {
            EmbedError::MalformedResponse(format!(
                "wrong count: {} vectors for {} inputs",
                batch.len() - missing,
                batch.len()
            ))
        })?;
        for v in out.iter_mut() {
            l2_normalize(v);
        }
        Ok(out)
    }

    fn describe_embedder(&self) -> EmbedderInfo {
        EmbedderInfo {
            backend_kind: BackendKind::Copilot,
            profile_name: self.profile.name.to_string(),
            dim: self.profile.dim,
            pooling: Pooling::Mean,
            prefixes: EmbedPrefixes {
                query: self.profile.prefix_query.to_string(),
                document: self.profile.prefix_document.to_string(),
            },
            base_url: self.base_url.clone(),
            model: self.model.clone(),
        }
    }

    async fn health_check(&self) -> Result<(), EmbedError> {
        let out = self.embed(&[String::from("health")]).await?;
        if out.len() != 1 {
            return Err(EmbedError::EmptyResult(format!(
                "health check returned {} vectors for 1 input",
                out.len()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::openai_compat::test_util::spawn_stub;

    fn block_on<F: std::future::Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(fut)
    }

    /// Pinned-endpoint auth against the stub base: `credentials()` skips
    /// the network exchange, so tests never leave the machine.
    fn stub_auth(base: &str) -> Arc<CopilotAuth> {
        Arc::new(CopilotAuth::with_endpoint("test-key".into(), base.to_string()))
    }

    #[test]
    fn base_url_env_matrix() {
        assert_eq!(base_url_from_env(None, None), DEFAULT_COPILOT_BASE_URL);
        assert_eq!(
            base_url_from_env(Some("http://127.0.0.1:8080/"), None),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            base_url_from_env(None, Some("https://hud.local")),
            "https://hud.local"
        );
        assert_eq!(base_url_from_env(Some("  "), Some("https://hud.local")), "https://hud.local");
    }

    #[test]
    fn unknown_models_use_the_text_embedding_profile() {
        assert_eq!(profile_for("text-embedding-3-small").name, "text-embedding-3-small");
        assert_eq!(profile_for("text-embedding-ada-002").name, "text-embedding-3-small");
        // Known profiles keep their prefixes.
        assert_eq!(profile_for("nomic-embed-text-v1.5").name, "nomic-embed-text-v1.5");
    }

    #[test]
    fn wire_path_headers_and_body() {
        let resp_body = r#"{"data":[{"embedding":[0.0,2.0],"index":0}]}"#.to_string();
        let (base, handle) = spawn_stub(vec![(200, resp_body)]);
        let backend = CopilotEmbeddings::from_parts(
            base.clone(),
            stub_auth(&base),
            "text-embedding-3-small".into(),
            5,
            InputKind::Query,
        )
        .unwrap();
        let out = block_on(backend.embed(&["q".to_string()])).unwrap();
        assert_eq!(out, vec![vec![0.0, 1.0]]); // L2-normalized [0,2] → [0,1]
        let raw = handle.join().unwrap().remove(0);
        assert!(raw.starts_with("POST /embeddings HTTP/1.1"), "raw: {raw}");
        let lower = raw.to_lowercase();
        assert!(lower.contains("authorization: bearer test-key"), "raw: {raw}");
        assert!(lower.contains("copilot-integration-id: vscode-chat"), "raw: {raw}");
        assert!(lower.contains("editor-version: vscode/1.104.1"), "raw: {raw}");
        assert!(
            raw.contains("{\"model\":\"text-embedding-3-small\",\"input\":[\"q\"]}"),
            "raw: {raw}"
        );
    }

    #[test]
    fn known_profile_prefixes_are_applied() {
        let resp_body = r#"{"data":[{"embedding":[1.0],"index":0}]}"#.to_string();
        let (base, handle) = spawn_stub(vec![(200, resp_body)]);
        let backend = CopilotEmbeddings::from_parts(
            base.clone(),
            stub_auth(&base),
            "nomic-embed-text-v1.5".into(),
            5,
            InputKind::Query,
        )
        .unwrap();
        let _ = block_on(backend.embed(&["q".to_string()])).unwrap();
        let raw = handle.join().unwrap().remove(0);
        assert!(
            raw.contains("\"input\":[\"search_query: q\"]"),
            "raw: {raw}"
        );
    }

    #[test]
    fn order_restored_and_normalized() {
        let resp_body = r#"{"data":[
            {"embedding":[3.0,4.0],"index":1},
            {"embedding":[0.0,2.0],"index":0}
        ]}"#
            .replace(char::is_whitespace, "");
        let (base, handle) = spawn_stub(vec![(200, resp_body)]);
        let backend = CopilotEmbeddings::from_parts(
            base.clone(),
            stub_auth(&base),
            String::new(), // empty → DEFAULT_COPILOT_MODEL
            5,
            InputKind::Document,
        )
        .unwrap();
        assert_eq!(backend.describe_embedder().model, "text-embedding-3-small");
        let out = block_on(backend.embed(&["a".to_string(), "b".to_string()])).unwrap();
        assert_eq!(out.len(), 2);
        assert!((out[0][0] - 0.0).abs() < 1e-6 && (out[0][1] - 1.0).abs() < 1e-6);
        assert!((out[1][0] - 0.6).abs() < 1e-6 && (out[1][1] - 0.8).abs() < 1e-6);
        let _ = handle.join().unwrap();
    }

    #[test]
    fn auth_error_maps_to_auth_rejected() {
        let (base, handle) = spawn_stub(vec![(401, "{\"error\":\"unauthorized\"}".to_string())]);
        let backend = CopilotEmbeddings::from_parts(
            base.clone(),
            stub_auth(&base),
            "text-embedding-3-small".into(),
            5,
            InputKind::Query,
        )
        .unwrap();
        let err = block_on(backend.embed(&["q".to_string()])).unwrap_err();
        assert!(matches!(err, EmbedError::AuthRejected(_)), "err: {err:?}");
        let _ = handle.join().unwrap();
    }

    #[test]
    fn missing_credential_is_actionable_auth_rejected() {
        let backend = CopilotEmbeddings::from_parts(
            "https://api.githubcopilot.com".into(),
            Arc::new(CopilotAuth::new(String::new())),
            "text-embedding-3-small".into(),
            5,
            InputKind::Query,
        )
        .unwrap();
        let err = block_on(backend.embed(&["q".to_string()])).unwrap_err();
        assert!(matches!(err, EmbedError::AuthRejected(ref m) if m.contains("gh auth login")));
    }
}
