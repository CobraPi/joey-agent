// Run panel tool-call timeline extension (T097, FR-027).
//
// Extends the existing run-panel.ts to render a tool-call timeline (not a text
// log) where each read/write/search is a row with a state icon, stream agent
// output progressively into the destination widget, show elapsed time + phase
// label, and reattach to an in-flight run after tab close.
//
// This module is additive — the existing RunPanelView composes it.

export interface ToolCallRow {
  call_id: string;
  kind: 'read' | 'write' | 'search' | 'execute' | 'think';
  state: 'pending' | 'running' | 'done' | 'error';
  name: string;
  summary: string;
  started_at: number; // epoch ms
  elapsed_ms?: number;
  destination_artifact?: string;
}

const KIND_ICONS: Record<string, string> = {
  read: '📖',
  write: '✏',
  search: '🔍',
  execute: '⚡',
  think: '🧠',
};

const STATE_COLORS: Record<string, string> = {
  pending: '#9ca3af',
  running: '#2563eb',
  done: '#16a34a',
  error: '#dc2626',
};

/** Renders a tool-call timeline for a running step. */
export class ToolCallTimeline {
  private rows: Map<string, ToolCallRow> = new Map();
  private startTime: number = 0;

  constructor(private root: HTMLElement) {}

  /** Start a new run session. */
  startRun(): void {
    this.startTime = Date.now();
    this.rows.clear();
    this.render();
  }

  /** Add or update a tool-call row. Progressive streaming. */
  updateRow(row: ToolCallRow): void {
    if (row.state === 'done' || row.state === 'error') {
      row.elapsed_ms = Date.now() - row.started_at;
    }
    this.rows.set(row.call_id, row);
    this.render();
  }

  /** Reattach to an in-flight run (after tab close / reconnect). */
  reattach(existingRows: ToolCallRow[], startTime: number): void {
    this.startTime = startTime;
    this.rows.clear();
    existingRows.forEach((r) => this.rows.set(r.call_id, r));
    this.render();
  }

  render(): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', 'Tool-call timeline');

    // Elapsed time header.
    const elapsed = this.startTime > 0 ? Date.now() - this.startTime : 0;
    const header = document.createElement('div');
    header.style.cssText = 'display:flex;justify-content:space-between;padding:8px 12px;border-bottom:1px solid #eee;font-size:13px;';
    header.innerHTML = `<span style="font-weight:600;">Tool calls</span><span style="color:#666;font-family:monospace;">⏱ ${formatElapsed(elapsed)}</span>`;
    this.root.appendChild(header);

    // Rows.
    const list = document.createElement('div');
    Array.from(this.rows.values()).forEach((row) => {
      list.appendChild(this.renderRow(row));
    });
    this.root.appendChild(list);

    // Progressive update timer for running state.
    if (Array.from(this.rows.values()).some((r) => r.state === 'running')) {
      requestAnimationFrame(() => this.render());
    }
  }

  private renderRow(row: ToolCallRow): HTMLElement {
    const div = document.createElement('div');
    const color = STATE_COLORS[row.state] ?? '#999';
    const icon = KIND_ICONS[row.kind] ?? '?';
    div.style.cssText = `display:flex;align-items:center;gap:8px;padding:6px 12px;border-bottom:1px solid #f5f5f5;font-size:13px;`;
    div.setAttribute('aria-label', `${row.kind} ${row.name}: ${row.state}${row.summary ? '. ' + row.summary : ''}`);

    const stateIcon = row.state === 'running' ? '◐' : row.state === 'done' ? '✓' : row.state === 'error' ? '✗' : '○';

    div.innerHTML = `<span style="width:20px;text-align:center;" aria-hidden="true">${icon}</span>
      <span style="flex:1;"><strong>${escapeHtml(row.name)}</strong> <span style="color:#999;">${escapeHtml(row.summary)}</span></span>
      <span style="color:${color};width:20px;text-align:center;" aria-hidden="true">${stateIcon}</span>
      ${row.elapsed_ms ? `<span style="color:#999;font-size:11px;font-family:monospace;">${formatElapsed(row.elapsed_ms)}</span>` : ''}`;

    return div;
  }
}

function formatElapsed(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  if (ms < 60000) return `${(ms / 1000).toFixed(1)}s`;
  return `${Math.floor(ms / 60000)}m${Math.floor((ms % 60000) / 1000)}s`;
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
