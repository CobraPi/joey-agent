/** Keyboard navigation + visible focus + descriptive ARIA labels across
 * explorer/editor/workflow/run-panel/review (FR-027/SC-011). */

/** Install global keyboard shortcuts for the workspace. */
export function installKeyboardShortcuts(handlers: {
  onSearch?: () => void;
  onSave?: () => void;
  onToggleExplorer?: () => void;
  onToggleWorkflow?: () => void;
  onToggleReview?: () => void;
  onToggleReadiness?: () => void;
  onCancel?: () => void;
}): () => void {
  const handler = (e: KeyboardEvent) => {
    // Only respond to Ctrl/Cmd-based shortcuts to avoid interfering with typing.
    if (!(e.ctrlKey || e.metaKey)) return;

    const key = e.key.toLowerCase();
    const map: Record<string, (() => void) | undefined> = {
      k: handlers.onSearch,
      s: handlers.onSave,
      e: handlers.onToggleExplorer,
      w: handlers.onToggleWorkflow,
      r: handlers.onToggleReview,
      d: handlers.onToggleReadiness,
    };

    if (key === 'escape' || key === 'esc') {
      handlers.onCancel?.();
      return;
    }

    const action = map[key];
    if (action) {
      e.preventDefault();
      action();
    }
  };

  document.addEventListener('keydown', handler);
  return () => document.removeEventListener('keydown', handler);
}

/** Ensure an element has a visible focus ring and is keyboard-reachable. */
export function makeFocusable(el: HTMLElement): void {
  if (!el.hasAttribute('tabindex')) {
    el.setAttribute('tabindex', '0');
  }
  el.classList.add('keyboard-focusable');
}

/** Focus management: move focus to the first focusable element within a container. */
export function focusFirst(container: HTMLElement): void {
  const focusable = container.querySelector<HTMLElement>(
    'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])',
  );
  if (focusable) {
    focusable.focus();
  } else {
    container.focus();
  }
}

/** Trap focus within a modal/dialog region (for blocking prompts). */
export function trapFocus(container: HTMLElement): () => void {
  const selector =
    'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

  const handler = (e: KeyboardEvent) => {
    if (e.key !== 'Tab') return;
    const focusable = Array.from(container.querySelectorAll<HTMLElement>(selector));
    if (focusable.length === 0) return;

    const first = focusable[0];
    const last = focusable[focusable.length - 1];

    if (e.shiftKey) {
      if (document.activeElement === first) {
        e.preventDefault();
        last.focus();
      }
    } else {
      if (document.activeElement === last) {
        e.preventDefault();
        first.focus();
      }
    }
  };

  container.addEventListener('keydown', handler);
  return () => container.removeEventListener('keydown', handler);
}

/** Announce a message to screen readers via an aria-live region. */
export function announce(message: string, container?: HTMLElement): void {
  let live = container?.querySelector<HTMLDivElement>('[role="status"][aria-live="polite"]');
  if (!live) {
    live = document.createElement('div');
    live.setAttribute('role', 'status');
    live.setAttribute('aria-live', 'polite');
    live.className = 'sr-only';
    (container || document.body).appendChild(live);
  }
  live.textContent = message;
}
