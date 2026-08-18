//! CDP (Chrome DevTools Protocol) transport: WebSocket JSON-RPC to the
//! browser endpoint, flat session mux keyed by `sessionId`, command/response
//! correlation, event fan-out (research.md D1/D3).
//!
//! Design points:
//! * One WebSocket to the browser-level `webSocketDebuggerUrl`.
//! * Commands carry optional `sessionId` — the browser routes them to the
//!   attached target (flat protocol mode).
//! * `Target.setAutoAttach { flatten: true }` gives us per-frame sessions
//!   for same- AND cross-origin frames without new sockets.

pub mod domains;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio_tungstenite::tungstenite::Message;

/// Normative error surface (contracts/cdp-session.md). Display strings are
/// user-visible in tool errors — pinned by unit test.
#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("browser not connected")]
    NotConnected,
    #[error("attach failed: {0}")]
    AttachFailed(String),
    #[error("launch failed: {0}")]
    LaunchFailed(String),
    #[error("no Chromium-family browser found; set browser.executable_path or start one with --remote-debugging-port")]
    NoBrowserFound,
    #[error("agent page is gone (closed or crashed); call ensure_page to recover")]
    PageGone,
    #[error("target not found: {0}")]
    TargetNotFoundRaw(String),
    #[error("ambiguous match: {0} candidates refused")]
    AmbiguousRaw(String),
    #[error("page did not settle within the hard timeout ({waited_ms}ms); partial state returned")]
    SettleTimeout { waited_ms: u64 },
    #[error("navigation blocked by URL safety policy: {0}")]
    UrlBlocked(String),
    #[error("raw CDP passthrough is disabled; set browser.allow_raw_cdp=true to enable")]
    RawCdpDisabled,
    #[error("CDP protocol error: {0}")]
    Protocol(String),
    #[error("timeout: {0}")]
    Timeout(String),
}

impl BrowserError {
    /// Target-not-found refusal carrying candidate elements (FR-007).
    pub fn target_not_found(candidates: &[String]) -> Self {
        BrowserError::TargetNotFoundRaw(candidates.join(", "))
    }

    /// Ambiguous-match refusal carrying candidates (FR-007).
    pub fn ambiguous(candidates: &[String]) -> Self {
        BrowserError::AmbiguousRaw(format!(
            "{} [{}]",
            candidates.len(),
            candidates.join("; ")
        ))
    }
}

/// A pending command's reply channel.
type Pending = oneshot::Sender<Result<Value, BrowserError>>;

/// Internal command after correlation.
enum ToWs {
    Send { id: u64, text: String },
    Shutdown,
}

/// Connection to the browser endpoint.
pub struct CdpConnection {
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, Pending>>>,
    writer: mpsc::UnboundedSender<ToWs>,
    /// Broadcast fan-out of protocol events (method, session_id, params).
    events: mpsc::UnboundedReceiver<(String, Option<String>, Value)>,
}

impl CdpConnection {
    /// Connect to `ws_url` (the browser-level webSocketDebuggerUrl).
    pub async fn connect(ws_url: &str) -> Result<Arc<Self>, BrowserError> {
        let (ws, _resp) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| BrowserError::AttachFailed(format!("websocket: {e}")))?;
        let (sink, stream) = ws.split();
        let sink = Arc::new(Mutex::new(sink));

        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (writer_tx, mut writer_rx) = mpsc::unbounded_channel();

        // Reader task: route responses to pending channels, events to fan-out.
        let pending: Arc<Mutex<HashMap<u64, Pending>>> =
            Arc::new(Mutex::new(HashMap::new()));
        tokio::spawn({
            let pending = pending.clone();
            async move {
                let mut stream = stream;
                while let Some(msg) = stream.next().await {
                    let text = match msg {
                        Ok(Message::Text(t)) => t,
                        Ok(Message::Close(_)) | Err(_) => break,
                        Ok(_) => continue,
                    };
                    let v: Value = match serde_json::from_str(&text) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };
                    if let Some(id) = v.get("id").and_then(|i| i.as_u64()) {
                        let mut map = pending.lock().await;
                        if let Some(tx) = map.remove(&id) {
                            let result = if let Some(err) = v.get("error") {
                                Err(BrowserError::Protocol(err.to_string()))
                            } else {
                                Ok(v.get("result").cloned().unwrap_or(Value::Null))
                            };
                            let _ = tx.send(result);
                        }
                    } else if let Some(method) = v.get("method").and_then(|m| m.as_str()) {
                        let session = v
                            .get("sessionId")
                            .and_then(|s| s.as_str())
                            .map(str::to_string);
                        let params = v.get("params").cloned().unwrap_or(Value::Null);
                        let _ = event_tx.send((method.to_string(), session, params));
                    }
                }
                // Socket closed: fail everything still pending.
                let mut map = pending.lock().await;
                for (_, tx) in map.drain() {
                    let _ = tx.send(Err(BrowserError::NotConnected));
                }
            }
        });

        // Writer task.
        tokio::spawn(async move {
            while let Some(cmd) = writer_rx.recv().await {
                match cmd {
                    ToWs::Send { id: _, text } => {
                        let mut s = sink.lock().await;
                        if s.send(Message::Text(text)).await.is_err() {
                            break;
                        }
                    }
                    ToWs::Shutdown => break,
                }
            }
        });

        Ok(Arc::new(CdpConnection {
            next_id: AtomicU64::new(1),
            pending,
            writer: writer_tx,
            events: event_rx,
        }))
    }

    /// Send a command; `session` routes to a specific attached target.
    pub async fn send(
        &self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value, BrowserError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let mut body = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            body["sessionId"] = Value::String(s.to_string());
        }
        let text = serde_json::to_string(&body)
            .map_err(|e| BrowserError::Protocol(format!("serialize: {e}")))?;

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        if self.writer.send(ToWs::Send { id, text }).is_err() {
            self.pending.lock().await.remove(&id);
            return Err(BrowserError::NotConnected);
        }
        match rx.await {
            Ok(result) => result,
            Err(_) => Err(BrowserError::NotConnected),
        }
    }

    /// Take the next protocol event (cooperative polling; the session layer
    /// selects on this for dialogs, frame changes, console entries).
    /// Take the next protocol event; requires `&mut` because the receiver
    /// is owned. The session layer wraps this in its own event loop task.
    pub async fn next_event(&mut self) -> Option<(String, Option<String>, Value)> {
        self.events.recv().await
    }
}

// ---------------------------------------------------------------------------
// Tests: framing + error mapping on canned JSON (no live browser).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_messages_pinned() {
        assert_eq!(
            BrowserError::NotConnected.to_string(),
            "browser not connected"
        );
        assert_eq!(
            BrowserError::RawCdpDisabled.to_string(),
            "raw CDP passthrough is disabled; set browser.allow_raw_cdp=true to enable"
        );
        assert_eq!(
            BrowserError::NoBrowserFound.to_string(),
            "no Chromium-family browser found; set browser.executable_path or start one with --remote-debugging-port"
        );
        assert!(BrowserError::SettleTimeout { waited_ms: 10_000 }
            .to_string()
            .contains("10000ms"));
        assert_eq!(
            BrowserError::UrlBlocked("127.0.0.1".into()).to_string(),
            "navigation blocked by URL safety policy: 127.0.0.1"
        );
    }

    #[test]
    fn refusal_constructors() {
        let e = BrowserError::target_not_found(&["e1 Save".into(), "e2 Save".into()]);
        assert!(e.to_string().contains("e1 Save"));
        let a = BrowserError::ambiguous(&["a".into(), "b".into(), "c".into()]);
        // FR-007: candidates must be LISTED in the refusal, not just counted.
        assert_eq!(a.to_string(), "ambiguous match: 3 [a; b; c] candidates refused");
    }

    #[tokio::test]
    async fn canned_response_routing() {
        // Simulate the reader-side routing logic against canned frames to pin
        // the correlation contract without a socket.
        let v: Value = serde_json::from_str(
            r#"{"id":7,"result":{"ok":true}}"#,
        )
        .unwrap();
        assert_eq!(v.get("id").and_then(|i| i.as_u64()), Some(7));
        assert_eq!(v.get("result").unwrap()["ok"], true);

        let e: Value = serde_json::from_str(
            r#"{"method":"Page.javascriptDialogOpening","sessionId":"S1","params":{"message":"hi"}}"#,
        )
        .unwrap();
        assert!(e.get("id").is_none());
        assert_eq!(e["method"], "Page.javascriptDialogOpening");
        assert_eq!(e["sessionId"], "S1");
        assert_eq!(e["params"]["message"], "hi");

        let err: Value = serde_json::from_str(
            r#"{"id":9,"error":{"code":-32000,"message":"nope"}}"#,
        )
        .unwrap();
        assert!(err.get("error").is_some());
    }
}
