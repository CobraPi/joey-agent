// Tree diff widget (T044, FR-009).
// Renders ProjectStructureNode as a tree diff with exists/planned-missing/
// not-in-plan status. Each missing node offers "scaffold this".

export interface TreeDiffData {
  nodes: Array<{ path: string; status: 'exists' | 'planned_missing' | 'not_in_plan' }>;
}

const STATUS_META: Record<string, { icon: string; color: string; label: string }> = {
  exists: { icon: '✓', color: '#16a34a', label: 'Exists' },
  planned_missing: { icon: '✗', color: '#dc2626', label: 'Planned but missing' },
  not_in_plan: { icon: '?', color: '#d97706', label: 'Not in plan' },
};

export class TreeDiff {
  constructor(private root: HTMLElement) {}

  render(data: TreeDiffData, onScaffold?: (path: string) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', 'Project structure tree diff');

    const list = document.createElement('div');
    list.style.cssText = 'font-family:monospace;font-size:13px;';

    data.nodes.forEach((n) => {
      const meta = STATUS_META[n.status] ?? STATUS_META.exists;
      const row = document.createElement('div');
      row.style.cssText = `display:flex;align-items:center;gap:8px;padding:4px 0;border-bottom:1px solid #f3f4f6;`;
      row.innerHTML = `<span style="color:${meta.color};width:20px;" aria-hidden="true">${meta.icon}</span><span style="flex:1;">${escapeHtml(n.path)}</span><span style="color:#999;font-size:11px;">${meta.label}</span>`;

      if (n.status === 'planned_missing') {
        const btn = document.createElement('button');
        btn.textContent = 'Scaffold';
        btn.style.cssText = 'padding:2px 8px;background:#2563eb;color:white;border:none;border-radius:3px;cursor:pointer;font-size:11px;';
        btn.setAttribute('aria-label', `Scaffold missing file ${n.path}`);
        btn.addEventListener('click', () => onScaffold?.(n.path));
        row.appendChild(btn);
      }

      row.setAttribute('aria-label', `${n.path}: ${meta.label}`);
      list.appendChild(row);
    });

    this.root.appendChild(list);
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
