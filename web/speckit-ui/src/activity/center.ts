// Activity center (T073, FR-026).
// Unified Agent Activity Center rendering questions, permissions, runs, and
// review decisions chronologically with origin tags.

export interface ActivityData {
  events: Array<{
    type: 'question' | 'permission' | 'run' | 'review_decision';
    timestamp: string;
    origin_tag: 'draft' | 'derived' | 'proposed_patch';
    summary: string;
  }>;
}

const TYPE_META: Record<string, { icon: string; color: string }> = {
  question: { icon: '?', color: '#2563eb' },
  permission: { icon: '🔒', color: '#d97706' },
  run: { icon: '▶', color: '#16a34a' },
  review_decision: { icon: '✓', color: '#7c3aed' },
};

const ORIGIN_COLORS: Record<string, string> = {
  draft: '#6b7280',
  derived: '#d97706',
  proposed_patch: '#2563eb',
};

export class ActivityCenter {
  constructor(private root: HTMLElement) {}

  render(data: ActivityData, onAction?: (type: string, timestamp: string) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', 'Agent activity center');

    if (data.events.length === 0) {
      this.root.innerHTML = '<p style="color:#999;padding:12px;">No activity yet.</p>';
      return;
    }

    const list = document.createElement('div');
    list.style.cssText = 'font-size:13px;';

    data.events.forEach((e) => {
      const meta = TYPE_META[e.type] ?? TYPE_META.run;
      const originColor = ORIGIN_COLORS[e.origin_tag] ?? '#999';
      const row = document.createElement('button');
      row.style.cssText = `display:flex;align-items:center;gap:8px;width:100%;text-align:left;padding:8px;border:none;border-bottom:1px solid #eee;background:none;cursor:pointer;`;
      row.setAttribute('aria-label', `${e.type}: ${e.summary} at ${e.timestamp}`);
      row.innerHTML = `<span style="color:${meta.color};width:20px;" aria-hidden="true">${meta.icon}</span>
        <span style="flex:1;">${escapeHtml(e.summary)}</span>
        <span style="color:${originColor};font-size:11px;text-transform:uppercase;">${escapeHtml(e.origin_tag)}</span>
        <span style="color:#999;font-size:11px;">${escapeHtml(e.timestamp)}</span>`;
      row.addEventListener('click', () => onAction?.(e.type, e.timestamp));
      list.appendChild(row);
    });

    this.root.appendChild(list);
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
