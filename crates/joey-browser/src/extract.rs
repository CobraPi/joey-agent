//! Injected-JS extraction pipeline: the deep scanner (shadow-piercing,
//! frame-traversing), the MutationObserver settle probe, and overlay
//! heuristics (research.md D2/D3/D4/D5; FR-001..004a, FR-010, FR-011).

/// Deep interactive-element scanner (scan.js) — property-based, ZERO DOM
/// mutation (D2). Returns a JSON-serializable array; the Rust side assigns
/// refids. Depth-capped (spec edge case: circular/deep nesting terminates,
/// capped regions reported).
pub const SCAN_JS: &str = r#"
(() => {
  const MAX_DEPTH = 24;
  const INTERACTIVE = 'button, a, input, select, textarea, [role="button"], [role="menuitem"], [role="option"], [role="tab"], [role="checkbox"], [role="radio"], [role="switch"], [role="link"], [contenteditable="true"]';
  const out = [];
  const cappedRegions = [];
  let capHit = false;

  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const visible = (el) => {
    const r = el.getBoundingClientRect();
    if (r.width <= 0 && r.height <= 0) return false;
    const st = getComputedStyle(el);
    return st.visibility !== 'hidden' && st.display !== 'none';
  };

  function locatorFor(el, root) {
    // Structural CSS locator, robust to text change; built without mutation.
    if (el.id) return '#' + CSS.escape(el.id);
    const parts = [];
    let node = el;
    while (node && node !== root && parts.length < 8) {
      const parent = node.parentElement || (node.getRootNode && node.getRootNode().host) || null;
      const tag = node.tagName ? node.tagName.toLowerCase() : 'unknown';
      if (parent) {
        const sibs = Array.from(parent.children || []).filter(c => c.tagName === node.tagName);
        const idx = sibs.indexOf(node) + 1;
        parts.unshift(sibs.length > 1 ? `${tag}:nth-of-type(${idx})` : tag);
      } else {
        parts.unshift(tag);
      }
      node = parent;
    }
    return parts.join(' > ') || el.tagName.toLowerCase();
  }

  function describe(el, frame, depth) {
    const r = el.getBoundingClientRect();
    const role = el.tagName.toLowerCase() === 'a' ? 'link'
      : (el.getAttribute('role') || el.tagName.toLowerCase());
    const attrs = {};
    for (const a of ['aria-label', 'placeholder', 'href', 'name', 'type']) {
      const v = el.getAttribute(a);
      if (v) attrs[a] = v.length > 80 ? v.slice(0, 80) : v;
    }
    let text;
    if (el.tagName === 'SELECT') {
      // Native selects render the selected option's label, not innerText.
      const opt = el.options && el.options[el.selectedIndex];
      text = norm((opt && opt.text) || el.getAttribute('aria-label') || '');
    } else {
      text = norm(el.getAttribute('aria-label') || el.innerText || el.value || el.placeholder || '');
    }
    if (text.length > 120) text = Array.from(text).slice(0, 120).join('');
    out.push({
      role, text, frame,
      locator: locatorFor(el, document),
      geometry: { x: r.x, y: r.y + window.scrollY, w: r.width, h: r.height },
      attributes: attrs,
      interactable: visible(el) && !el.disabled,
      value: (el.value !== undefined && el.value !== null && el.type !== 'password') ? String(el.value).slice(0, 80) : null,
    });
  }

  function walk(root, frame, depth) {
    if (depth > MAX_DEPTH) { capHit = true; return; }
    let nodes;
    try { nodes = root.querySelectorAll(INTERACTIVE); } catch (e) { return; }
    for (const el of nodes) {
      try { describe(el, frame, depth); } catch (e) { /* hostile element: skip */ }
    }
    // Piercing: same-origin iframes reachable via contentDocument (iframes
    // are not in the INTERACTIVE selector — traverse them explicitly).
    try {
      for (const f of root.querySelectorAll('iframe')) {
        if (f.contentDocument) {
          walk(f.contentDocument, 'iframe:' + (f.name || f.id || f.src.slice(-30) || 'unnamed'), depth + 1);
        }
      }
    } catch (e) { /* skip */ }
    // Shadow hosts that are not themselves interactive.
    try {
      for (const host of root.querySelectorAll('*')) {
        if (host.shadowRoot) {
          walk(host.shadowRoot, frame + '|shadow', depth + 1);
        }
      }
    } catch (e) { /* skip */ }
  }

  walk(document, 'main', 0);
  return JSON.stringify({ elements: out, capped: capHit });
})()
"#;

/// MutationObserver settle probe (observer.js) — quiet-window promise.
/// Sentinel-marked: the observer's own bookkeeping mutations (none — we only
/// observe) and our marker overlays are excluded via the data attribute.
pub const OBSERVER_JS: &str = r#"
((quietMs) => {
  const SENTINEL = 'data-joey-sentinel';
  window.__joeySettle = new Promise((resolve) => {
    let timer = null;
    let last = Date.now();
    const obs = new MutationObserver((muts) => {
      for (const m of muts) {
        if (m.target && m.target.nodeType === 1 && m.target.hasAttribute && m.target.hasAttribute(SENTINEL)) continue;
        last = Date.now();
        return;
      }
    });
    obs.observe(document, { subtree: true, childList: true, attributes: true, characterData: true });
    const poll = setInterval(() => {
      if (Date.now() - last >= quietMs) {
        clearInterval(poll); obs.disconnect();
        resolve({ settled: true, waitedMs: Date.now() - last });
      }
    }, 100);
    // Hard cap handled Rust-side via timeout; promise stays pending then.
  });
  return 'installed';
})(%QUIET_MS%)
"#;

/// Overlay detection + conservative dismissal identification (overlays.js).
pub const OVERLAYS_JS: &str = r#"
(() => {
  const norm = (s) => (s || '').replace(/\s+/g, ' ').trim();
  const CONSENT_HINTS = [
    'accept cookies', 'we use cookies', 'cookie policy', 'consent',
    'privacy preferences', 'allow all cookies', 'reject all',
    'reject cookies', 'manage preferences', 'your privacy choices',
  ];
  const SAFE_DISMISS = /^(accept|reject|decline|close|dismiss|got it|okay|ok|i agree|manage|preferences|settings|not now|no thanks|continue without)/i;
  const SAFE_DISMISS_EXACT = new Set(['x', '×', '✕', '✖', '⨯', 'close']);

  const findings = [];

  function isBlocking(el) {
    const st = getComputedStyle(el);
    if (st.position !== 'fixed' && st.position !== 'sticky' && st.position !== 'absolute') return false;
    const r = el.getBoundingClientRect();
    // Covers a meaningful chunk of the viewport?
    if (r.width < window.innerWidth * 0.3 && r.height < window.innerHeight * 0.2) return false;
    const z = parseInt(st.zIndex || '0', 10);
    return z >= 100 || st.position === 'fixed';
  }

  for (const el of document.querySelectorAll('div, section, aside, dialog, [role="dialog"], [role="alertdialog"]')) {
    if (!isBlocking(el)) continue;
    const text = norm(el.innerText || '').slice(0, 300);
    const isDialog = el.hasAttribute('open') || el.getAttribute('role') === 'dialog' || el.tagName === 'DIALOG';
    const consentHit = CONSENT_HINTS.some((h) => text.toLowerCase().includes(h));
    // Safe dismissal control: a button/link whose label is a standard verb,
    // present inside this overlay.
    let dismiss = null;
    for (const b of el.querySelectorAll('button, a, [role="button"]')) {
      const label = norm(b.innerText || b.getAttribute('aria-label') || '');
      if (SAFE_DISMISS.test(label) || SAFE_DISMISS_EXACT.has(label.toLowerCase())) {
        // Prefer reject/close-style over accept (conservative: minimize
        // consent grants made on the user's behalf).
        if (!dismiss || /^(reject|decline|close|dismiss|not now|no thanks)/i.test(label)) {
          dismiss = { label, selector: describeFor(b) };
        }
      }
    }
    const kind = consentHit ? 'consent' : isDialog ? 'dialog' : 'unknown';
    findings.push({
      kind,
      description: kind === 'consent' ? 'Consent/notification overlay' : (isDialog ? 'Modal dialog' : 'Blocking overlay'),
      frame: 'main',
      hasSafeDismissal: !!dismiss,
      dismissalLabel: dismiss ? dismiss.label : null,
    });
  }
  return JSON.stringify({ overlays: findings });

  function describeFor(el) {
    if (el.id) return '#' + CSS.escape(el.id);
    return null; // coordinate dismissal handled Rust-side when null
  }
})()
"#;

/// Marker overlay injection for Set-of-Mark capture (vision.rs consumer).
/// All injected nodes carry the sentinel attribute so (a) the settle
/// observer ignores them and (b) cleanup is trivial.
pub const MARKERS_JS: &str = r#"
((markerSpec) => {
  const SENTINEL = 'data-joey-sentinel';
  document.querySelectorAll(`[${SENTINEL}]`).forEach((n) => n.remove());
  const layer = document.createElement('div');
  layer.setAttribute(SENTINEL, '1');
  layer.style.cssText = 'position:fixed;inset:0;z-index:2147483647;pointer-events:none;';
  for (const m of markerSpec) {
    const box = document.createElement('div');
    box.setAttribute(SENTINEL, '1');
    box.style.cssText = `position:absolute;left:${m.x}px;top:${m.y}px;width:${m.w}px;height:${m.h}px;border:2px solid #ff3b30;background:rgba(255,59,48,0.18);color:#fff;font:bold 12px monospace;display:flex;align-items:flex-start;justify-content:flex-start;padding:1px 3px;`;
    box.textContent = m.id;
    layer.appendChild(box);
    if (m.label) {
      const tag = document.createElement('div');
      tag.setAttribute(SENTINEL, '1');
      tag.style.cssText = `position:absolute;left:${m.x}px;top:${m.y + m.h + 2}px;color:#fff;background:#ff3b30;font:bold 11px monospace;padding:1px 4px;`;
      tag.textContent = m.label;
      layer.appendChild(tag);
    }
  }
  document.documentElement.appendChild(layer);
  return 'ok';
})(%MARKER_SPEC%)
"#;

/// Marker cleanup (after capture).
pub const CLEANUP_MARKERS_JS: &str =
    r#"document.querySelectorAll('[data-joey-sentinel]').forEach((n) => n.remove()); 'cleaned'"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_are_nonempty_and_balanced() {
        for (name, js) in [
            ("scan", SCAN_JS),
            ("observer", OBSERVER_JS),
            ("overlays", OVERLAYS_JS),
            ("markers", MARKERS_JS),
        ] {
            assert!(!js.trim().is_empty(), "{name} empty");
            // Every script must be an expression (IIFE) — sanity via braces.
            let opens = js.matches('(').count();
            let closes = js.matches(')').count();
            assert_eq!(opens, closes, "{name} unbalanced parens");
        }
    }

    #[test]
    fn placeholders_present() {
        assert!(OBSERVER_JS.contains("%QUIET_MS%"));
        assert!(MARKERS_JS.contains("%MARKER_SPEC%"));
    }

    #[test]
    fn scan_js_pierces_shadow_and_frames_in_source() {
        assert!(SCAN_JS.contains("shadowRoot"));
        assert!(SCAN_JS.contains("contentDocument"));
        assert!(SCAN_JS.contains("nth-of-type"));
        assert!(!SCAN_JS.contains("setAttribute"), "scanner must not mutate DOM");
    }

    #[test]
    fn overlays_sentinel_and_consent_hints_present() {
        assert!(OVERLAYS_JS.contains("accept cookies"));
        assert!(MARKERS_JS.contains("data-joey-sentinel"));
        assert!(CLEANUP_MARKERS_JS.contains("data-joey-sentinel"));
    }
}
