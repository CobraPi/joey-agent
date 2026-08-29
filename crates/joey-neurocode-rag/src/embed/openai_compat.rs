//! OpenAI-compatible HTTP embedding backend (SECONDARY, remote path).
//!
//! `POST {base_url}/v1/embeddings` with Bearer auth from
//! `neurocode.rag.api_key` (`Authorization: Bearer *** sent ONLY when
//! the key is non-empty — contract §2 "Optional header"); covers OpenAI,
//! Voyage (`api.voyageai.com/v1/embeddings`), and Ollama's `/v1/embeddings`
//! compatibility endpoint with zero per-vendor code (research R2). Wire
//! JSON pinned byte-for-byte by contracts/embedding-backend.md §2 —
//! [`EmbeddingsRequest`]/[`EmbeddingsResponse`] and the wire tests below.
//!
//! Contract bindings:
//!
//! - **Order preservation** (contract § Batching): the response `index`
//!   field restores input order — position `i` ↔ `batch[i]`, even if a
//!   transport shuffles `data`.
//! - **Prefixes** (Model Profiles rule 1): callers pass RAW text; the
//!   EMBEDDER applies the profile prefix before tokenization. For an HTTP
//!   backend tokenization happens server-side, so the prefixed string is
//!   exactly what goes on the wire — uniform with `LocalOnnx`, keeping a
//!   local index and a remote one comparable under the same profile.
//! - **Normalization** (trait surface + profile table): vectors are
//!   L2-normalized client-side before return ("mean pooling + L2
//!   (client-side)" — the server pools, the client normalizes).
//! - **Errors** (§ Error taxonomy): HTTP 401/403 → `AuthRejected`,
//!   429 → `RateLimited`, connect/timeout/DNS → `Unreachable`,
//!   JSON-shape / wrong-count / bad-index violations →
//!   `MalformedResponse`. The taxonomy has no server-error class; any
//!   other non-2xx means the backend is not serving → `Unreachable`.
//! - **Timeout**: `neurocode.rag.timeout_secs` bounds the whole request
//!   (connect + headers + body).
//! - **Batching**: the caller already batches at 16–128
//!   (`neurocode.rag.batch_size`); one request carries the whole slice
//!   (contract: the backend MAY split internally — with caller-side
//!   batching it never needs to).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::embed::local_onnx::InputKind;
use crate::embed::profiles::{default_profile, lookup, EmbedProfile, Pooling};
use crate::embed::{BackendKind, EmbedError, EmbedPrefixes, EmbedderInfo, EmbeddingBackend};

/// Wire path appended to the configured `base_url`.
pub const EMBEDDINGS_PATH: &str = "/v1/embeddings";

// ---------------------------------------------------------------------------
// Wire shapes (contract §2 — pinned byte-for-byte)
// ---------------------------------------------------------------------------

/// Request wire shape — PINNED EXACTLY (contract §2: "field set and
/// ordering as serialized"):
///
/// ```json
/// {"model": "<model>", "input": ["<chunk text 1>", "<chunk text 2>"]}
/// ```
///
/// serde serializes struct fields in declaration order with no whitespace,
/// reproducing the contract bytes exactly. `input` carries the
/// PROFILE-PREFIXED texts (see module docs — prefixes are the embedder's
/// job, applied before server-side tokenization).
#[derive(Debug, Serialize)]
pub struct EmbeddingsRequest<'a> {
    /// Model identity (`neurocode.rag.model`).
    pub model: &'a str,
    /// Prefixed input texts, in batch order.
    pub input: &'a [String],
}

/// Response wire shape — PINNED (contract §2):
///
/// ```json
/// {"data": [{"embedding": [0.0123, -0.0456], "index": 0}, ...]}
/// ```
///
/// Unknown fields (real servers add `object`/`usage`) are tolerated —
/// the contract pins what we PARSE, and the wire tests pin those bytes.
#[derive(Debug, Deserialize)]
pub struct EmbeddingsResponse {
    /// One item per input; `index` restores input order.
    pub data: Vec<EmbeddingData>,
}

/// One `data` item: the vector plus its input position.
#[derive(Debug, Deserialize)]
pub struct EmbeddingData {
    /// The embedding vector (L2-normalized client-side after decode).
    pub embedding: Vec<f32>,
    /// Which input this vector corresponds to (`batch[index]`).
    pub index: usize,
}

// ---------------------------------------------------------------------------
// Shared HTTP-backend helpers (used by ollama.rs too)
// ---------------------------------------------------------------------------

/// Map an HTTP status to the structural taxonomy (contract § Error
/// taxonomy): 401/403 → `AuthRejected`, 429 → `RateLimited`, any other
/// non-success → `Unreachable` (no server-error class exists; the backend
/// is not serving).
pub(crate) fn map_status(status: reqwest::StatusCode, url: &str) -> EmbedError {
    match status.as_u16() {
        401 | 403 => EmbedError::AuthRejected(format!("HTTP {} from {}", status.as_u16(), url)),
        429 => EmbedError::RateLimited(format!("HTTP 429 from {}", url)),
        other => EmbedError::Unreachable(format!("HTTP {} from {}", other, url)),
    }
}

/// Map a reqwest transport error: connect/timeout/DNS failures (and any
/// other send-time error — the request never produced a usable response)
/// → `Unreachable`; body/decode failures → `MalformedResponse`.
pub(crate) fn map_send_error(err: reqwest::Error, url: &str) -> EmbedError {
    if err.is_decode() || err.is_body() {
        EmbedError::MalformedResponse(format!("decoding response from {}: {}", url, err))
    } else {
        EmbedError::Unreachable(format!("request to {} failed: {}", url, err))
    }
}

/// In-place client-side L2 normalization (trait surface: "vectors are
/// normalized before return"). A zero vector stays zero — never NaN.
pub(crate) fn l2_normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Apply the profile prefix for `kind` to one raw text (contract Model
/// Profiles rule 1 — prefix ++ raw, verbatim).
pub(crate) fn prefixed_input(profile: &EmbedProfile, text: &str, kind: InputKind) -> String {
    match kind {
        InputKind::Query => profile.query_input(text),
        InputKind::Document => profile.document_input(text),
    }
}

/// Whether a `base_url` string's host is loopback — `127.0.0.1`, `::1`,
/// or `localhost` (contract § Consent gate names exactly these three).
///
/// Lives here (not in `mod.rs`) so the registry module stays free of any
/// direct reqwest code — its structural "auto resolution is
/// filesystem-only" guarantee scans for exactly that. Pure string parse
/// via `Url`; NO DNS resolution (a name that merely RESOLVES to loopback
/// is not loopback here). `Url::parse` lowercases ASCII hosts, so
/// `http://LOCALHOST:1` counts; anything unparsable is NOT loopback
/// (conservative — the consent gate then applies).
pub fn base_url_is_loopback(base_url: &str) -> bool {
    match reqwest::Url::parse(base_url) {
        Ok(url) => matches!(
            url.host_str().unwrap_or_default(),
            "127.0.0.1" | "::1" | "[::1]" | "localhost"
        ),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Backend
// ---------------------------------------------------------------------------

/// OpenAI-compatible HTTP embedding backend (contract §2).
///
/// Construct via [`Self::documents`] (indexing pipelines — profile
/// document prefix) or [`Self::query`] (search legs — query prefix),
/// mirroring `LocalOnnxBackend`'s kind-at-construction adapters: the
/// trait's single `embed` cannot know the kind, callers pick it once.
pub struct OpenAiCompat {
    http: reqwest::Client,
    base_url: String,
    model: String,
    api_key: String,
    profile: &'static EmbedProfile,
    kind: InputKind,
}

impl OpenAiCompat {
    fn with_kind(
        base_url: String,
        model: String,
        api_key: String,
        timeout_secs: i64,
        kind: InputKind,
    ) -> Result<Self, EmbedError> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs.max(1) as u64))
            .build()
            .map_err(|e| EmbedError::Other(format!("http client build: {}", e)))?;
        let profile = lookup(&model).unwrap_or_else(default_profile);
        Ok(Self { http, base_url, model, api_key, profile, kind })
    }

    /// Adapter applying the profile DOCUMENT prefix (indexing pipelines).
    pub fn documents(
        base_url: String,
        model: String,
        api_key: String,
        timeout_secs: i64,
    ) -> Result<Self, EmbedError> {
        Self::with_kind(base_url, model, api_key, timeout_secs, InputKind::Document)
    }

    /// Adapter applying the profile QUERY prefix (search legs).
    pub fn query(
        base_url: String,
        model: String,
        api_key: String,
        timeout_secs: i64,
    ) -> Result<Self, EmbedError> {
        Self::with_kind(base_url, model, api_key, timeout_secs, InputKind::Query)
    }

    /// The kind this adapter embeds as (for diagnostics).
    pub fn input_kind(&self) -> InputKind {
        self.kind
    }

    /// The full request URL: `{base_url}/v1/embeddings` (trailing `/` on
    /// the configured base_url tolerated).
    fn endpoint(&self) -> String {
        format!("{}{}", self.base_url.trim_end_matches('/'), EMBEDDINGS_PATH)
    }
}

#[async_trait::async_trait]
impl EmbeddingBackend for OpenAiCompat {
    async fn embed(&self, batch: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        // Empty input → empty output, no request (mirrors LocalOnnx).
        if batch.is_empty() {
            return Ok(Vec::new());
        }
        let prefixed: Vec<String> = batch
            .iter()
            .map(|t| prefixed_input(self.profile, t, self.kind))
            .collect();
        let body = EmbeddingsRequest { model: &self.model, input: &prefixed };
        let url = self.endpoint();
        let mut req = self.http.post(&url).json(&body);
        if !self.api_key.is_empty() {
            req = req.bearer_auth(&self.api_key);
        }
        let resp = req.send().await.map_err(|e| map_send_error(e, &url))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(map_status(status, &url));
        }
        let parsed: EmbeddingsResponse =
            resp.json().await.map_err(|e| map_send_error(e, &url))?;

        // Order preservation via the `index` field: slot each vector at its
        // input position; count/bounds/duplicate violations are wire-shape
        // violations → MalformedResponse (contract: "JSON shape violation,
        // wrong count").
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
            backend_kind: BackendKind::OpenAiCompat,
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

// ---------------------------------------------------------------------------
// Test infrastructure: a stub HTTP/1.1 server on 127.0.0.1 (loopback ⇒
// consent-free) serving canned responses and recording raw requests —
// the wire-contract obligation (byte-for-byte request assertions) runs
// against it with NO consent fixtures. Shared with ollama.rs's tests.
// ---------------------------------------------------------------------------
#[cfg(test)]
pub(crate) mod test_util {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// Spawn a stub server serving exactly one canned response per
    /// expected request (each on its own connection — `Connection:
    /// close`). Returns its loopback base_url and a join handle yielding
    /// the RAW request bytes (request line + headers + body) it received.
    pub(crate) fn spawn_stub(
        responses: Vec<(u16, String)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind stub server");
        let addr = listener.local_addr().expect("stub addr");
        let handle = std::thread::spawn(move || {
            let mut recorded = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("stub accept");
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(10)))
                    .expect("stub read timeout");
                let raw = read_request(&mut stream);
                recorded.push(raw.clone());
                let reason = match status {
                    200 => "OK",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    429 => "Too Many Requests",
                    _ => "Internal Server Error",
                };
                let resp = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status,
                    reason,
                    body.len(),
                    body
                );
                stream.write_all(resp.as_bytes()).expect("stub write");
                stream.flush().expect("stub flush");
            }
            recorded
        });
        (format!("http://{}", addr), handle)
    }

    /// Read one full HTTP/1.1 request (headers + Content-Length body).
    fn read_request(stream: &mut std::net::TcpStream) -> String {
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        let header_end = loop {
            if let Some(pos) = find_header_end(&buf) {
                break pos;
            }
            let n = stream.read(&mut chunk).expect("stub read headers");
            if n == 0 {
                panic!("client closed before sending full headers");
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let content_length = headers
            .lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                if k.trim().eq_ignore_ascii_case("content-length") {
                    v.trim().parse::<usize>().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0);
        while buf.len() < header_end + 4 + content_length {
            let n = stream.read(&mut chunk).expect("stub read body");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    /// Split a raw request into (request line, header lines, body).
    pub(crate) fn split_request(raw: &str) -> (String, String, String) {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default().to_string();
        let headers = lines.collect::<Vec<_>>().join("\n");
        (request_line, headers, body.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::test_util::{spawn_stub, split_request};
    use super::*;
    use crate::embed::profiles::NOMIC_EMBED_TEXT_V1_5;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    }

    fn doc_backend(base_url: &str, api_key: &str) -> OpenAiCompat {
        OpenAiCompat::documents(
            base_url.to_string(),
            NOMIC_EMBED_TEXT_V1_5.name.to_string(),
            api_key.to_string(),
            5,
        )
        .expect("construct backend")
    }

    // ── wire contract: request bytes pinned EXACTLY ─────────────────────

    /// Contract §2: `{"model": "<model>", "input": [...]}` — field set,
    /// field ORDER (model before input), no extra fields, no whitespace.
    #[test]
    fn openai_request_wire_json_is_pinned_byte_for_byte() {
        let input = vec![
            "search_document: fn main()".to_string(),
            "search_document: mod tests".to_string(),
        ];
        let req = EmbeddingsRequest { model: "nomic-embed-text-v1.5", input: &input };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"model":"nomic-embed-text-v1.5","input":["search_document: fn main()","search_document: mod tests"]}"#
        );
        // Single input: same shape, one element.
        let one = vec!["search_query: token validation".to_string()];
        let req = EmbeddingsRequest { model: "m2", input: &one };
        assert_eq!(
            serde_json::to_string(&req).unwrap(),
            r#"{"model":"m2","input":["search_query: token validation"]}"#
        );
    }

    // ── wire contract: response shape parses (incl. shuffled index) ─────

    #[test]
    fn openai_response_wire_shape_parses_contract_example() {
        let resp: EmbeddingsResponse = serde_json::from_str(
            r#"{"data":[{"embedding":[0.0123,-0.0456],"index":0},{"embedding":[0.0789,0.0321],"index":1}]}"#,
        )
        .expect("contract example parses");
        assert_eq!(resp.data.len(), 2);
        assert_eq!(resp.data[0].index, 0);
        assert_eq!(resp.data[1].index, 1);
        // Tolerates real-server extras (object/usage) — we pin what we parse.
        let resp: EmbeddingsResponse = serde_json::from_str(
            r#"{"object":"list","data":[{"embedding":[1.0],"index":0}],"usage":{"prompt_tokens":1}}"#,
        )
        .expect("extra fields tolerated");
        assert_eq!(resp.data[0].embedding, vec![1.0]);
    }

    // ── embed round-trip against the stub (loopback) ────────────────────

    /// POST /v1/embeddings, Bearer header present iff api_key non-empty,
    /// body byte-exact (prefixed inputs, pinned field order), response
    /// vectors L2-normalized, order preserved.
    #[test]
    fn openai_embed_posts_pinned_wire_and_normalizes() {
        let (base, handle) = spawn_stub(vec![(
            200,
            r#"{"data":[{"embedding":[3.0,4.0],"index":0},{"embedding":[0.0,2.0],"index":1}]}"#
                .to_string(),
        )]);
        let backend = doc_backend(&base, "sk-test-123");
        let out = rt()
            .block_on(backend.embed(&["fn main()".into(), "mod tests".into()]))
            .expect("embed");
        let raws = handle.join().expect("stub thread");
        assert_eq!(raws.len(), 1, "exactly one request");
        let (line, headers, body) = split_request(&raws[0]);
        assert_eq!(line, "POST /v1/embeddings HTTP/1.1");
        assert!(
            headers.to_ascii_lowercase().contains("bearer sk-test-123"),
            "Bearer auth header: {headers}"
        );
        assert_eq!(
            body,
            r#"{"model":"nomic-embed-text-v1.5","input":["search_document: fn main()","search_document: mod tests"]}"#
        );
        // Order preserved + client-side L2: [3,4]→[0.6,0.8], [0,2]→[0,1].
        assert!((out[0][0] - 0.6).abs() < 1e-6 && (out[0][1] - 0.8).abs() < 1e-6);
        assert!((out[1][0] - 0.0).abs() < 1e-6 && (out[1][1] - 1.0).abs() < 1e-6);
    }

    /// The `index` field restores input order even when data arrives
    /// shuffled; a query-kind backend applies the QUERY prefix.
    #[test]
    fn openai_index_field_restores_order_from_shuffled_data() {
        let (base, handle) = spawn_stub(vec![(
            200,
            r#"{"data":[{"embedding":[0.5,0.5],"index":1},{"embedding":[1.0,0.0],"index":0}]}"#
                .to_string(),
        )]);
        let backend = OpenAiCompat::query(
            base,
            NOMIC_EMBED_TEXT_V1_5.name.to_string(),
            String::new(),
            5,
        )
        .unwrap();
        let out = rt()
            .block_on(backend.embed(&["alpha".into(), "beta".into()]))
            .expect("embed");
        let (_, headers, body) = split_request(&handle.join().unwrap()[0]);
        assert!(!headers.to_ascii_lowercase().contains("authorization"));
        assert_eq!(
            body,
            r#"{"model":"nomic-embed-text-v1.5","input":["search_query: alpha","search_query: beta"]}"#
        );
        // index 0 → [1,0]; index 1 → [0.7071, 0.7071] after L2.
        assert!((out[0][0] - 1.0).abs() < 1e-6 && out[0][1].abs() < 1e-6);
        assert!((out[1][0] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
        assert!((out[1][1] - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-6);
    }

    // ── structural error mapping ────────────────────────────────────────

    #[test]
    fn openai_http_error_statuses_map_structurally() {
        let cases: Vec<(u16, fn(&EmbedError) -> bool)> = vec![
            (401, |e| matches!(e, EmbedError::AuthRejected(_))),
            (403, |e| matches!(e, EmbedError::AuthRejected(_))),
            (429, |e| matches!(e, EmbedError::RateLimited(_))),
            (500, |e| matches!(e, EmbedError::Unreachable(_))),
            (503, |e| matches!(e, EmbedError::Unreachable(_))),
        ];
        for (status, is_class) in cases {
            let (base, handle) = spawn_stub(vec![(status, "{}".to_string())]);
            let backend = doc_backend(&base, "");
            let err = rt()
                .block_on(backend.embed(&["x".into()]))
                .expect_err("must fail");
            assert!(is_class(&err), "HTTP {status} → wrong class: {err:?}");
            handle.join().unwrap();
        }
    }

    #[test]
    fn openai_malformed_responses_map_structurally() {
        let cases: Vec<(&str, &str)> = vec![
            // Not JSON at all.
            ("not json", "garbage"),
            // JSON but wrong shape entirely.
            (r#"{"nope":1}"#, "wrong shape"),
            // Right shape, wrong count (2 inputs, 1 vector).
            (r#"{"data":[{"embedding":[1.0],"index":0}]}"#, "wrong count"),
            // Index out of range.
            (
                r#"{"data":[{"embedding":[1.0],"index":5}]}"#,
                "index out of range",
            ),
            // Duplicate index.
            (
                r#"{"data":[{"embedding":[1.0],"index":0},{"embedding":[1.0],"index":0}]}"#,
                "duplicate index",
            ),
        ];
        for (body_src, why) in cases {
            let (base, handle) = spawn_stub(vec![(200, body_src.to_string())]);
            let backend = doc_backend(&base, "");
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

    /// Empty batch: empty output, and NO request (nothing is listening on
    /// this port — a network attempt would fail the call).
    #[test]
    fn openai_empty_batch_returns_empty_without_network() {
        let backend = doc_backend("http://127.0.0.1:1", "");
        let out = rt().block_on(backend.embed(&[])).expect("empty ok");
        assert!(out.is_empty());
    }

    /// Unreachable endpoint (nothing listening) → Unreachable, fast.
    #[test]
    fn openai_connection_refused_maps_to_unreachable() {
        let backend = doc_backend("http://127.0.0.1:1", "");
        let err = rt()
            .block_on(backend.embed(&["x".into()]))
            .expect_err("refused");
        assert!(matches!(err, EmbedError::Unreachable(_)), "{err:?}");
    }

    // ── descriptor / health ─────────────────────────────────────────────

    #[test]
    fn openai_describe_embedder_is_static_and_profile_pinned() {
        let backend = OpenAiCompat::query(
            "http://127.0.0.1:1".into(),
            NOMIC_EMBED_TEXT_V1_5.name.to_string(),
            String::new(),
            1,
        )
        .unwrap();
        let info = backend.describe_embedder(); // no network I/O
        assert_eq!(info.backend_kind, BackendKind::OpenAiCompat);
        assert_eq!(info.profile_name, "nomic-embed-text-v1.5");
        assert_eq!(info.dim, 768);
        assert_eq!(info.pooling, Pooling::Mean);
        assert_eq!(info.prefixes.query, "search_query: ");
        assert_eq!(info.prefixes.document, "search_document: ");
        assert_eq!(info.base_url, "http://127.0.0.1:1");
        assert_eq!(info.model, "nomic-embed-text-v1.5");
        assert_eq!(backend.input_kind(), InputKind::Query);
    }

    #[test]
    fn openai_health_check_is_one_trivial_embed() {
        let (base, handle) =
            spawn_stub(vec![(200, r#"{"data":[{"embedding":[1.0],"index":0}]}"#.to_string())]);
        let backend = doc_backend(&base, "");
        rt().block_on(backend.health_check()).expect("health");
        let raws = handle.join().unwrap();
        let (_, _, body) = split_request(&raws[0]);
        assert_eq!(
            body,
            r#"{"model":"nomic-embed-text-v1.5","input":["search_document: health"]}"#
        );
    }
}
