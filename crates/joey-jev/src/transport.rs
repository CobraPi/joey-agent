//! Decision transports: the trait the decider speaks, the reqwest client
//! for the live loopback service, and a canned fixture for browserless,
//! service-less tests (Constitution IV).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use parking_lot::Mutex;

use async_trait::async_trait;

use crate::wire::{SystemOneRequest, SystemOneResponse};

/// Transport-level failure modes the decider collapses into fallbacks.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportError {
    /// Request exceeded the total budget — never retried.
    Timeout,
    /// 422: the service rejected the payload as oversize/invalid (detail is
    /// the capped response body; one shrink retry happens above this layer).
    Rejected422 { detail: String },
    /// 401/403: treat as unavailable (misconfig surfaces in the log only).
    Unauthorized,
    /// Any other wire/protocol failure.
    Wire(String),
}

/// What `JevDecider` speaks (contracts/decision-points.md).
#[async_trait]
pub trait DecisionTransport: Send + Sync {
    /// `GET /v1/models` availability probe.
    async fn probe(&self) -> bool;
    /// `POST /v1/systemone` decision round-trip.
    async fn ask(&self, req: &SystemOneRequest) -> Result<SystemOneResponse, TransportError>;
}

// ---------------------------------------------------------------------------
// reqwest (live service)
// ---------------------------------------------------------------------------

/// Short-lived total-timeout client for the loopback Jev service
/// (research.md R6; template: joey-providers zai.rs probe client — NOT the
/// SSE streaming client, which is deliberately timeout-free).
pub struct ReqwestTransport {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
}

impl ReqwestTransport {
    pub fn new(base_url: &str, api_key: &str, timeout: Duration) -> Self {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .unwrap_or_default();
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
        }
    }

    fn bearer(&self) -> reqwest::header::HeaderValue {
        let mut v = reqwest::header::HeaderValue::from_str(&format!(
            "Bearer {}",
            self.api_key
        ))
        .unwrap_or_else(|_| reqwest::header::HeaderValue::from_static("Bearer local"));
        v.set_sensitive(true);
        v
    }
}

#[async_trait]
impl DecisionTransport for ReqwestTransport {
    async fn probe(&self) -> bool {
        let url = format!("{}/v1/models", self.base_url);
        match self.client.get(&url).header("Authorization", self.bearer()).send().await {
            Ok(r) => r.status().as_u16() == 200,
            Err(_) => false,
        }
    }

    async fn ask(&self, req: &SystemOneRequest) -> Result<SystemOneResponse, TransportError> {
        let url = format!("{}/v1/systemone", self.base_url);
        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.bearer())
            .json(req)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    TransportError::Timeout
                } else {
                    TransportError::Wire(format!("send: {e}"))
                }
            })?;
        let status = resp.status().as_u16();
        let body = resp.text().await.unwrap_or_default();
        match status {
            200 => serde_json::from_str(&body)
                .map_err(|e| TransportError::Wire(format!("decode: {e}"))),
            422 => Err(TransportError::Rejected422 {
                // ponytail: cap detail at 512 chars — diagnostics only, never parsed.
                detail: body.chars().take(512).collect(),
            }),
            401 | 403 => Err(TransportError::Unauthorized),
            _ => Err(TransportError::Wire(format!("status {status}"))),
        }
    }
}

// ---------------------------------------------------------------------------
// Fixture (tests)
// ---------------------------------------------------------------------------

/// Canned transport: scripted probe verdicts and ask responses, injectable
/// failures, and call counters for zero-footprint assertions (SC-005/T018).
pub struct FixtureTransport {
    probe_ok: AtomicBool,
    responses: Mutex<std::collections::VecDeque<Result<SystemOneResponse, TransportError>>>,
    ask_calls: AtomicUsize,
    probe_calls: AtomicUsize,
    requests: Mutex<Vec<SystemOneRequest>>,
}

impl FixtureTransport {
    /// Fixture that answers probes and pops scripted responses.
    pub fn new(probe_ok: bool) -> Self {
        Self {
            probe_ok: AtomicBool::new(probe_ok),
            responses: Mutex::new(Default::default()),
            ask_calls: AtomicUsize::new(0),
            probe_calls: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
        }
    }

    /// Queue the next ask result (FIFO).
    pub fn push(&self, r: Result<SystemOneResponse, TransportError>) {
        self.responses.lock().push_back(r);
    }

    pub fn set_probe_ok(&self, ok: bool) {
        self.probe_ok.store(ok, Ordering::SeqCst);
    }

    pub fn ask_calls(&self) -> usize {
        self.ask_calls.load(Ordering::SeqCst)
    }

    pub fn probe_calls(&self) -> usize {
        self.probe_calls.load(Ordering::SeqCst)
    }

    /// Most recent request sent (for shrink/shape assertions).
    pub fn last_request(&self) -> Option<SystemOneRequest> {
        self.requests.lock().last().cloned()
    }
}

#[async_trait]
impl DecisionTransport for FixtureTransport {
    async fn probe(&self) -> bool {
        self.probe_calls.fetch_add(1, Ordering::SeqCst);
        self.probe_ok.load(Ordering::SeqCst)
    }

    async fn ask(&self, req: &SystemOneRequest) -> Result<SystemOneResponse, TransportError> {
        self.ask_calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().push(req.clone());
        self.requests.lock().truncate(16); // ponytail: keep the last 16 requests — assertions only need recent shape
        self.responses
            .lock()
            .pop_front()
            .unwrap_or(Err(TransportError::Wire("fixture exhausted".into())))
    }
}
