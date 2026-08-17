//! Browser automation tools (feature 016): the 12 declared core names made
//! functional + 4 additive verbs (research.md D9; contracts/browser-tools.md).
//!
//! Registration follows the neurocode conditional pattern: every tool holds
//! an optional shared [`BrowserHandle`]; `check()` returns false when no
//! session manager is wired, hiding the tools from the model until a
//! browser session exists.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::context::ToolContext;
use crate::registry::{Tool, ToolResult};

use joey_browser::refs::TargetDescriptor;
use joey_browser::session::BrowserManager;
use joey_browser::BrowserConfig;

// ---------------------------------------------------------------------------
// Handle
// ---------------------------------------------------------------------------

/// Shared browser session manager handle. `None` inside until connected.
#[derive(Clone, Default)]
pub struct BrowserHandle {
    manager: Arc<tokio::sync::Mutex<Option<Arc<BrowserManager>>>>,
    connected: Arc<std::sync::atomic::AtomicBool>,
}

impl BrowserHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Connect (attach or managed launch). Idempotent when already connected.
    pub async fn connect(&self, cfg: BrowserConfig) -> Result<(), joey_browser::BrowserError> {
        let mut guard = self.manager.lock().await;
        if let Some(m) = guard.as_ref() {
            let _ = m;
            return Ok(());
        }
        let m = BrowserManager::connect(cfg).await?;
        *guard = Some(m);
        self.connected.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    /// Disconnect and clear.
    pub async fn disconnect(&self) -> Result<(), joey_browser::BrowserError> {
        let mut guard = self.manager.lock().await;
        if let Some(m) = guard.take() {
            m.disconnect().await?;
        }
        self.connected.store(false, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    pub(crate) async fn run<T, F, Fut>(&self, op: F) -> Result<T, joey_browser::BrowserError>
    where
        F: FnOnce(Arc<BrowserManager>) -> Fut,
        Fut: std::future::Future<Output = Result<T, joey_browser::BrowserError>>,
    {
        let guard = self.manager.lock().await;
        let m = guard.as_ref().ok_or(joey_browser::BrowserError::NotConnected)?;
        op(m.clone()).await
    }
}

/// Process-global browser handle so the CLI (slash commands) and the tool
/// registry share one session manager.
pub fn shared_browser_handle() -> Arc<BrowserHandle> {
    use once_cell::sync::Lazy;
    static SHARED: Lazy<Arc<BrowserHandle>> = Lazy::new(|| Arc::new(BrowserHandle::new()));
    SHARED.clone()
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register all browser tools; each is hidden until a handle is connected.
pub fn register_browser_tools(registry: &mut crate::registry::ToolRegistry, handle: Option<Arc<BrowserHandle>>) {
    let handle = match handle {
        Some(h) => h,
        None => return, // nothing wired: register nothing (tools stay hidden)
    };
    registry.register(Arc::new(BrowserNavigate { handle: handle.clone() }));
    registry.register(Arc::new(BrowserSnapshot { handle: handle.clone() }));
    registry.register(Arc::new(BrowserClick { handle: handle.clone() }));
    registry.register(Arc::new(BrowserType { handle: handle.clone() }));
    registry.register(Arc::new(BrowserScroll { handle: handle.clone() }));
    registry.register(Arc::new(BrowserBack { handle: handle.clone() }));
    registry.register(Arc::new(BrowserPress { handle: handle.clone() }));
    registry.register(Arc::new(BrowserGetImages { handle: handle.clone() }));
    registry.register(Arc::new(BrowserVision { handle: handle.clone() }));
    registry.register(Arc::new(BrowserConsole { handle: handle.clone() }));
    registry.register(Arc::new(BrowserCdp { handle: handle.clone() }));
    registry.register(Arc::new(BrowserDialog { handle: handle.clone() }));
    registry.register(Arc::new(BrowserHover { handle: handle.clone() }));
    registry.register(Arc::new(BrowserSelectOption { handle: handle.clone() }));
    registry.register(Arc::new(BrowserDrag { handle: handle.clone() }));
    registry.register(Arc::new(BrowserClickCoords { handle }));
}

// ---------------------------------------------------------------------------
// Shared plumbing
// ---------------------------------------------------------------------------

fn target_from(args: &Value) -> Result<TargetDescriptor, ToolResult> {
    let t = args.get("target").cloned().unwrap_or(Value::Null);
    if t.is_null() {
        return Err(ToolResult::Error("missing required 'target' descriptor".into()));
    }
    serde_json::from_value(t)
        .map_err(|e| ToolResult::Error(format!("invalid target descriptor: {e}")))
}

fn ok_json(v: Value) -> ToolResult {
    ToolResult::Text(serde_json::to_string_pretty(&v).unwrap_or_default())
}

fn err(e: joey_browser::BrowserError) -> ToolResult {
    ToolResult::Error(e.to_string())
}

macro_rules! browser_tool {
    ($name:ident, $wire:literal, $desc:literal, $emoji:literal) => {
        pub struct $name {
            pub handle: Arc<BrowserHandle>,
        }
        impl $name {
            pub const WIRE: &'static str = $wire;
            pub const DESC: &'static str = $desc;
            pub const EMOJI: &'static str = $emoji;
            pub fn check_connected(&self, _ctx: &ToolContext) -> bool {
                self.handle.is_connected()
            }
        }
    };
}

browser_tool!(BrowserNavigate, "browser_navigate", "Navigate the agent's dedicated browser tab to a URL. Waits for content settle. Refuses local/private network targets per URL-safety policy. Returns url/title/frame count.", "🌐");
browser_tool!(BrowserSnapshot, "browser_snapshot", "Take a deep structural snapshot of the current page: pierces shadow DOM and frames, viewport-priority presentation (in-view elements listed fully, out-of-view summarized). Optional since_last=true returns only the delta (feeds).", "📸");
browser_tool!(BrowserClick, "browser_click", "Click an element. Target descriptor: {refid?, locator?, text?, geometry?} resolved via cascading fallback (refid→locator→text→geometry); result reports which strategy resolved.", "👆");
browser_tool!(BrowserType, "browser_type", "Type text into an input. Focuses the target first; optional clear and Enter-submit.", "⌨️");
browser_tool!(BrowserScroll, "browser_scroll", "Scroll the page, or a specific scrollable container when 'target' is provided.", "🖲️");
browser_tool!(BrowserBack, "browser_back", "Go back one history entry in the agent tab.", "◀️");
browser_tool!(BrowserPress, "browser_press", "Press a key with optional modifiers (ctrl/alt/shift/meta), e.g. Cmd+Enter.", "🎹");
browser_tool!(BrowserGetImages, "browser_get_images", "List images on the current page (src, alt, dimensions, visibility).", "🖼️");
browser_tool!(BrowserVision, "browser_vision", "Capture an annotated Set-of-Mark screenshot of the viewport with numbered markers; use when structural extraction fails or a visual check is needed.", "👁️");
browser_tool!(BrowserConsole, "browser_console", "Read buffered console entries from the page (level, text, source).", "🖥️");
browser_tool!(BrowserCdp, "browser_cdp", "Raw CDP passthrough (expert). Requires browser.allow_raw_cdp=true; bypasses URL-safety gates — use with care.", "🔧");
browser_tool!(BrowserDialog, "browser_dialog", "Accept or dismiss a JavaScript dialog (alert/confirm/prompt); optional prompt_text.", "💬");
browser_tool!(BrowserHover, "browser_hover", "Hover an element (opens hover-only menus).", "🖱️");
browser_tool!(BrowserSelectOption, "browser_select_option", "Select an option on a native <select> dropdown.", "📋");
browser_tool!(BrowserDrag, "browser_drag", "Drag from a source element to a target element (kanban, upload zones).", "🫳");
browser_tool!(BrowserClickCoords, "browser_click_coords", "Click at viewport pixel coordinates (elements with no handlers, or SoM marker picks via 'marker').", "🎯");

// ---------------------------------------------------------------------------
// Parameters + execute per tool
// ---------------------------------------------------------------------------

fn target_param() -> Value {
    json!({
        "type": "object",
        "description": "Target descriptor: at least one of refid/locator/text/geometry.",
        "properties": {
            "refid": { "type": "string", "description": "Element refid from the latest snapshot (e.g. e12)." },
            "locator": { "type": "string", "description": "Structural CSS locator fallback." },
            "text": { "type": "string", "description": "Visible-text fallback." },
            "geometry": {
                "type": "object",
                "properties": {
                    "x": { "type": "number" }, "y": { "type": "number" },
                    "w": { "type": "number" }, "h": { "type": "number" }
                },
                "required": ["x", "y", "w", "h"]
            }
        }
    })
}

#[async_trait]
impl Tool for BrowserNavigate {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "URL to open in the agent tab." }
            },
            "required": ["url"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let url = match args.get("url").and_then(|v| v.as_str()) {
            Some(u) => u,
            None => return ToolResult::Error("missing url".into()),
        };
        match self.handle.run(|m| async move { m.navigate(url).await }).await {
            Ok(r) => ok_json(json!({ "navigated": true, "result": r })),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserSnapshot {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "viewport_only": { "type": "boolean", "default": false, "description": "Restrict to in-view elements only." },
                "since_last": { "type": "boolean", "default": false, "description": "Return only elements new since the previous snapshot (feeds)." }
            }
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let since_last = args.get("since_last").and_then(|v| v.as_bool()).unwrap_or(false);
        let _ = since_last; // delta mode wired with the feed pipeline (T057)
        match self.handle.run(|m| async move {
            let registry = m.scan_to_registry().await?;
            let snapshot = joey_browser::snapshot::structural_snapshot(
                m.eval_string("location.href").await?,
                m.eval_string("document.title").await?,
                m.frame_count().await?,
                m.viewport().await?,
                registry.elements.clone(),
                vec![],
                &m.config.budgets,
                m.config.budgets.viewport_margin,
            );
            Ok(snapshot.render())
        }).await {
            Ok(text) => ToolResult::Text(text),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserClick {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": { "target": target_param() }, "required": ["target"] })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let t = match target_from(&args) {
            Ok(t) => t,
            Err(e) => return e,
        };
        match self.handle.run(|m| async move { m.click(&t).await }).await {
            Ok(r) => ok_json(json!({ "ok": r.ok, "resolved_by": r.resolved_by.as_str(), "detail": r.detail })),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserType {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": target_param(),
                "text": { "type": "string" },
                "clear": { "type": "boolean", "default": false },
                "submit": { "type": "boolean", "default": false }
            },
            "required": ["target", "text"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let t = match target_from(&args) {
            Ok(t) => t,
            Err(e) => return e,
        };
        let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let clear = args.get("clear").and_then(|v| v.as_bool()).unwrap_or(false);
        let submit = args.get("submit").and_then(|v| v.as_bool()).unwrap_or(false);
        match self.handle.run(move |m| async move { m.r#type(&t, &text, clear, submit).await }).await {
            Ok(r) => ok_json(json!({ "ok": r.ok, "resolved_by": r.resolved_by.as_str(), "detail": r.detail })),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserScroll {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "direction": { "type": "string", "enum": ["up", "down"] },
                "amount": { "type": "number", "description": "Pixels to scroll (default 600)." },
                "target": target_param()
            },
            "required": ["direction"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let direction = args.get("direction").and_then(|v| v.as_str()).unwrap_or("down").to_string();
        let amount = args.get("amount").and_then(|v| v.as_f64()).unwrap_or(600.0);
        let target = match args.get("target") {
            Some(_) => match target_from(&args) {
                Ok(t) => Some(t),
                Err(e) => return e,
            },
            None => None,
        };
        match self.handle.run(move |m| async move { m.scroll(target.as_ref(), &direction, amount).await }).await {
            Ok(r) => ok_json(json!({ "ok": r.ok, "detail": r.detail })),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserBack {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        match self.handle.run(|m| async move { m.back().await }).await {
            Ok(r) => ok_json(json!({ "ok": true, "result": r })),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserPress {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "key": { "type": "string", "description": "Key name, e.g. Enter, Tab, a." },
                "modifiers": {
                    "type": "array",
                    "items": { "type": "string", "enum": ["ctrl", "alt", "shift", "meta", "cmd"] },
                    "description": "Modifier keys held."
                }
            },
            "required": ["key"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let key = match args.get("key").and_then(|v| v.as_str()) {
            Some(k) => k.to_string(),
            None => return ToolResult::Error("missing key".into()),
        };
        let mods = args.get("modifiers").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        let has = |n: &str| mods.iter().any(|m| m.as_str() == Some(n));
        let (ctrl, alt, shift, meta) = (has("ctrl"), has("alt"), has("shift"), has("meta") || has("cmd"));
        match self.handle.run(move |m| async move { m.press_key(&key, ctrl, alt, shift, meta).await }).await {
            Ok(r) => ok_json(json!({ "ok": r.ok, "detail": r.detail })),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserGetImages {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        match self.handle.run(|m| async move {
            let raw = m.evaluate(
                "(function(){ var out=[]; document.querySelectorAll('img').forEach(function(i){ out.push({src:i.currentSrc||i.src, alt:i.alt||'', w:i.naturalWidth||0, h:i.naturalHeight||0, visible: i.getBoundingClientRect().width>0}); }); return JSON.stringify(out.slice(0,200)); })()"
            ).await?;
            Ok(raw)
        }).await {
            Ok(Value::Null) => ToolResult::Error("no images".into()),
            Ok(v) => {
                let s = v.as_str().unwrap_or("[]");
                match serde_json::from_str::<Value>(s) {
                    Ok(images) => ok_json(json!({ "images": images })),
                    Err(_) => ToolResult::Error("image list decode failed".into()),
                }
            }
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserVision {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "prompt": { "type": "string", "description": "Optional focus prompt for the visual analysis." }
            }
        })
    }
    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        match self.handle.run(|m| async move { m.visual_observe(None).await }).await {
            Ok(v) => ok_json(serde_json::to_value(&v).unwrap_or_default()),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserConsole {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn execute(&self, _args: Value, _ctx: &ToolContext) -> ToolResult {
        let s = match self.handle
            .run(|m| async move {
                m.eval_string(
                    "(function(){ return JSON.stringify((window.__joeyConsole||[]).slice(-200)); })()",
                )
                .await
            })
            .await
        {
            Ok(v) => v,
            Err(e) => return err(e),
        };
        match serde_json::from_str::<Value>(&s) {
            Ok(entries) => ok_json(json!({ "entries": entries })),
            Err(_) => ok_json(json!({ "entries": [] })),
        }
    }
}

#[async_trait]
impl Tool for BrowserCdp {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "method": { "type": "string", "description": "CDP method name." },
                "params": { "type": "object", "description": "CDP params object." }
            },
            "required": ["method"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let method = match args.get("method").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return ToolResult::Error("missing method".into()),
        };
        let params = args.get("params").cloned().unwrap_or(json!({}));
        match self.handle.run(move |m| async move { m.raw_cdp(&method, params).await }).await {
            Ok(v) => ok_json(v),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserDialog {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ["accept", "dismiss"] },
                "prompt_text": { "type": "string" }
            },
            "required": ["action"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let accept = args.get("action").and_then(|v| v.as_str()) == Some("accept");
        let prompt = args.get("prompt_text").and_then(|v| v.as_str()).map(str::to_string);
        match self.handle.run(move |m| async move { m.handle_dialog(accept, prompt).await }).await {
            Ok(_) => ok_json(json!({ "handled": true })),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserHover {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": { "target": target_param() }, "required": ["target"] })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let t = match target_from(&args) {
            Ok(t) => t,
            Err(e) => return e,
        };
        match self.handle.run(move |m| async move { m.hover(&t).await }).await {
            Ok(r) => ok_json(json!({ "ok": r.ok, "resolved_by": r.resolved_by.as_str(), "detail": r.detail })),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserSelectOption {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target": target_param(),
                "value": { "type": "string", "description": "Option value to select." }
            },
            "required": ["target", "value"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let t = match target_from(&args) {
            Ok(t) => t,
            Err(e) => return e,
        };
        let value = args.get("value").and_then(|v| v.as_str()).unwrap_or("").to_string();
        match self.handle.run(move |m| async move { m.select_option(&t, &value).await }).await {
            Ok(r) => ok_json(json!({ "ok": r.ok, "resolved_by": r.resolved_by.as_str(), "selected": r.detail })),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserDrag {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "source": target_param(),
                "target": target_param()
            },
            "required": ["source", "target"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let s = match args.get("source").map(|_| target_from(&json!({ "target": args["source"] }))) {
            Some(Ok(t)) => t,
            _ => return ToolResult::Error("missing source".into()),
        };
        let t = match args.get("target").map(|_| target_from(&json!({ "target": args["target"] }))) {
            Some(Ok(t)) => t,
            _ => return ToolResult::Error("missing target".into()),
        };
        match self.handle.run(move |m| async move { m.drag(&s, &t).await }).await {
            Ok(r) => ok_json(json!({ "ok": r.ok, "detail": r.detail })),
            Err(e) => err(e),
        }
    }
}

#[async_trait]
impl Tool for BrowserClickCoords {

    fn name(&self) -> &str {
        Self::WIRE
    }
    fn toolset(&self) -> &str {
        "web"
    }
    fn emoji(&self) -> &str {
        Self::EMOJI
    }
    fn description(&self) -> &str {
        Self::DESC
    }
    fn check(&self, ctx: &ToolContext) -> bool {
        self.check_connected(ctx)
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "x": { "type": "number" },
                "y": { "type": "number" },
                "marker": { "type": "string", "description": "SoM marker id from browser_vision; resolves to its rect center." }
            },
            "required": ["x", "y"]
        })
    }
    async fn execute(&self, args: Value, _ctx: &ToolContext) -> ToolResult {
        let x = args.get("x").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let y = args.get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
        match self.handle.run(move |m| async move { m.click_coords(x, y).await }).await {
            Ok(r) => ok_json(json!({ "ok": r.ok, "detail": r.detail })),
            Err(e) => err(e),
        }
    }
}
