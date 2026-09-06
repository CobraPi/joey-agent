//! Action verbs: pre-action re-scan, cascade resolution, CDP Input events
//! (research.md D2; FR-005..009; contracts/browser-tools.md).

use serde_json::{json, Value};

use crate::cdp::domains::{key_event, modifier_bitmask, mouse_event, MouseEventType};
use crate::cdp::BrowserError;
use crate::refs::{ElementRefRegistry, ResolvedBy, TargetDescriptor};
use crate::session::BrowserManager;

/// JSON-encode a string for interpolation into a JS string literal.
/// Rust `{:?}` Debug escaping emits sequences like `\u{1}` that are
/// INVALID JavaScript; serde_json emits `\u0001`, which is valid.
pub(crate) fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Walk the DOM exactly like the scanner (same-origin iframe documents +
/// shadow roots) and run `op` — a comma expression over `el` evaluating to
/// a string — on the first element matching by locator (exact → suffix) or
/// normalized text (mirroring the resolver cascade). Returns 'nope' when
/// not found.
///
/// Bug fix: locators SCAN_JS builds for elements inside iframes/shadow
/// roots cannot be resolved by `document.querySelector` on the top
/// document — they must be re-resolved through the owning frame/shadow
/// root at action time.
const FIND_AND_OP_JS: &str = r#"
(function(){
  const LOC = %LOCATOR%;
  const TXT = %TEXT%;
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
  function locatorFor(el) {
    if (el.id) return '#' + CSS.escape(el.id);
    const parts = [];
    let node = el;
    while (node && node !== document && parts.length < 8) {
      const parent = node.parentElement || (node.getRootNode && node.getRootNode().host) || null;
      const tag = node.tagName ? node.tagName.toLowerCase() : 'unknown';
      if (parent) {
        const sibs = Array.from(parent.children || []).filter((c) => c.tagName === node.tagName);
        const idx = sibs.indexOf(node) + 1;
        parts.unshift(sibs.length > 1 ? tag + ':nth-of-type(' + idx + ')' : tag);
      } else {
        parts.unshift(tag);
      }
      node = parent;
    }
    return parts.join(' > ') || el.tagName.toLowerCase();
  }
  let exact = null, suffix = null, byText = null;
  function consider(el) {
    if (el.nodeType !== 1) return;
    let loc = null;
    try { loc = locatorFor(el); } catch (e) { return; }
    if (LOC && loc === LOC && exact === null) exact = el;
    else if (LOC && typeof loc === 'string' && loc.endsWith(LOC) && suffix === null) suffix = el;
    else if (TXT && byText === null) {
      const txt = norm(el.getAttribute('aria-label') || el.innerText || el.value || el.placeholder || '');
      if (txt === TXT) byText = el;
    }
  }
  function walk(root) {
    if (!root) return;
    let nodes;
    try { nodes = root.querySelectorAll('*'); } catch (e) { return; }
    for (const el of nodes) consider(el);
    try {
      for (const f of root.querySelectorAll('iframe')) {
        if (f.contentDocument) walk(f.contentDocument);
      }
    } catch (e) { /* skip */ }
    for (const host of nodes) {
      if (host.shadowRoot) walk(host.shadowRoot);
    }
  }
  walk(document);
  const el = exact || suffix || byText;
  if (!el) return 'nope';
  return %OP%;
})()
"#;

fn find_and_op_js(locator: &str, text: &str, op: &str) -> String {
    FIND_AND_OP_JS
        .replace("%LOCATOR%", &js_str(locator))
        .replace("%TEXT%", &js_str(text))
        .replace("%OP%", op)
}

/// op: focus the element.
const FOCUS_OP: &str = "(el.focus(), 'ok')";
/// op: focus + clear a form control's value.
const FOCUS_CLEAR_OP: &str = "(el.focus(), el.value = '', 'ok')";

/// op: set a form control's value (JSON-escaped — never Rust `{:?}`) and
/// fire `change`.
fn select_op(value: &str) -> String {
    format!(
        "(el.value = {}, el.dispatchEvent(new Event('change', {{bubbles:true}})), String(el.value))",
        js_str(value)
    )
}

/// op: scroll a container by `delta` px.
fn scroll_op(delta: f64) -> String {
    format!("(el.scrollTop += {delta}, String(el.scrollTop))")
}

/// True when `key` denotes a single printable character typed without
/// ctrl/alt/meta (shift is allowed: shift+a still types). Such keys must
/// be dispatched as `keyDown` WITH `text` — rawKeyDown with text omitted
/// types nothing.
fn is_printable_key(key: &str, ctrl: bool, alt: bool, meta: bool) -> bool {
    if ctrl || alt || meta {
        return false;
    }
    let mut chars = key.chars();
    let Some(c) = chars.next() else { return false };
    chars.next().is_none() && !c.is_control()
}

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
                // Non-finite rects (CSS calc/transform edge cases) must never
                // reach the ordering/resolution paths — skip such rows.
                let g = &el.geometry;
                if !(g.x.is_finite() && g.y.is_finite() && g.w.is_finite() && g.h.is_finite()) {
                    continue;
                }
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
        // Focus + optional clear via JS (non-mutating beyond the input's own
        // value). find_and_op_js resolves the element ONCE through the same
        // frame/shadow-root cascade the scanner uses — document.querySelector
        // cannot reach elements inside iframes/shadow roots.
        let clear_js = find_and_op_js(
            &el.locator,
            &el.text,
            if clear { FOCUS_CLEAR_OP } else { FOCUS_OP },
        );
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
        let _s = &self.ensure_page().await?.session_id;
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
                let js = find_and_op_js(&el.locator, &el.text, &scroll_op(sign * amount_px));
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
        let js = find_and_op_js(&el.locator, &el.text, &select_op(value));
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
        if is_printable_key(key, ctrl, alt, meta) {
            // Printable keys must be dispatched as keyDown WITH `text` —
            // rawKeyDown with text omitted types nothing.
            let ch = key.to_string();
            for kind in ["keyDown", "keyUp"] {
                self.conn()?
                    .send(
                        "Input.dispatchKeyEvent",
                        key_event(kind, key, Some(&code), modifiers, Some(&ch)),
                        Some(&page.session_id),
                    )
                    .await?;
            }
        } else {
            for kind in ["rawKeyDown", "keyUp"] {
                self.conn()?
                    .send(
                        "Input.dispatchKeyEvent",
                        key_event(kind, key, Some(&code), modifiers, None),
                        Some(&page.session_id),
                    )
                    .await?;
            }
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
