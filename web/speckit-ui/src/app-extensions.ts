// Deep-link state restoration (T078, FR-038).
// Every feature, node, run, and review state has a deep link. Selection,
// filters, scroll position, and staged status survive view changes and
// browser Back/Forward.

export interface DeepLinkState {
  feature_id: string;
  node_id?: string;
  view?: string;
  scroll_pos?: number;
}

/** Serialize state into a URL hash string. */
export function serializeState(state: DeepLinkState): string {
  const parts: string[] = [`f=${encodeURIComponent(state.feature_id)}`];
  if (state.node_id) parts.push(`n=${encodeURIComponent(state.node_id)}`);
  if (state.view) parts.push(`v=${encodeURIComponent(state.view)}`);
  if (state.scroll_pos !== undefined) parts.push(`s=${state.scroll_pos}`);
  return parts.join('&');
}

/** Restore state from a URL hash string. */
export function restoreState(hash: string): DeepLinkState | null {
  const clean = hash.replace(/^#/, '');
  if (!clean) return null;

  const params = new Map<string, string>();
  clean.split('&').forEach((pair) => {
    const [key, value] = pair.split('=');
    if (key && value !== undefined) {
      params.set(key, decodeURIComponent(value));
    }
  });

  const featureId = params.get('f');
  if (!featureId) return null;

  return {
    feature_id: featureId,
    node_id: params.get('n') ?? undefined,
    view: params.get('v') ?? undefined,
    scroll_pos: params.has('s') ? Number(params.get('s')) : undefined,
  };
}

/** Push state to the URL hash (Back/Forward navigable). */
export function pushState(state: DeepLinkState): void {
  if (typeof window === 'undefined') return;
  const hash = serializeState(state);
  window.history.pushState(state, '', `#${hash}`);
}

/** Listen for Back/Forward navigation. */
export function onPopState(callback: (state: DeepLinkState | null) => void): () => void {
  if (typeof window === 'undefined') return () => {};
  const handler = () => {
    const state = restoreState(window.location.hash);
    callback(state);
  };
  window.addEventListener('popstate', handler);
  return () => window.removeEventListener('popstate', handler);
}
