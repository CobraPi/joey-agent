/** Resizable/collapsible/reorderable workspace panes via split.js (FR-002/026). */
export class PaneLayout {
  private el: HTMLElement;
  private panes: Map<string, HTMLElement> = new Map();
  private splitInstance: any = null;

  constructor() {
    this.el = document.createElement('div');
    this.el.className = 'pane-layout';
    this.el.setAttribute('role', 'region');
    this.el.setAttribute('aria-label', 'Workspace panes');
  }

  get element(): HTMLElement {
    return this.el;
  }

  /** Add a pane with the given id and content element. */
  addPane(id: string, label: string, content: HTMLElement): void {
    const pane = document.createElement('div');
    pane.className = 'pane';
    pane.dataset.paneId = id;
    pane.setAttribute('role', 'region');
    pane.setAttribute('aria-label', label);

    const header = document.createElement('div');
    header.className = 'pane-header';
    header.innerHTML = `<span class="pane-label">${esc(label)}</span>`;

    const collapseBtn = document.createElement('button');
    collapseBtn.type = 'button';
    collapseBtn.className = 'pane-collapse';
    collapseBtn.setAttribute('aria-label', `Collapse ${label}`);
    collapseBtn.textContent = '−';
    collapseBtn.addEventListener('click', () => {
      content.hidden = !content.hidden;
      collapseBtn.textContent = content.hidden ? '+' : '−';
    });
    header.appendChild(collapseBtn);

    pane.appendChild(header);
    pane.appendChild(content);
    this.el.appendChild(pane);
    this.panes.set(id, pane);

    this.initSplit();
  }

  removePane(id: string): void {
    const pane = this.panes.get(id);
    if (pane) {
      pane.remove();
      this.panes.delete(id);
      this.initSplit();
    }
  }

  private initSplit(): void {
    if (this.splitInstance) {
      try {
        this.splitInstance.destroy();
      } catch {
        // split.js destroy may throw if already destroyed; ignore.
      }
      this.splitInstance = null;
    }

    const elements = Array.from(this.panes.values());
    if (elements.length < 2) return;

    // Dynamically import split.js (graceful degradation if not installed).
    import('split.js')
      .then((Split) => {
        this.splitInstance = Split.default(elements, {
          sizes: this.computeSizes(elements.length),
          gutterSize: 6,
          minSize: 200,
        });
      })
      .catch(() => {
        // split.js not installed — panes stack naturally via CSS flexbox.
      });
  }

  private computeSizes(count: number): number[] {
    const equal = 100 / count;
    return Array(count).fill(equal);
  }

  /** Get the saved pane layout for persistence (FR-026). */
  getLayout(): unknown {
    const sizes: Record<string, number> = {};
    if (this.splitInstance) {
      try {
        const s = this.splitInstance.getSizes() as number[];
        const ids = Array.from(this.panes.keys());
        ids.forEach((id, i) => {
          sizes[id] = s[i] ?? 100 / ids.length;
        });
      } catch {
        // ignore
      }
    }
    return sizes;
  }
}

function esc(s: string): string {
  const div = document.createElement('div');
  div.textContent = s;
  return div.innerHTML;
}
