// Command palette (T077, FR-034).
// ⌘K palette through which every action, artifact, requirement, and task is
// reachable by typing. Keeps CLI muscle memory intact.

export interface PaletteItem {
  id: string;
  label: string;
  category: string;
  action: () => void;
}

export class CommandPalette {
  private root: HTMLElement;
  private items: PaletteItem[] = [];
  private visible = false;
  private input: HTMLInputElement | null = null;

  constructor(root: HTMLElement) {
    this.root = root;
    this.setupKeyboardShortcut();
  }

  private setupKeyboardShortcut(): void {
    if (typeof document === 'undefined') return;
    document.addEventListener('keydown', (e) => {
      if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
        e.preventDefault();
        this.toggle();
      }
      if (e.key === 'Escape' && this.visible) {
        this.hide();
      }
    });
  }

  toggle(): void {
    this.visible ? this.hide() : this.show();
  }

  show(): void {
    this.visible = true;
    this.render(this.items);
    this.input?.focus();
  }

  hide(): void {
    this.visible = false;
    this.root.innerHTML = '';
  }

  render(items: PaletteItem[]): void {
    this.items = items;
    if (!this.visible) return;

    this.root.innerHTML = '';
    this.root.setAttribute('role', 'dialog');
    this.root.setAttribute('aria-label', 'Command palette (⌘K)');

    const overlay = document.createElement('div');
    overlay.style.cssText = 'position:fixed;inset:0;background:rgba(0,0,0,0.3);display:flex;align-items:flex-start;justify-content:center;padding-top:100px;z-index:1000;';
    overlay.addEventListener('click', (e) => {
      if (e.target === overlay) this.hide();
    });

    const panel = document.createElement('div');
    panel.style.cssText = 'background:white;border-radius:8px;box-shadow:0 8px 32px rgba(0,0,0,0.2);width:90%;max-width:500px;overflow:hidden;';

    this.input = document.createElement('input');
    this.input.type = 'text';
    this.input.placeholder = 'Type a command, artifact, requirement, or task…';
    this.input.style.cssText = 'width:100%;padding:16px;border:none;border-bottom:1px solid #eee;font-size:15px;box-sizing:border-box;';
    this.input.setAttribute('aria-label', 'Search commands');
    this.input.setAttribute('aria-controls', 'palette-results');

    const results = document.createElement('div');
    results.id = 'palette-results';
    results.setAttribute('role', 'listbox');
    results.style.cssText = 'max-height:400px;overflow-y:auto;';

    const updateResults = () => {
      const query = this.input?.value.toLowerCase() ?? '';
      results.innerHTML = '';
      const filtered = items.filter((i) => i.label.toLowerCase().includes(query) || i.category.toLowerCase().includes(query));
      filtered.slice(0, 20).forEach((item) => {
        const row = document.createElement('button');
        row.setAttribute('role', 'option');
        row.style.cssText = 'display:flex;justify-content:space-between;width:100%;text-align:left;padding:10px 16px;border:none;background:none;cursor:pointer;border-bottom:1px solid #f5f5f5;';
        row.innerHTML = `<span>${escapeHtml(item.label)}</span><span style="color:#999;font-size:11px;">${escapeHtml(item.category)}</span>`;
        row.addEventListener('click', () => {
          item.action();
          this.hide();
        });
        results.appendChild(row);
      });
    };

    this.input.addEventListener('input', updateResults);
    updateResults();

    panel.appendChild(this.input);
    panel.appendChild(results);
    overlay.appendChild(panel);
    this.root.appendChild(overlay);
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
