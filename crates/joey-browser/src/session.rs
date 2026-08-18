//! BrowserManager: attach-or-launch connection lifecycle, the dedicated
//! agent tab, and page-level state (research.md D8; FR-017; data-model §1-2).
//!
//! Tab discipline (clarification Q1): the agent always creates its own tab
//! via `Target.createTarget` and never touches user tabs. Acquisition
//! (clarification Q2): attach when available, else managed launch
//! (headless when no display).

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::cdp::domains::{AttachToTargetResult, CreateTargetResult, GetFrameTreeResult};
use crate::cdp::{BrowserError, CdpConnection};
use crate::config::BrowserConfig;
use crate::launch;

/// Connection mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Attached,
    Managed,
    Disconnected,
}

/// Status snapshot for `/browser status`.
#[derive(Debug, Clone)]
pub struct BrowserStatus {
    pub mode: Mode,
    pub endpoint: String,
    pub page_url: Option<String>,
    pub page_title: Option<String>,
}

/// The agent's dedicated tab identity.
#[derive(Debug, Clone)]
pub struct PageRef {
    pub target_id: String,
    pub session_id: String,
}

/// Frame info tracked per scan.
#[derive(Debug, Clone)]
pub struct FrameInfo {
    pub id: String,
    pub origin: Option<String>,
    pub label: String,
    /// None for main frame; Some for OOPIF targets.
    pub oopif_session: Option<String>,
}

/// Owns the connection + dedicated tab.
pub struct BrowserManager {
    mode: Mutex<Mode>,
    conn: Option<Arc<CdpConnection>>,
    page: Mutex<Option<PageRef>>,
    pub config: BrowserConfig,
    managed_child: Mutex<Option<tokio::process::Child>>,
}

impl BrowserManager {
    /// Attach to a running browser at `cfg.cdp_url`, else discover + launch
    /// a managed instance. Headless per policy when managed.
    pub async fn connect(cfg: BrowserConfig) -> Result<Arc<Self>, BrowserError> {
        // 1. Try attach: probe /json/version with 2s timeout.
        if let Some(ws_url) = probe_ws_url(&cfg.cdp_url).await {
            let conn = CdpConnection::connect(&ws_url)
                .await
                .map_err(|e| BrowserError::AttachFailed(format!("{e}")))?;
            let mgr = Self {
                mode: Mutex::new(Mode::Attached),
                conn: Some(conn),
                page: Mutex::new(None),
                config: cfg,
                managed_child: Mutex::new(None),
            };
            return Ok(Arc::new(mgr));
        }
        // 2. Managed launch.
        let discovered =
            launch::discover(cfg.executable_path.as_deref()).ok_or(BrowserError::NoBrowserFound)?;
        let headless = launch::resolve_headless(cfg.headless);
        let managed = launch::launch_managed(&discovered.path, headless).await?;
        let conn = CdpConnection::connect(&managed.ws_url)
            .await
            .map_err(|e| BrowserError::LaunchFailed(format!("{e}")))?;
        let mgr = Self {
            mode: Mutex::new(Mode::Managed),
            conn: Some(conn),
            page: Mutex::new(None),
            config: cfg,
            managed_child: Mutex::new(Some(managed.child)),
        };
        Ok(Arc::new(mgr))
    }

    pub(crate) fn conn(&self) -> Result<&Arc<CdpConnection>, BrowserError> {
        self.conn.as_ref().ok_or(BrowserError::NotConnected)
    }

    /// Ensure the agent's dedicated tab exists; idempotent; exactly one.
    pub async fn ensure_page(&self) -> Result<PageRef, BrowserError> {
        let mut guard = self.page.lock().await;
        if let Some(p) = guard.as_ref() {
            // Liveness check: the target still exists.
            let targets = self
                .conn()?
                .send("Target.getTargets", json!({}), None)
                .await?;
            let alive = targets["targetInfos"]
                .as_array()
                .map(|a| a.iter().any(|t| t["targetId"] == p.target_id))
                .unwrap_or(false);
            if alive {
                return Ok(p.clone());
            }
            tracing::warn!(target = %p.target_id, "agent tab gone; recreating");
        }
        let created: CreateTargetResult = serde_json::from_value(
            self.conn()?
                .send("Target.createTarget", json!({ "url": "about:blank" }), None)
                .await?,
        )
        .map_err(|e| BrowserError::Protocol(format!("createTarget decode: {e}")))?;
        let attached: AttachToTargetResult = serde_json::from_value(
            self.conn()?
                .send(
                    "Target.attachToTarget",
                    json!({ "targetId": created.targetId, "flatten": true }),
                    None,
                )
                .await?,
        )
        .map_err(|e| BrowserError::Protocol(format!("attach decode: {e}")))?;
        let page = PageRef {
            target_id: created.targetId,
            session_id: attached.sessionId,
        };
        // Enable the domains we need on the page session.
        for method in ["Page.enable", "Runtime.enable", "Log.enable"] {
            let _ = self.conn()?.send(method, json!({}), Some(&page.session_id)).await;
        }
        // Per-frame sessions for cross-origin piercing (D3).
        let _ = self
            .conn()?
            .send(
                "Target.setAutoAttach",
                crate::cdp::domains::set_auto_attach(true, true),
                Some(&page.session_id),
            )
            .await;
        *guard = Some(page.clone());
        Ok(page)
    }

    /// Navigate the agent tab (URL-safety gate first). Returns title/frames.
    pub async fn navigate(&self, url: &str) -> Result<Value, BrowserError> {
        // FR-020: same URL-safety rules as the agent's other web tools.
        if !self.config.allow_local_urls {
            if let Err(reason) = crate::url_safety_bridge::url_safety_check(url) {
                return Err(BrowserError::UrlBlocked(reason));
            }
        }
        let page = self.ensure_page().await?;
        let r = self
            .conn()?
            .send(
                "Page.navigate",
                json!({ "url": url }),
                Some(&page.session_id),
            )
            .await
            .map_err(|e| BrowserError::Protocol(format!("navigate: {e}")))?;
        if let Some(err) = r.get("errorText").and_then(|t| t.as_str()) {
            if !err.is_empty() {
                return Err(BrowserError::Protocol(format!("navigation error: {err}")));
            }
        }
        Ok(r)
    }

    /// History back in the agent tab.
    pub async fn back(&self) -> Result<Value, BrowserError> {
        let page = self.ensure_page().await?;
        self.conn()?
            .send("Page.navigateToHistoryEntry", json!({ "entryId": -2 }), Some(&page.session_id))
            .await
    }

    /// Current frame tree (merged from the page session).
    pub async fn frame_tree(&self) -> Result<GetFrameTreeResult, BrowserError> {
        let page = self.ensure_page().await?;
        let v = self
            .conn()?
            .send("Page.getFrameTree", json!({}), Some(&page.session_id))
            .await?;
        serde_json::from_value(v).map_err(|e| BrowserError::Protocol(format!("frameTree: {e}")))
    }

    /// Evaluate JS in the page session; returns the JSON value.
    pub async fn evaluate(&self, expr: &str) -> Result<Value, BrowserError> {
        let page = self.ensure_page().await?;
        let r = self
            .conn()?
            .send(
                "Runtime.evaluate",
                json!({ "expression": expr, "returnByValue": true, "awaitPromise": true }),
                Some(&page.session_id),
            )
            .await?;
        if let Some(exc) = r.get("exceptionDetails") {
            if !exc.is_null() {
                return Err(BrowserError::Protocol(format!("eval exception: {exc}")));
            }
        }
        Ok(r.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Evaluate a JS expression that returns a string (convenience).
    pub async fn eval_string(&self, expr: &str) -> Result<String, BrowserError> {
        let v = self.evaluate(&format!("String({expr})")).await?;
        Ok(v.as_str().unwrap_or_default().to_string())
    }

    /// Number of frames in the current tree (main + children).
    pub async fn frame_count(&self) -> Result<usize, BrowserError> {
        let tree = self.frame_tree().await?;
        fn count(node: &crate::cdp::domains::FrameTreeNode) -> usize {
            1 + node.child_frames.iter().map(count).sum::<usize>()
        }
        Ok(count(&tree.frameTree))
    }

    /// Current viewport metrics.
    pub async fn viewport(&self) -> Result<crate::snapshot::Viewport, BrowserError> {
        let v = self
            .evaluate(
                "({ x: 0, y: window.scrollY, w: window.innerWidth, h: window.innerHeight, scrollY: window.scrollY })",
            )
            .await?;
        let num = |k: &str| v[k].as_f64().unwrap_or(0.0);
        Ok(crate::snapshot::Viewport {
            x: num("x"),
            y: num("y"),
            w: num("w"),
            h: num("h"),
            scroll_y: num("scrollY"),
        })
    }

    /// UNGATED raw send to the page session (diagnostics; cfg-allow_raw_cdp
    /// does NOT apply — this is crate-internal).
    pub async fn raw_eval_diag(&self, method: &str, params: Value) -> Result<Value, BrowserError> {
        let page = self.ensure_page().await?;
        self.conn()?.send(method, params, Some(&page.session_id)).await
    }

    /// Raw CDP passthrough (gated).
    pub async fn raw_cdp(&self, method: &str, params: Value) -> Result<Value, BrowserError> {
        if !self.config.allow_raw_cdp {
            return Err(BrowserError::RawCdpDisabled);
        }
        self.conn()?.send(method, params, None).await
    }

    /// Accept/dismiss an open JavaScript dialog.
    pub async fn handle_dialog(
        &self,
        accept: bool,
        prompt_text: Option<String>,
    ) -> Result<(), BrowserError> {
        let page = self.ensure_page().await?;
        let mut params = json!({ "accept": accept });
        if let Some(p) = prompt_text {
            params["promptText"] = Value::String(p);
        }
        self.conn()?
            .send("Page.handleJavaScriptDialog", params, Some(&page.session_id))
            .await?;
        Ok(())
    }

    /// Disconnect: kill the child iff Managed (no orphans); leave an
    /// attached browser running.
    pub async fn disconnect(self: Arc<Self>) -> Result<(), BrowserError> {
        let mut mode = self.mode.lock().await;
        if let Some(child) = self.managed_child.lock().await.as_mut() {
            let _ = child.kill().await;
        }
        *self.page.lock().await = None;
        *mode = Mode::Disconnected;
        Ok(())
    }

    pub async fn status(&self) -> BrowserStatus {
        let mode = *self.mode.lock().await;
        let page = self.page.lock().await.clone();
        let (url, title) = match page {
            Some(p) => match self.evaluate("({ url: location.href, title: document.title })").await {
                Ok(v) => (
                    v["url"].as_str().map(str::to_string),
                    v["title"].as_str().map(str::to_string),
                ),
                Err(_) => (None, None),
            },
            None => (None, None),
        };
        BrowserStatus {
            mode,
            endpoint: self.config.cdp_url.clone(),
            page_url: url,
            page_title: title,
        }
    }

    /// Current connection mode.
    pub async fn mode(&self) -> Mode {
        *self.mode.lock().await
    }

    /// Count of page-type targets (agent-tab isolation assertions).
    pub async fn targets_count(&self) -> Result<usize, BrowserError> {
        let r = self
            .conn()?
            .send("Target.getTargets", json!({}), None)
            .await?;
        Ok(r["targetInfos"]
            .as_array()
            .map(|a| a.iter().filter(|t| t["type"] == "page").count())
            .unwrap_or(0))
    }

    /// Alias for disconnect(self: Arc<Self>) used by tests/callers that
    /// hold the manager in an Arc.
    pub async fn disconnect_arc(self: Arc<Self>) -> Result<(), BrowserError> {
        self.disconnect().await
    }

    /// Full structural snapshot of the agent page (viewport-priority).
    pub async fn snapshot(&self) -> Result<crate::snapshot::Snapshot, BrowserError> {
        let registry = self.scan_to_registry().await?;
        Ok(crate::snapshot::structural_snapshot(
            self.eval_string("location.href").await?,
            self.eval_string("document.title").await?,
            self.frame_count().await?,
            self.viewport().await?,
            registry.elements,
            Vec::new(),
            &self.config.budgets,
            self.config.budgets.viewport_margin,
        ))
    }

    /// Detect + handle blocking overlays per the configured policy
    /// (FR-011; research.md D5). Conservative default: auto-dismiss only
    /// high-confidence consent overlays with a safe dismissal control;
    /// everything else is left for the model (flagged upstream by the
    /// caller reading the overlay findings).
    pub async fn apply_overlay_policy(&self) -> Result<Vec<serde_json::Value>, BrowserError> {
        use crate::config::OverlayPolicy;
        if self.config.overlay_policy == OverlayPolicy::Never {
            return Ok(Vec::new());
        }
        let raw = self.evaluate(crate::extract::OVERLAYS_JS).await?;
        let parsed: serde_json::Value = serde_json::from_str(raw.as_str().unwrap_or("{\"overlays\":[]}"))
            .map_err(|e| BrowserError::Protocol(format!("overlay decode: {e}")))?;
        let mut acted = Vec::new();
        for ov in parsed["overlays"].as_array().cloned().unwrap_or_default() {
            let kind = ov["kind"].as_str().unwrap_or("unknown");
            let safe = ov["hasSafeDismissal"].as_bool().unwrap_or(false);
            // Conservative: only consent + safe dismissal.
            let dismissible = kind == "consent" && safe
                || self.config.overlay_policy == OverlayPolicy::Aggressive && safe;
            acted.push(serde_json::json!({
                "kind": kind,
                "description": ov["description"].as_str().unwrap_or(""),
                "action": if dismissible { "auto_dismissed" } else { "flagged" },
            }));
            if dismissible {
                // Prefer the reject-style label the heuristics identified.
                let label = ov["dismissalLabel"].as_str().unwrap_or("Reject all");
                let js = format!(
                    "(function(){{ const btns=[...document.querySelectorAll('button, a')]; \
                    const b=btns.find(x=>(x.innerText||'').trim()==={lbl:?}); \
                    if(b){{ b.click(); return 'dismissed'; }} return 'no-control'; }})()",
                    lbl = label
                );
                let _ = self.evaluate(&js).await;
            }
        }
        Ok(acted)
    }

    /// Current observation mode (FR-013/FR-014): `visual` when the last
    /// scan yielded zero actionable elements, else `structural`.
    pub async fn observation_mode(&self) -> &'static str {
        let registry = self.scan_to_registry().await.unwrap_or_default();
        let actionable = registry.elements.iter().any(|e| e.interactable);
        if actionable { "structural" } else { "visual" }
    }
}

/// Probe `http://host:port/json/version` for `webSocketDebuggerUrl`.
async fn probe_ws_url(cdp_url: &str) -> Option<String> {
    let url = format!("{}/json/version", cdp_url.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .ok()?;
    let resp: Value = client.get(url).send().await.ok()?.json().await.ok()?;
    resp.get("webSocketDebuggerUrl")
        .and_then(|u| u.as_str())
        .map(str::to_string)
}

// reqwest is not currently a joey-browser dependency — cfg-gate the probe to
// keep the manifest lean only if reqwest is absent; it IS in the workspace,
// so add it below.
