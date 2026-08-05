// Cross-view selection / traceability spine (T068, FR-021).
//
// Selecting any SemanticId broadcasts via an event bus; every open view dims
// unrelated nodes, highlights the traceability spine (principle → story →
// requirement → task → file → check), and scrolls to the relevant widget.

export type SelectionEvent = {
  semanticId: string;
  source: string;
};

type SelectionListener = (event: SelectionEvent) => void;

/** Event bus for cross-view selection highlighting. */
export class SelectionBus {
  private listeners: SelectionListener[] = [];
  private current: SelectionEvent | null = null;

  /** Broadcast a selection. Every open view receives it. */
  select(semanticId: string, source: string): void {
    this.current = { semanticId, source };
    this.listeners.forEach((fn) => fn(this.current!));
  }

  /** Clear the selection. */
  clear(): void {
    this.current = null;
    this.listeners.forEach((fn) => fn({ semanticId: '', source: 'clear' }));
  }

  /** Register a listener. Returns an unsubscribe function. */
  on(listener: SelectionListener): () => void {
    this.listeners.push(listener);
    return () => {
      this.listeners = this.listeners.filter((l) => l !== listener);
    };
  }

  /** The current selection. */
  get currentSelection(): SelectionEvent | null {
    return this.current;
  }
}

/** Highlight a DOM element based on whether it's on the traceability spine
 * of the current selection. Off-spine elements dim; on-spine elements stay. */
export function applySpineHighlight(
  container: HTMLElement,
  selectedId: string,
  spineMap: Map<string, string[]>,
): void {
  const spine = spineMap.get(selectedId) ?? [];
  const spineSet = new Set([selectedId, ...spine]);

  // Each widget element carries a data-semantic-id attribute.
  container.querySelectorAll('[data-semantic-id]').forEach((el) => {
    const id = el.getAttribute('data-semantic-id') ?? '';
    if (spineSet.has(id)) {
      (el as HTMLElement).style.opacity = '1';
      (el as HTMLElement).style.outline = '2px solid #2563eb';
    } else {
      (el as HTMLElement).style.opacity = '0.3';
      (el as HTMLElement).style.outline = '';
    }
  });
}
