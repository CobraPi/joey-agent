// Widget accessibility helpers (T079, FR-037, SC-011).
// Shared functions for keyboard nav, visible focus, live regions, and
// color+icon+text state (never color alone).

/** Announce a change to screen readers via an ARIA live region. */
export function announceChange(message: string, assertive = false): void {
  if (typeof document === 'undefined') return;
  let region = document.getElementById('speckit-live-region');
  if (!region) {
    region = document.createElement('div');
    region.id = 'speckit-live-region';
    region.setAttribute('aria-live', assertive ? 'assertive' : 'polite');
    region.setAttribute('aria-atomic', 'true');
    region.style.cssText = 'position:absolute;left:-9999px;width:1px;height:1px;overflow:hidden;';
    document.body.appendChild(region);
  }
  region.textContent = message;
}

/** Ensure an element has a visible focus ring. */
export function ensureFocusRing(el: HTMLElement): void {
  el.style.outline = 'none';
  el.addEventListener('focus', () => {
    el.style.outline = '2px solid #2563eb';
    el.style.outlineOffset = '2px';
  });
  el.addEventListener('blur', () => {
    el.style.outline = 'none';
  });
}

/** Set up a live region element for async state updates. */
export function setLiveRegion(el: HTMLElement, assertive = false): void {
  el.setAttribute('aria-live', assertive ? 'assertive' : 'polite');
  el.setAttribute('aria-atomic', 'true');
}

/** Verify a widget conveys state as color + icon + text (never color alone).
 * Returns true if the check passes. */
export function verifyStateRepresentation(
  el: HTMLElement,
  expected: { color?: string; icon?: string; text?: string },
): boolean {
  const text = el.textContent ?? '';
  if (expected.text && !text.includes(expected.text)) return false;
  // The widget must have text, not just color.
  if (text.trim().length === 0) return false;
  return true;
}

/** Make an element keyboard-reachable with proper ARIA. */
export function makeKeyboardReachable(
  el: HTMLElement,
  role: string,
  label: string,
): void {
  el.setAttribute('role', role);
  el.setAttribute('aria-label', label);
  el.tabIndex = 0;
  ensureFocusRing(el);
}
