//! GitHub Copilot embeddings backend (REMOTE, explicit backend).
//!
//! `POST {base_url}/embeddings` — the Copilot models API's OpenAI-compatible
//! embeddings endpoint (NO `/v1` prefix). Activated ONLY by the explicit
//! `neurocode.rag.backend = copilot` setting — fully decoupled from
//! `model.provider` (works with any LLM provider). Auth reuses
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
//! Size limits: inputs are truncated to MAX_EMBED_INPUT_BYTES and batches
//! are split into sub-requests bounded by MAX_EMBED_REQUEST_BYTES /
//! MAX_EMBED_ITEMS_PER_REQUEST, keeping every request under upstream's
//! 8192-token per-input and 300,000-token per-request limits.
//!
//! Joey-native extension — upstream Hermes has no Copilot embeddings path.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use joey_providers::copilot::{custom_endpoint, CopilotAuth};

use crate::embed::local_onnx::InputKind;
use crate::embed::openai_compat::{
    l2_normalize, map_send_error, map_status_with_body, prefixed_input, EmbeddingData,
    EmbeddingsRequest, EmbeddingsResponse,
};
use crate::embed::profiles::{self, EmbedProfile, Pooling, TEXT_EMBEDDING_3_SMALL};
use crate::embed::{BackendKind, EmbedError, EmbedPrefixes, EmbedderInfo, EmbeddingBackend};

/// Wire path appended to the resolved base URL — NO `/v1` prefix (the
/// Copilot models API serves `/embeddings` directly).
pub const COPILOT_EMBEDDINGS_PATH: &str = "/embeddings";

/// Default upstream base (mirrors joey-providers' COPILOT_BASE_URL).
pub const DEFAULT_COPILOT_BASE_URL: &str = "https://api.githubcopilot.com";

/// Default base for the GitHub-native (dotcom) embeddings path — the ONLY
/// endpoint that serves the Metis family (`metis-1024-I16-Binary`).
pub const COPILOT_DOTCOM_EMBEDDINGS_BASE_URL: &str = "https://api.github.com";
const DOTCOM_BASE_URL_ENV: &str = "COPILOT_DOTCOM_EMBEDDINGS_BASE_URL";

/// CAPI-served embeddings model used when a pinned custom endpoint
/// (proxy) cannot serve the GitHub-native (Metis/dotcom) API.
pub const CAPI_EMBEDDINGS_MODEL: &str = "text-embedding-3-small";

/// Per-input cap: upstream rejects longer inputs with HTTP 400
/// (`Invalid 'input[0]': maximum input length is 8192 tokens.`) — ~3
/// bytes/token worst case for code/CJK, with margin.
const MAX_EMBED_INPUT_BYTES: usize = 23_000;

/// Per-request cap: upstream rejects bigger requests with HTTP 400
/// (`Invalid 'input': maximum request size is 300000 tokens per
/// request.`) — 300,000 tokens at ~3 bytes/token, with margin.
const MAX_EMBED_REQUEST_BYTES: usize = 850_000;

/// Item cap per sub-request — preserves today's single-request batch
/// shape for normal batches (callers batch at 16–128 items).
const MAX_EMBED_ITEMS_PER_REQUEST: usize = 64;

/// GitHub-native request body for the dotcom embeddings endpoint.
#[derive(serde::Serialize)]
struct NativeEmbeddingsRequest<'a> {
    inputs: &'a [String],
    input_type: &'a str,
    embedding_model: &'a str,
}

/// GitHub-native response: vectors are POSITIONAL (no `index` field).
#[derive(serde::Deserialize)]
struct NativeEmbeddingsResponse {
    #[allow(dead_code)]
    embedding_model: String,
    embeddings: Vec<NativeEmbeddingData>,
}

#[derive(serde::Deserialize)]
struct NativeEmbeddingData {
    embedding: Vec<f32>,
}

/// Metis-family models are served by the GitHub dotcom embeddings
/// endpoint with the GitHub-native request body and the RAW Copilot
/// credential (`Authorization: token …`) — the OpenAI-style CAPI
/// endpoint rejects them with `model_not_supported`.
fn is_native_copilot_model(model: &str) -> bool {
    model.starts_with("metis")
}

/// Resolve the effective embeddings model for the active endpoint mode.
/// A pinned custom endpoint (proxy) serves ONLY the OpenAI-style CAPI
/// embeddings API: an empty or Metis-family model resolves to
/// `text-embedding-3-small` there (the proxy 400s the native dotcom body);
/// with no proxy the Metis default targets api.github.com directly.
pub fn resolve_model_for_endpoint(model: &str, proxy_pinned: bool) -> String {
    let trimmed = model.trim();
    if proxy_pinned && (trimmed.is_empty() || is_native_copilot_model(trimmed)) {
        return CAPI_EMBEDDINGS_MODEL.to_string();
    }
    if trimmed.is_empty() {
        return crate::config::DEFAULT_COPILOT_MODEL.to_string();
    }
    trimmed.to_string()
}

/// The effective embeddings model for THIS process given the ambient
/// endpoint mode: a pinned custom endpoint (proxy) resolves empty and
/// Metis-family ids to the CAPI-served default, exactly like
/// [`CopilotEmbeddings::with_kind`] does at construction.
pub fn effective_model(model: &str) -> String {
    resolve_model_for_endpoint(model, joey_providers::copilot::custom_endpoint().is_some())
}

/// Serializes tests that mutate the endpoint env vars
/// (COPILOT_API_BASE_URL / AI_USAGE_HUD_BASE_URL) — the same convention as
/// joey-providers' `copilot::TEST_ENV_LOCK`. Both this module's tests and
/// `embed::tests::resolve_constructs_gated_copilot_backend` must hold it,
/// otherwise a parallel `set_var` can land inside another test's
/// scrubbed-env window (observed: metis assert failing with
/// text-embedding-3-small).
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

/// Longest prefix of `s` that is at most `max_bytes` bytes AND ends on a
/// UTF-8 char boundary (never splits a multibyte character).
fn truncate_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut cut = 0;
    for (i, _) in s.char_indices() {
        if i > max_bytes {
            break;
        }
        cut = i;
    }
    &s[..cut]
}

/// Greedy [start, end) ranges over `inputs` such that each range carries
/// at most MAX_EMBED_ITEMS_PER_REQUEST items and at most
/// MAX_EMBED_REQUEST_BYTES of input text: extend the current range until
/// adding the next input would breach either cap, then close it. Inputs
/// are pre-truncated to MAX_EMBED_INPUT_BYTES so one always fits, but an
/// input that alone exceeds the byte cap still gets its own range —
/// progress is guaranteed, never an infinite loop.
fn split_embed_batch(inputs: &[String]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut start = 0;
    while start < inputs.len() {
        let mut end = start;
        let mut bytes = 0;
        while end < inputs.len() {
            let next_len = inputs[end].len();
            let items_full = end - start >= MAX_EMBED_ITEMS_PER_REQUEST;
            let bytes_full = end > start && bytes + next_len > MAX_EMBED_REQUEST_BYTES;
            if items_full || bytes_full {
                break;
            }
            bytes += next_len;
            end += 1;
        }
        ranges.push((start, end));
        start = end;
    }
    ranges
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
    /// Base for the GitHub-native (dotcom) path: env → pinned custom
    /// endpoint → the api.github.com default. A pinned/custom endpoint
    /// serves the native body itself.
    native_base_url: String,
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
        let proxy_pinned = pinned.is_some();
        let base_url = pinned.clone().unwrap_or_else(resolve_base_url);
        let native_base_url = pinned
            .clone()
            .or_else(|| {
                std::env::var(DOTCOM_BASE_URL_ENV)
                    .ok()
                    .filter(|v| !v.trim().is_empty())
            })
            .unwrap_or_else(|| COPILOT_DOTCOM_EMBEDDINGS_BASE_URL.to_string());
        let auth = shared_auth(raw_token, pinned);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(1) as u64))
            .build()
            .map_err(|e| EmbedError::Other(format!("http client build: {}", e)))?;
        let model = resolve_model_for_endpoint(&model, proxy_pinned);
        let profile = profile_for(&model);
        Ok(Self { http, base_url, native_base_url, model, auth, profile, kind })
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

    /// The full request URL for the GitHub-native path.
    fn native_endpoint(base: &str) -> String {
        format!("{}{}", base.trim_end_matches('/'), COPILOT_EMBEDDINGS_PATH)
    }

    /// Embed via the GitHub-native dotcom endpoint (`/embeddings` at
    /// api.github.com): native body (`inputs`/`input_type`/
    /// `embedding_model`), positional response vectors, and the RAW
    /// Copilot credential (`Authorization: token …`) — NEVER the
    /// exchanged token, NEVER bearer auth.
    async fn embed_native(
        &self,
        batch: &[String],
        batch_len: usize,
    ) -> Result<Vec<Vec<f32>>, EmbedError> {
        if !self.auth.has_raw_token() {
            return Err(EmbedError::AuthRejected(AUTH_HINT.to_string()));
        }
        let token = self.auth.raw_token().to_string();
        let input_type = match self.kind {
            InputKind::Query => "query",
            InputKind::Document => "document",
        };
        let url = Self::native_endpoint(&self.native_base_url);
        let body = NativeEmbeddingsRequest {
            inputs: batch,
            input_type,
            embedding_model: &self.model,
        };
        let mut req = self.http.post(&url).json(&body);
        for (name, value) in copilot_request_headers() {
            req = req.header(name, value);
        }
        req = req.header("authorization", format!("token {}", token));
        let resp = req.send().await.map_err(|e| map_send_error(e, &url))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(map_status_with_body(status, &url, &body));
        }
        let parsed: NativeEmbeddingsResponse =
            resp.json().await.map_err(|e| map_send_error(e, &url))?;

        // Positional order (no `index` field): vector count must match
        // the input count exactly.
        if parsed.embeddings.len() != batch_len {
            return Err(EmbedError::MalformedResponse(format!(
                "wrong count: {} vectors for {} inputs",
                parsed.embeddings.len(),
                batch_len
            )));
        }
        let mut out: Vec<Vec<f32>> = parsed
            .embeddings
            .into_iter()
            .map(|d| d.embedding)
            .collect();
        for v in out.iter_mut() {
            l2_normalize(v);
        }
        Ok(out)
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

    /// Send ONE OpenAI-dialect embeddings sub-request and parse the
    /// response — the per-request send/parse half extracted from `embed`
    /// so batch splitting REUSES (not duplicates) the wire behavior:
    /// same body shape, Copilot headers, bearer auth, and error mapping
    /// as the former single-request path. Response indexes are
    /// SLICE-relative; `embed` offsets them back to batch positions.
    async fn post_embeddings_slice(
        &self,
        token: &str,
        url: &str,
        slice: &[String],
    ) -> Result<EmbeddingsResponse, EmbedError> {
        let body = EmbeddingsRequest { model: &self.model, input: slice };
        let mut req = self.http.post(url).json(&body);
        for (name, value) in copilot_request_headers() {
            req = req.header(name, value);
        }
        req = req.bearer_auth(token);
        let resp = req.send().await.map_err(|e| map_send_error(e, url))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(map_status_with_body(status, url, &body));
        }
        resp.json().await.map_err(|e| map_send_error(e, url))
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
        Ok(Self { http, native_base_url: base_url.clone(), base_url, model, auth, profile, kind })
    }
}

#[async_trait::async_trait]
impl EmbeddingBackend for CopilotEmbeddings {
    async fn embed(&self, batch: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let prefixed: Vec<String> = batch
            .iter()
            .map(|t| prefixed_input(self.profile, t, self.kind))
            .collect();
        if is_native_copilot_model(&self.model) {
            return self.embed_native(&prefixed, batch.len()).await;
        }
        // Size-limit handling: truncate each input, then split the batch
        // into budgeted sub-requests (the embed-backend contract allows
        // the backend to split internally — see openai_compat §Batching).
        // Truncation does NOT change the item count.
        let truncated: Vec<String> = prefixed
            .into_iter()
            .map(|s| {
                if s.len() <= MAX_EMBED_INPUT_BYTES {
                    s
                } else {
                    truncate_char_boundary(&s, MAX_EMBED_INPUT_BYTES).to_string()
                }
            })
            .collect();
        let ranges = split_embed_batch(&truncated);
        let (token, base) = self.token_and_base().await?;
        let url = Self::endpoint(&base);
        let mut items: Vec<EmbeddingData> = Vec::with_capacity(batch.len());
        let mut offset = 0usize;
        for (start, end) in ranges {
            let parsed =
                self.post_embeddings_slice(&token, &url, &truncated[start..end]).await?;
            for mut item in parsed.data {
                item.index += offset; // slice-relative → batch-absolute
                items.push(item);
            }
            offset += end - start;
        }

        // Order preservation via the `index` field — identical contract to
        // the OpenAI-compat backend (count/bounds/duplicate violations are
        // MalformedResponse).
        let mut slots: Vec<Option<Vec<f32>>> = vec![None; batch.len()];
        for item in items {
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
            "text-embedding-3-small".into(), // explicit → OpenAI path
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
    fn empty_model_defaults_to_metis() {
        let backend = CopilotEmbeddings::from_parts(
            "https://api.github.com".into(),
            stub_auth("https://api.github.com"),
            String::new(), // empty → DEFAULT_COPILOT_MODEL
            5,
            InputKind::Query,
        )
        .unwrap();
        let info = backend.describe_embedder();
        assert_eq!(info.model, "metis-1024-I16-Binary");
        assert_eq!(info.dim, 1024);
    }

    #[test]
    fn metis_native_wire_path_headers_and_body() {
        let resp_body =
            r#"{"embedding_model":"metis-1024-I16-Binary","embeddings":[{"embedding":[0.0,2.0]}]}"#
                .to_string();
        let (base, handle) = spawn_stub(vec![(200, resp_body)]);
        let backend = CopilotEmbeddings::from_parts(
            base.clone(),
            stub_auth(&base),
            "metis-1024-I16-Binary".into(),
            5,
            InputKind::Query,
        )
        .unwrap();
        let out = block_on(backend.embed(&["q".to_string()])).unwrap();
        assert_eq!(out, vec![vec![0.0, 1.0]]); // L2-normalized [0,2] → [0,1]
        let raw = handle.join().unwrap().remove(0);
        assert!(raw.starts_with("POST /embeddings HTTP/1.1"), "raw: {raw}");
        let lower = raw.to_lowercase();
        assert!(lower.contains("authorization: token test-key"), "raw: {raw}");
        assert!(lower.contains("copilot-integration-id: vscode-chat"), "raw: {raw}");
        assert!(lower.contains("editor-version: vscode/1.104.1"), "raw: {raw}");
        assert!(
            raw.contains("\"embedding_model\":\"metis-1024-I16-Binary\""),
            "raw: {raw}"
        );
        assert!(raw.contains("\"input_type\":\"query\""), "raw: {raw}");
        assert!(raw.contains("\"inputs\":[\"q\"]"), "raw: {raw}");
    }

    #[test]
    fn metis_native_count_mismatch_is_malformed() {
        // 2 inputs, 1 embedding → positional count violation.
        let resp_body =
            r#"{"embedding_model":"metis-1024-I16-Binary","embeddings":[{"embedding":[0.1]}]}"#
                .to_string();
        let (base, handle) = spawn_stub(vec![(200, resp_body)]);
        let backend = CopilotEmbeddings::from_parts(
            base.clone(),
            stub_auth(&base),
            "metis-1024-I16-Binary".into(),
            5,
            InputKind::Query,
        )
        .unwrap();
        let err = block_on(backend.embed(&["a".to_string(), "b".to_string()])).unwrap_err();
        assert!(matches!(err, EmbedError::MalformedResponse(_)), "err: {err:?}");
        let _ = handle.join().unwrap();
    }

    #[test]
    fn resolve_model_for_endpoint_proxy_aware() {
        assert_eq!(resolve_model_for_endpoint("", false), "metis-1024-I16-Binary");
        assert_eq!(resolve_model_for_endpoint("", true), "text-embedding-3-small");
        assert_eq!(resolve_model_for_endpoint("metis-1024-I16-Binary", true), "text-embedding-3-small");
        assert_eq!(resolve_model_for_endpoint("metis-1024-I16-Binary", false), "metis-1024-I16-Binary");
        assert_eq!(resolve_model_for_endpoint("  text-embedding-3-small ", true), "text-embedding-3-small");
        assert_eq!(resolve_model_for_endpoint("nomic-embed-text-v1.5", true), "nomic-embed-text-v1.5");
    }

    #[test]
    fn effective_model_tracks_pinned_proxy() {
        // Hermetic: pin/remove the endpoint envs for the duration (same
        // save/remove/restore pattern as resolve_constructs_gated_copilot_backend).
        // Serialized on TEST_ENV_LOCK — a sibling test's concurrent set_var
        // must not land inside this test's scrubbed-env window.
        let _lock = crate::embed::copilot::TEST_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved_api = std::env::var("COPILOT_API_BASE_URL").ok();
        let saved_hud = std::env::var("AI_USAGE_HUD_BASE_URL").ok();
        std::env::remove_var("COPILOT_API_BASE_URL");
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
        assert_eq!(effective_model(""), "metis-1024-I16-Binary");
        assert_eq!(effective_model("metis-1024-I16-Binary"), "metis-1024-I16-Binary");
        std::env::set_var("AI_USAGE_HUD_BASE_URL", "http://127.0.0.1:9317");
        assert_eq!(effective_model(""), "text-embedding-3-small");
        assert_eq!(effective_model("metis-1024-I16-Binary"), "text-embedding-3-small");
        assert_eq!(effective_model("text-embedding-3-small"), "text-embedding-3-small");
        std::env::remove_var("AI_USAGE_HUD_BASE_URL");
        if let Some(v) = saved_api { std::env::set_var("COPILOT_API_BASE_URL", v); }
        if let Some(v) = saved_hud { std::env::set_var("AI_USAGE_HUD_BASE_URL", v); }
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

    #[test]
    fn http_400_body_detail_reaches_caller() {
        let (base, handle) = spawn_stub(vec![(
            400,
            r#"{"error":{"message":"Copilot API error: HTTP 400 — Bad Request","type":"invalid_request_error","code":"bad_request"}}"#
                .to_string(),
        )]);
        let backend = CopilotEmbeddings::from_parts(
            base.clone(),
            stub_auth(&base),
            "text-embedding-3-small".into(),
            5,
            InputKind::Query,
        )
        .unwrap();
        let err = block_on(backend.embed(&["q".to_string()])).unwrap_err();
        assert!(matches!(err, EmbedError::Unreachable(_)), "err: {err:?}");
        assert!(
            err.to_string().contains("Copilot API error: HTTP 400 — Bad Request"),
            "err: {err}"
        );
        let _ = handle.join().unwrap();
    }

    #[test]
    fn oversized_input_is_truncated() {
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
        let input = "a".repeat(100_000);
        let out = block_on(backend.embed(&[input])).unwrap();
        assert_eq!(out.len(), 1);
        let raw = handle.join().unwrap().remove(0);
        assert!(raw.starts_with("POST /embeddings HTTP/1.1"), "raw: {raw}");
        let body = raw.split_once("\r\n\r\n").unwrap().1;
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        let got = parsed["input"][0].as_str().unwrap();
        assert_eq!(got.len(), MAX_EMBED_INPUT_BYTES, "input[0] len: {}", got.len());
        assert!(got.chars().all(|c| c == 'a'));
    }

    #[test]
    fn large_batch_splits_into_budgeted_subrequests() {
        // 100 x 20_000 B: byte cap admits 42 per request (43 x 20_000 >
        // 850_000) -> ceil(100/42) = 3 sequential POSTs. The stub's
        // canned slices must mirror that exact greedy split, with
        // SLICE-relative `index` fields offset per slice.
        let enc = |i: usize| {
            // Direction encodes the input index; survives L2
            // normalization (unlike magnitude encodings).
            let v = [(i + 1) as f32, 1.0];
            format!("[{},{}]", v[0], v[1])
        };
        let slice_body = |base_index: usize, n: usize| {
            let items = (0..n)
                .map(|k| {
                    format!(
                        r#"{{"embedding":{},"index":{}}}"#,
                        enc(base_index + k),
                        k
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(r#"{{"data":[{}]}}"#, items)
        };
        let responses = vec![
            (200, slice_body(0, 42)),
            (200, slice_body(42, 42)),
            (200, slice_body(84, 16)),
        ];
        let (base, handle) = spawn_stub(responses);
        let backend = CopilotEmbeddings::from_parts(
            base.clone(),
            stub_auth(&base),
            "text-embedding-3-small".into(),
            5,
            InputKind::Query,
        )
        .unwrap();
        let batch: Vec<String> = (0..100).map(|_| "b".repeat(20_000)).collect();
        let out = block_on(backend.embed(&batch)).unwrap();
        assert_eq!(out.len(), 100);

        let recorded = handle.join().unwrap();
        assert!(recorded.len() >= 3, "posts: {}", recorded.len());
        for raw in &recorded {
            let body = raw.split_once("\r\n\r\n").unwrap().1;
            assert!(
                body.len() <= MAX_EMBED_REQUEST_BYTES + 2_000,
                "body len: {}",
                body.len()
            );
            let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
            let n = parsed["input"].as_array().unwrap().len();
            assert!(n <= MAX_EMBED_ITEMS_PER_REQUEST, "inputs per post: {n}");
        }

        // Order: normalized [(i+1), 1] has first component
        // (i+1)/sqrt((i+1)^2+1) — strictly increasing in i.
        for w in out.windows(2) {
            assert!(w[1][0] > w[0][0], "order violated: {} !< {}", w[0][0], w[1][0]);
        }
    }

    #[test]
    fn normal_batch_remains_single_request() {
        let resp_body = r#"{"data":[
            {"embedding":[1.0,0.0],"index":0},
            {"embedding":[0.0,1.0],"index":1},
            {"embedding":[1.0,1.0],"index":2}
        ]}"#
            .replace(char::is_whitespace, "");
        let (base, handle) = spawn_stub(vec![(200, resp_body)]);
        let backend = CopilotEmbeddings::from_parts(
            base.clone(),
            stub_auth(&base),
            "text-embedding-3-small".into(),
            5,
            InputKind::Document,
        )
        .unwrap();
        let out = block_on(backend.embed(&["x".to_string(), "y".to_string(), "z".to_string()]))
            .unwrap();
        assert_eq!(out.len(), 3);
        let recorded = handle.join().unwrap();
        assert_eq!(recorded.len(), 1, "posts: {}", recorded.len());
        let body = recorded[0].split_once("\r\n\r\n").unwrap().1;
        assert_eq!(
            body,
            r#"{"model":"text-embedding-3-small","input":["x","y","z"]}"#
        );
    }

    #[test]
    fn truncate_char_boundary_cuts_on_char_boundary() {
        // Under limit: unchanged.
        let ascii = "hello";
        assert_eq!(truncate_char_boundary(ascii, 10), ascii);
        // Over limit, ASCII: exactly max bytes.
        let long = "a".repeat(100);
        let cut = truncate_char_boundary(&long, 23);
        assert_eq!(cut.len(), 23);
        // Multibyte: never splits a UTF-8 char; prefix round-trips.
        let s = "日本語日本語"; // 3 bytes per char, 18 bytes total
        let cut = truncate_char_boundary(s, 7);
        assert!(cut.len() <= 7);
        assert!(s.is_char_boundary(cut.len()));
        assert_eq!(cut, "日本"); // 6 bytes — next char would end at 9 > 7
        let _: &str = cut; // round-trips as &str by construction
    }

    #[test]
    fn split_embed_batch_ranges() {
        // Empty input -> no ranges.
        assert!(split_embed_batch(&[]).is_empty());
        // 64 small inputs fit one range.
        let small: Vec<String> = (0..64).map(|i| i.to_string()).collect();
        assert_eq!(split_embed_batch(&small), vec![(0, 64)]);
        // 65 -> 2 ranges (item cap binds first).
        let small65: Vec<String> = (0..65).map(|i| i.to_string()).collect();
        assert_eq!(split_embed_batch(&small65), vec![(0, 64), (64, 65)]);
        // 100 x 20_000 B: every range within both caps, coverage exact,
        // contiguous, in order.
        let big: Vec<String> = (0..100).map(|_| "b".repeat(20_000)).collect();
        let ranges = split_embed_batch(&big);
        let mut covered = 0usize;
        let mut cursor = 0usize;
        for (start, end) in &ranges {
            assert_eq!(*start, cursor, "ranges must be contiguous/in order");
            let items = end - start;
            assert!(items <= MAX_EMBED_ITEMS_PER_REQUEST, "items: {items}");
            let bytes: usize = big[*start..*end].iter().map(|s| s.len()).sum();
            assert!(bytes <= MAX_EMBED_REQUEST_BYTES, "bytes: {bytes}");
            covered += items;
            cursor = *end;
        }
        assert_eq!(covered, 100);
        assert_eq!(cursor, 100);
    }
}
