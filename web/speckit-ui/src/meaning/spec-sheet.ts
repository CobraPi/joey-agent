// Spec sheet widget (T042, FR-009).
// Renders TechnicalContextField as labelled tiles. Unresolved values render
// as directly-clickable controls, not color-only text.

export interface SpecSheetData {
  fields: Array<{ key: string; value: string | null }>;
}

export class SpecSheet {
  constructor(private root: HTMLElement) {}

  render(data: SpecSheetData, onResolve?: (key: string) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', 'Technical context');

    const grid = document.createElement('div');
    grid.style.cssText = 'display:grid;grid-template-columns:repeat(auto-fill,minmax(200px,1fr));gap:12px;';

    data.fields.forEach((f) => {
      const tile = document.createElement('div');
      tile.style.cssText = 'border:1px solid #e5e7eb;border-radius:8px;padding:12px;background:white;';
      tile.innerHTML = `<p style="margin:0;font-size:11px;text-transform:uppercase;color:#666;">${escapeHtml(f.key)}</p>`;

      if (f.value) {
        const val = document.createElement('p');
        val.textContent = f.value;
        val.style.cssText = 'margin:4px 0 0;font-weight:600;';
        tile.appendChild(val);
      } else {
        // FR-009: unresolved values are directly-clickable controls, not
        // color-only text.
        const btn = document.createElement('button');
        btn.textContent = '+ Resolve';
        btn.style.cssText = 'margin-top:4px;padding:4px 12px;background:#2563eb;color:white;border:none;border-radius:4px;cursor:pointer;font-size:12px;';
        btn.setAttribute('aria-label', `Resolve value for ${f.key}`);
        btn.addEventListener('click', () => onResolve?.(f.key));
        tile.appendChild(btn);
      }
      grid.appendChild(tile);
    });

    this.root.appendChild(grid);
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
