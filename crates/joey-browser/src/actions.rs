//! Action verbs: pre-action re-scan, cascade resolution, CDP Input events
//! (research.md D2; FR-005..009; contracts/browser-tools.md).

use serde_json::{json, Value};

use crate::cdp::domains::{key_event, modifier_bitmask, mouse_event, MouseEventType};
use crate::cdp::BrowserError;
use crate::refs::{ElementRefRegistry, ResolvedBy, TargetDescriptor};
use crate::session::BrowserManager;

/// Result of one action.
#[derive(Debug, Clone)]
pub struct ActionResult {
    pub ok: bool,
    pub resolved_by: ResolvedBy,
    pub detail: String,
}

impl BrowserManager {
    /// The pre-action pipeline: re-scan (fresh registry), resolve the
    /// descriptor through the cascade, then execute the verb (FR-005).
    async fn resolve_fresh(
        &self,
        target: &TargetDescriptor,
    ) -> Result<(ElementRefRegistry, crate::refs::ElementRef, ResolvedBy), BrowserError> {
        let registry = self.scan_to_registry().await?;
        let resolved = {
            let r = registry
            .resolve(target)
            .map_err(|e| match e {
                crate::refs::ResolutionFailure::Ambiguous(c) => {
                    BrowserError::ambiguous(&c)
                }
                crate::refs::ResolutionFailure::Empty => {
                    BrowserError::Protocol("empty target descriptor".into())
                }
                crate::refs::ResolutionFailure::TargetGone => {
                    BrowserError::target_not_found(&["element no longer resolvable after re-render".into()])
                }
            })?;
            (r.0.clone(), r.1)
        };
        let el = resolved.0;
        Ok((registry, el, resolved.1))
    }

    /// Scan the page and build a fresh registry (refids reset per scan).
    pub async fn scan_to_registry(&self) -> Result<ElementRefRegistry, BrowserError> {
        let raw = self.evaluate(crate::extract::SCAN_JS).await?;
        let s = raw.as_str().unwrap_or("null");
        let parsed: Value = serde_json::from_str(s)
            .map_err(|e| BrowserError::Protocol(format!("scan decode: {e}")))?;
        let mut registry = ElementRefRegistry::new();
        if let Some(els) = parsed.get("elements").and_then(|v| v.as_array()) {
            for e in els {
                let el: crate::refs::ElementRef = match serde_json::from_value(e.clone()) {
                    Ok(el) => el,
                    Err(_) => continue, // hostile row: skip
                };
                registry.push(el);
            }
        }
        Ok(registry)
    }

    /// Click via coordinate at element center (works for handlerless too).
    pub async fn click(&self, target: &TargetDescriptor) -> Result<ActionResult, BrowserError> {
        let (_, el, by) = self.resolve_fresh(target).await?;
        let (x, y) = el.geometry.center();
        self.dispatch_click(x, y).await?;
        Ok(ActionResult {
            ok: true,
            resolved_by: by,
            detail: format!("clicked \"{}\"", el.text),
        })
    }

    /// Physical click at viewport coordinates (FR-009).
    pub async fn click_coords(&self, x: f64, y: f64) -> Result<ActionResult, BrowserError> {
        self.dispatch_click(x, y).await?;
        Ok(ActionResult {
            ok: true,
            resolved_by: ResolvedBy::Geometry,
            detail: format!("clicked ({x:.0},{y:.0})"),
        })
    }

    async fn dispatch_click(&self, x: f64, y: f64) -> Result<(), BrowserError> {
        let page = self.ensure_page().await?;
        let s = &page.session_id;
        self.conn()?.send("Input.dispatchMouseEvent", mouse_event(MouseEventType::Moved, x, y, "none", 0), Some(s)).await?;
        self.conn()?.send("Input.dispatchMouseEvent", mouse_event(MouseEventType::Pressed, x, y, "left", 1), Some(s)).await?;
        self.conn()?.send("Input.dispatchMouseEvent", mouse_event(MouseEventType::Released, x, y, "left", 1), Some(s)).await?;
        Ok(())
    }

    /// Type into a target (focus first; optional clear + Enter submit).
    pub async fn r#type(
        &self,
        target: &TargetDescriptor,
        text: &str,
        clear: bool,
        submit: bool,
    ) -> Result<ActionResult, BrowserError> {
        let (_, el, by) = self.resolve_fresh(target).await?;
        let page = self.ensure_page().await?;
        let s = &page.session_id;
        // Focus + optional clear via JS (non-mutating beyond the input's own value).
        let clear_js = if clear {
            format!(
                "(function(){{ var e=document.querySelector('{}'); if(!e) return 'nope'; e.focus(); e.value=''; return 'ok'; }})()",
                el.locator.replace('\'', "\\'")
            )
        } else {
            format!(
                "(function(){{ var e=document.querySelector('{}'); if(!e) return 'nope'; e.focus(); return 'ok'; }})()",
                el.locator.replace('\'', "\\'")
            )
        };
        let fr = self.evaluate(&clear_js).await?;
        if fr.as_str() == Some("nope") {
            return Err(BrowserError::target_not_found(&[format!(
                "locator no longer matches: {}",
                el.locator
            )]));
        }
        // Type the text via insertText (IME-compatible, no per-key events).
        self.conn()?
            .send("Input.insertText", json!({ "text": text }), Some(s))
            .await?;
        if submit {
            self.conn()?
                .send(
                    "Input.dispatchKeyEvent",
                    key_event("keyDown", "Enter", Some("Enter"), 0, Some("\r")),
                    Some(s),
                )
                .await?;
            self.conn()?
                .send(
                    "Input.dispatchKeyEvent",
                    key_event("keyUp", "Enter", Some("Enter"), 0, None),
                    Some(s),
                )
                .await?;
        }
        Ok(ActionResult {
            ok: true,
            resolved_by: by,
            detail: format!("typed {} chars into \"{}\"", text.chars().count(), el.text),
        })
    }

    /// Hover (menus that only open on hover).
    pub async fn hover(&self, target: &TargetDescriptor) -> Result<ActionResult, BrowserError> {
        let (_, el, by) = self.resolve_fresh(target).await?;
        let (x, y) = el.geometry.center();
        let page = self.ensure_page().await?;
        self.conn()?
            .send("Input.dispatchMouseEvent", mouse_event(MouseEventType::Moved, x, y, "none", 0), Some(&page.session_id))
            .await?;
        Ok(ActionResult { ok: true, resolved_by: by, detail: format!("hovered \"{}\"", el.text) })
    }

    /// Scroll the page or a specific container.
    pub async fn scroll(
        &self,
        target: Option<&TargetDescriptor>,
        direction: &str,
        amount_px: f64,
    ) -> Result<ActionResult, BrowserError> {
        let s = &self.ensure_page().await?.session_id;
        let sign = if direction.eq_ignore_ascii_case("up") { -1.0 } else { 1.0 };
        match target {
            None => {
                // Page-level scroll via JS (works headless).
                let js = format!(
                    "(function(){{ window.scrollBy(0, {}); return String(window.scrollY); }})()",
                    sign * amount_px
                );
                let r = self.evaluate(&js).await?;
                Ok(ActionResult {
                    ok: true,
                    resolved_by: ResolvedBy::Geometry,
                    detail: format!("scroll_y={}", r.as_str().unwrap_or("?")),
                })
            }
            Some(t) => {
                // Container scroll: resolve the container, scroll it via JS.
                let (_, el, by) = self.resolve_fresh(t).await?;
                let js = format!(
                    "(function(){{ var e=document.querySelector('{}'); if(!e) return 'nope'; e.scrollTop += {}; return String(e.scrollTop); }})()",
                    el.locator.replace('\'', "\\'"),
                    sign * amount_px
                );
                let r = self.evaluate(&js).await?;
                if r.as_str() == Some("nope") {
                    return Err(BrowserError::target_not_found(&[format!(
                        "container no longer matches: {}",
                        el.locator
                    )]));
                }
                Ok(ActionResult {
                    ok: true,
                    resolved_by: by,
                    detail: format!("container scroll_top={}", r.as_str().unwrap_or("?")),
                })
            }
        }
    }

    /// Drag from source to target (mouse down/move/up sequence).
    pub async fn drag(
        &self,
        source: &TargetDescriptor,
        target: &TargetDescriptor,
    ) -> Result<ActionResult, BrowserError> {
        let (_, src, by_s) = self.resolve_fresh(source).await?;
        let (_, tgt, by_t) = self.resolve_fresh(target).await?;
        let (sx, sy) = src.geometry.center();
        let (tx, ty) = tgt.geometry.center();
        let page = self.ensure_page().await?;
        let s = &page.session_id;
        self.conn()?.send("Input.dispatchMouseEvent", mouse_event(MouseEventType::Moved, sx, sy, "none", 0), Some(s)).await?;
        self.conn()?.send("Input.dispatchMouseEvent", mouse_event(MouseEventType::Pressed, sx, sy, "left", 1), Some(s)).await?;
        // Interpolate a few moves so drag UIs register motion.
        for i in 1..=5 {
            let t = i as f64 / 5.0;
            self.conn()?.send("Input.dispatchMouseEvent", mouse_event(MouseEventType::Moved, sx + (tx - sx) * t, sy + (ty - sy) * t, "left", 0), Some(s)).await?;
        }
        self.conn()?.send("Input.dispatchMouseEvent", mouse_event(MouseEventType::Released, tx, ty, "left", 1), Some(s)).await?;
        Ok(ActionResult {
            ok: true,
            resolved_by: by_s,
            detail: format!("dragged \"{}\" → \"{}\" (target by {by_t:?})", src.text, tgt.text),
        })
    }

    /// Native select option (JS value set + change event).
    pub async fn select_option(
        &self,
        target: &TargetDescriptor,
        value: &str,
    ) -> Result<ActionResult, BrowserError> {
        let (_, el, by) = self.resolve_fresh(target).await?;
        let js = format!(
            "(function(){{ var e=document.querySelector('{}'); if(!e) return 'nope'; e.value={:?}; e.dispatchEvent(new Event('change', {{bubbles:true}})); return String(e.value); }})()",
            el.locator.replace('\'', "\\'"),
            value
        );
        let r = self.evaluate(&js).await?;
        match r.as_str() {
            Some("nope") => Err(BrowserError::target_not_found(&[format!(
                "select no longer matches: {}",
                el.locator
            )])),
            Some(v) => Ok(ActionResult {
                ok: true,
                resolved_by: by,
                detail: format!("selected \"{v}\""),
            }),
            None => Err(BrowserError::Protocol("select result not a string".into())),
        }
    }

    /// Press a key with modifiers.
    pub async fn press_key(
        &self,
        key: &str,
        ctrl: bool,
        alt: bool,
        shift: bool,
        meta: bool,
    ) -> Result<ActionResult, BrowserError> {
        let page = self.ensure_page().await?;
        let modifiers = modifier_bitmask(ctrl, alt, shift, meta);
        let code = key.to_string();
        for kind in ["rawKeyDown", "keyUp"] {
            self.conn()?
                .send(
                    "Input.dispatchKeyEvent",
                    key_event(kind, key, Some(&code), modifiers, None),
                    Some(&page.session_id),
                )
                .await?;
        }
        Ok(ActionResult { ok: true, resolved_by: ResolvedBy::Geometry, detail: format!("pressed {key}") })
    }

    /// Wait for settle (quiet window) — bounded by hard timeout.
    pub async fn wait_settle(&self) -> Result<u64, BrowserError> {
        let expr = crate::extract::OBSERVER_JS.replace("%QUIET_MS%", &self.config.quiet_window.as_millis().to_string());
        self.evaluate(&expr).await?;
        // Await the installed promise, bounded by hard timeout.
        let wait_js = "window.__joeySettle";
        let fut = self.evaluate(wait_js);
        match tokio::time::timeout(self.config.hard_timeout, fut).await {
            Ok(Ok(v)) => Ok(v["waitedMs"].as_u64().unwrap_or(0)),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(BrowserError::SettleTimeout { waited_ms: self.config.hard_timeout.as_millis() as u64 }),
        }
    }
}
