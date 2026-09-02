//! Ollama-native HTTP embedding backend (SECONDARY, daemon path).
//!
//! Thin second impl giving truncate/keep_alive control:
//! `POST {base_url}/api/embed` (contract §3). Same structural error
//! mapping, timeout, profile prefixes, and client-side L2 as the
//! OpenAI-compatible backend ([`super::openai_compat`] — shared helpers);
//! Bearer auth from `neurocode.rag.api_key` when non-empty.
//!
//! Contract bindings that differ from §2:
//!
//! - **Order preservation** is by ARRAY ORDER — `embeddings[i]`
//!   corresponds to `input[i]` positionally; there is no index field.
//! - A loopback `base_url` counts as LOCAL unconditionally (consent-free,
//!   FR-012/T032 — see the gate in [`super`] which keys off the host).
//! - **truncate: true** — inputs longer than the model's context are
//!   truncated server-side rather than erroring.
//! - **keep_alive** — the configured `neurocode.rag.timeout_secs`-style
//!   duration string carried verbatim on the wire (pinned shape:
//!   `"<configured>"`); it defaults to Ollama's own default when empty.
//!
//! Wire JSON pinned byte-for-byte by the tests below (contract §3):
//!
//! ```json
//! {"model": "<model>", "input": ["<s1>", "<s2>"], "truncate": true,
//!  "keep_alive": "<configured>"}
//! ```

use serde::{Deserialize, Serialize};

use crate::embed::local_onnx::InputKind;
use crate::embed::openai_compat::{
    l2_normalize, map_send_error, map_status_with_body, prefixed_input,
};
use crate::embed::profiles::{default_profile, lookup, EmbedProfile, Pooling};
use crate::embed::{BackendKind, EmbedError, EmbedPrefixes, EmbedderInfo, EmbeddingBackend};

/// Wire path appended to the configured `base_url`.
pub const EMBED_PATH: &str = "/api/embed";

/// Default `keep_alive` duration string when none is configured
/// (Ollama's own server default behavior — `5m`, its documented default).
pub const DEFAULT_KEEP_ALIVE: &str = "5m";

// ---------------------------------------------------------------------------
// Wire shapes (contract §3 — pinned byte-for-byte)
// ---------------------------------------------------------------------------

/// Request wire shape — PINNED (contract §3): fields serialize in
/// declaration order `model`, `input`, `truncate`, `keep_alive`, exactly
/// the contract's field set.
///
/// `input` carries the PROFILE-PREFIXED texts (same rule as every other
/// backend: prefixes are the embedder's job; see
/// [`super::openai_compat`] module docs).
#[derive(Debug, Serialize)]
pub struct EmbedRequest<'a> {
    /// Model identity (`neurocode.rag.model`).
    pub model: &'a str,
    /// Prefixed input texts, in batch order.
    pub input: &'a [String],
    /// Always `true` — over-long inputs truncate server-side.
    pub truncate: bool,
    /// How long the model stays loaded, e.g. `"5m"` (`"<configured>"`).
    pub keep_alive: &'a str,
}

/// Response wire shape — PINNED (contract §3):
///
/// ```json
/// {"embeddings": [[0.0123, -0.0456], [0.0789, 0.0321]]}
/// ```
///
/// Order is by array position (no index field). Unknown fields are
/// tolerated — the contract pins what we parse.
#[derive(Debug, Deserialize)]
pub struct EmbedResponse {
    /// One vector per input, positionally aligned.
    pub embeddings: Vec<Vec<f32>>,
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// Ollama-native embedding backend (contract §3).
///
/// Construct via [`Self::documents`] (indexing pipelines) or
/// [`Self::query`] (search legs) — the kind-at-construction adapter
/// pattern shared with [`crate::embed::LocalOnnxBackend`] and
/// [`crate::embed::openai_compat::OpenAiCompat`].
pub struct OllamaNative {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    keep_alive: String,
    profile: &'static EmbedProfile,
    kind: InputKind,
}

impl OllamaNative {
    fn with_kind(
        base_url: String,
        model: String,
        api_key: String,
        keep_alive: String,
        timeout_secs: i64,
        kind: InputKind,
    ) -> Result<Self, EmbedError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(timeout_secs.max(1) as u64))
            .build()
            .map_err(|e| EmbedError::Other(format!("http client build: {}", e)))?;
        let keep_alive =
            if keep_alive.trim().is_empty() { DEFAULT_KEEP_ALIVE.to_string() } else { keep_alive };
        let profile = lookup(&model).unwrap_or_else(default_profile);
        Ok(Self { http, base_url, model, api_key, keep_alive, profile, kind })
    }

    /// Adapter applying the profile DOCUMENT prefix (indexing pipelines).
    pub fn documents(
        base_url: String,
        model: String,
        api_key: String,
        keep_alive: String,
        timeout_secs: i64,
    ) -> Result<Self, EmbedError> {
        Self::with_kind(base_url, model, api_key, keep_alive, timeout_secs, InputKind::Document)
    }

    /// Adapter applying the profile QUERY prefix (search legs).
    pub fn query(
        base_url: String,
        model: String,
        api_key: String,
        keep_alive: String,
        timeout_secs: i64,
    ) -> Result<Self, EmbedError> {
        Self::with_kind(base_url, model, api_key, keep_alive, timeout_secs, InputKind::Query)
    }

    /// The kind this adapter embeds as (for diagnostics).
    pub fn input_kind(&self) -> InputKind {
        self.kind
    }

    /// The full request URL: `{base_url}/api/embed` (trailing `/` on the
    /// configured base_url tolerated).
    fn endpoint(&self) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), EMBED_PATH)
    }
}

#[async_trait::async_trait]
impl EmbeddingBackend for OllamaNative {
    async fn embed(&self, batch: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        // Empty input → empty output, no request (mirrors LocalOnnx).
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let prefixed: Vec<String> = batch
            .iter()
            .map(|t| prefixed_input(self.profile, t, self.kind))
            .collect();
        let body = EmbedRequest {
            model: &self.model,
            input: &prefixed,
            truncate: true,
            keep_alive: &self.keep_alive,
        };
        let url = self.endpoint();
        let mut req = self.http.post(&url).json(&body);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req.send().await.map_err(|e| map_send_error(e, &url))?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(map_status_with_body(status, &url, &body));
        }
        let parsed: EmbedResponse = resp.json().await.map_err(|e| map_send_error(e, &url))?;

        // Order preservation is by ARRAY ORDER (contract § Batching).
        if parsed.embeddings.len() != batch.len() {
            return Err(EmbedError::MalformedResponse(format!(
                "wrong count: {} embeddings for {} inputs",
                parsed.embeddings.len(),
                batch.len()
            )));
        }
        let mut out = parsed.embeddings;
        for v in out.iter_mut() {
            l2_normalize(v);
        }
        Ok(out)
    }

    fn describe_embedder(&self) -> EmbedderInfo {
        EmbedderInfo {
            backend_kind: BackendKind::OllamaNative,
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
    use super::super::openai_compat::test_util::{spawn_stub, split_request};
    use super::*;
    use crate::embed::profiles::CODERANK_EMBED;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    }

    fn doc_backend(base_url: &str, keep_alive: &str) -> OllamaNative {
        OllamaNative::documents(
            base_url.to_string(),
            CODERANK_EMBED.name.to_string(),
            String::new(),
            keep_alive.to_string(),
            5,
        )
        .expect("construct backend")
    }

    // ── wire contract: request bytes pinned EXACTLY ─────────────────────

    /// Contract §3: `{"model": ..., "input": [...], "truncate": true,
    /// "keep_alive": "<configured>"}` — field set, field ORDER, booleans
    /// lowercase, no whitespace, nothing extra.
    #[test]
    fn ollama_request_wire_json_is_pinned_byte_for_byte() {
        let input = vec![
            "Represent this query for searching relevant code: parse".to_string(),
            "fn main()".to_string(),
        ];
        let req = EmbedRequest {
            model: "CodeRankEmbed",
            input: &input,
            truncate: true,
            keep_alive: "10m",
        };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"model":"CodeRankEmbed","input":["Represent this query for searching relevant code: parse","fn main()"],"truncate":true,"keep_alive":"10m"}"#
        );
    }

    #[test]
    fn ollama_response_wire_shape_parses_contract_example() {
        let resp: EmbedResponse =
            serde_json::from_str(r#"{"embeddings":[[0.0123,-0.0456],[0.0789,0.0321]]}"#)
                .expect("contract example parses");
        assert_eq!(resp.embeddings.len(), 2);
        assert!((resp.embeddings[0][0] - 0.0123).abs() < 1e-9);
        // Tolerates real-server extras (model/load prompts).
        let resp: EmbedResponse =
            serde_json::from_str(r#"{"model":"x","embeddings":[[1.0]],"load_duration":1}"#)
                .expect("extra fields tolerated");
        assert_eq!(resp.embeddings[0], vec![1.0]);
    }

    // ── embed round-trip against the stub (loopback) ────────────────────

    /// POST /api/embed, body byte-exact (pinned field order incl.
    /// truncate + keep_alive), array-order preserved, vectors
    /// L2-normalized, no Authorization header when api_key empty.
    #[test]
    fn ollama_embed_posts_pinned_wire_array_order_and_normalizes() {
        let (base, handle) = spawn_stub(vec![(
            200,
            r#"{"embeddings":[[8.0,0.0],[0.0,5.0]]}"#.to_string(),
        )]);
        // CodeRankEmbed: query-only prefix; documents pass verbatim.
        let backend = doc_backend(&base, "10m");
        let out = rt()
            .block_on(backend.embed(&["fn a()".into(), "fn b()".into()]))
            .expect("embed");
        let raws = handle.join().expect("stub thread");
        assert_eq!(raws.len(), 1);
        let (line, headers, body) = split_request(&raws[0]);
        assert_eq!(line, "POST /api/embed HTTP/1.1");
        assert!(!headers.to_ascii_lowercase().contains("authorization"));
        assert_eq!(
            body,
            r#"{"model":"CodeRankEmbed","input":["fn a()","fn b()"],"truncate":true,"keep_alive":"10m"}"#
        );
        // Array order preserved; client-side L2: [8,0]→[1,0], [0,5]→[0,1].
        assert!((out[0][0] - 1.0).abs() < 1e-6 && out[0][1].abs() < 1e-6);
        assert!((out[1][0]).abs() < 1e-6 && (out[1][1] - 1.0).abs() < 1e-6);
    }

    /// Empty keep_alive configures the documented default (`5m`), on the
    /// wire verbatim; query kind applies CodeRankEmbed's query prefix.
    #[test]
    fn ollama_default_keep_alive_and_query_prefix_on_wire() {
        let (base, handle) = spawn_stub(vec![(
            200,
            r#"{"embeddings":[[2.0,0.0]]}"#.to_string(),
        )]);
        let backend = OllamaNative::query(
            base,
            CODERANK_EMBED.name.to_string(),
            String::new(),
            String::new(),
            5,
        )
        .unwrap();
        let out = rt().block_on(backend.embed(&["parse config".into()])).expect("embed");
        let (_, _, body) = split_request(&handle.join().unwrap()[0]);
        assert_eq!(
            body,
            r#"{"model":"CodeRankEmbed","input":["Represent this query for searching relevant code: parse config"],"truncate":true,"keep_alive":"5m"}"#
        );
        assert!((out[0][0] - 1.0).abs() < 1e-6);
    }

    /// Bearer auth IS sent when an api_key is configured (daemon behind
    /// a gateway) — same optional-header rule as §2.
    #[test]
    fn ollama_bearer_auth_sent_only_with_api_key() {
        let (base, handle) = spawn_stub(vec![(
            200,
            r#"{"embeddings":[[1.0]]}"#.to_string(),
        )]);
        let backend = OllamaNative::documents(
            base,
            CODERANK_EMBED.name.to_string(),
            "tok-42".to_string(),
            "5m".to_string(),
            5,
        )
        .unwrap();
        rt().block_on(backend.embed(&["x".into()])).expect("embed");
        let (_, headers, _) = split_request(&handle.join().unwrap()[0]);
        assert!(headers.to_ascii_lowercase().contains("bearer tok-42"));
    }

    // ── structural error mapping ────────────────────────────────────────

    #[test]
    fn ollama_http_error_statuses_map_structurally() {
        let cases: Vec<(u16, fn(&EmbedError) -> bool)> = vec![
            (401, |e| matches!(e, EmbedError::AuthRejected(_))),
            (403, |e| matches!(e, EmbedError::AuthRejected(_))),
            (429, |e| matches!(e, EmbedError::RateLimited(_))),
            (500, |e| matches!(e, EmbedError::Unreachable(_))),
        ];
        for (status, is_class) in cases {
            let (base, handle) = spawn_stub(vec![(status, "{}".to_string())]);
            let backend = doc_backend(&base, "5m");
            let err = rt()
                .block_on(backend.embed(&["x".into()]))
                .expect_err("must fail");
            assert!(is_class(&err), "HTTP {status} → wrong class: {err:?}");
            handle.join().unwrap();
        }
    }

    #[test]
    fn ollama_malformed_responses_map_structurally() {
        let cases: Vec<(&str, &str)> = vec![
            ("garbage", "not json"),
            (r#"{"nope":1}"#, "wrong shape"),
            // 2 inputs, 1 embedding — wrong count (array order).
            (r#"{"embeddings":[[1.0]]}"#, "wrong count"),
        ];
        for (body_src, why) in cases {
            let (base, handle) = spawn_stub(vec![(200, body_src.to_string())]);
            let backend = doc_backend(&base, "5m");
            let err = rt()
                .block_on(backend.embed(&["a".into(), "b".into()]))
                .expect_err(why);
            assert!(
                matches!(err, EmbedError::MalformedResponse(_)),
                "{why} → wrong class: {err:?}"
            );
            handle.join().unwrap();
        }
    }

    /// Empty batch: empty output, NO network (port 1 has nothing
    /// listening — an attempt would fail the call).
    #[test]
    fn ollama_empty_batch_returns_empty_without_network() {
        let backend = doc_backend("http://127.0.0.1:1", "5m");
        let out = rt().block_on(backend.embed(&[])).expect("empty ok");
        assert!(out.is_empty());
    }

    #[test]
    fn ollama_connection_refused_maps_to_unreachable() {
        let backend = doc_backend("http://127.0.0.1:1", "5m");
        let err = rt()
            .block_on(backend.embed(&["x".into()]))
            .expect_err("refused");
        assert!(matches!(err, EmbedError::Unreachable(_)), "{err:?}");
    }

    // ── descriptor / health ─────────────────────────────────────────────

    #[test]
    fn ollama_describe_embedder_is_static_and_profile_pinned() {
        let backend = OllamaNative::query(
            "http://localhost:11434".into(),
            CODERANK_EMBED.name.to_string(),
            String::new(),
            "5m".into(),
            1,
        )
        .unwrap();
        let info = backend.describe_embedder(); // no network I/O
        assert_eq!(info.backend_kind, BackendKind::OllamaNative);
        assert_eq!(info.profile_name, "CodeRankEmbed");
        assert_eq!(info.dim, 768);
        assert_eq!(info.pooling, Pooling::Mean);
        assert_eq!(info.prefixes.query, "Represent this query for searching relevant code: ");
        assert_eq!(info.prefixes.document, "");
        assert_eq!(info.base_url, "http://localhost:11434");
        assert_eq!(backend.input_kind(), InputKind::Query);
    }

    #[test]
    fn ollama_health_check_is_one_trivial_embed() {
        let (base, handle) =
            spawn_stub(vec![(200, r#"{"embeddings":[[1.0]]}"#.to_string())]);
        let backend = doc_backend(&base, "5m");
        rt().block_on(backend.health_check()).expect("health");
        let raws = handle.join().unwrap();
        let (_, _, body) = split_request(&raws[0]);
        // CodeRankEmbed document prefix is EMPTY — "health" goes verbatim.
        assert_eq!(
            body,
            r#"{"model":"CodeRankEmbed","input":["health"],"truncate":true,"keep_alive":"5m"}"#
        );
    }
}
