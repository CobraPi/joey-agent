// Atlas landing view (T033, FR-004/005).
//
// The bento landing view: next action, progress, health, binding, artifacts,
// recent activity. Each tile opens the relevant stage without losing context.
//
// Vanilla-TS web component (research.md §1 — no React).

export interface AtlasData {
  feature_id: string;
  next_action: NextAction;
  progress: { done_tasks: number; total_tasks: number; ratio: number };
  health: { parsing_ok: boolean; open_unknowns: number; orphan_count: number };
  branch: { name: string | null; drift: boolean };
  artifacts: Array<{ path: string; exists: boolean }>;
  recent_activity: Array<{ record_type: string; feature_id: string }>;
}

export type NextAction =
  | { action: 'unblock'; step_id: string; reason: string }
  | { action: 'refresh'; step_id: string }
  | { action: 'recover'; step_id: string }
  | { action: 'run'; step_id: string }
  | { action: 'all_done' };

/** The Atlas landing view — a bento grid of tiles answering where the
 * feature is, what's healthy, what's blocked, and the one next action. */
export class AtlasLanding {
  private root: HTMLElement;

  constructor(root: HTMLElement) {
    this.root = root;
  }

  render(data: AtlasData, onTileClick?: (target: string) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', `Atlas for ${data.feature_id}`);

    const grid = document.createElement('div');
    grid.className = 'atlas-grid';
    grid.style.cssText = 'display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:16px;padding:16px;';

    // Next-action tile (the deterministic recommendation — FR-005).
    grid.appendChild(this.nextActionTile(data.next_action, onTileClick));
    // Progress tile.
    grid.appendChild(this.progressTile(data.progress));
    // Health tile.
    grid.appendChild(this.healthTile(data.health));
    // Branch binding tile.
    grid.appendChild(this.bindingTile(data.branch));
    // Artifacts tile.
    grid.appendChild(this.artifactsTile(data.artifacts, onTileClick));
    // Recent activity tile.
    grid.appendChild(this.activityTile(data.recent_activity));

    this.root.appendChild(grid);
  }

  private tile(label: string): HTMLElement {
    const t = document.createElement('section');
    t.className = 'atlas-tile';
    t.setAttribute('aria-label', label);
    t.style.cssText = 'border:1px solid #ddd;border-radius:8px;padding:16px;background:#fafafa;';
    return t;
  }

  private nextActionTile(next: NextAction, onClick?: (t: string) => void): HTMLElement {
    const tile = this.tile('Next action');
    const desc = describeNextAction(next);
    tile.innerHTML = `<h3 style="margin:0 0 8px;font-size:14px;text-transform:uppercase;color:#666;">Next action</h3>
      <p style="margin:0;font-size:16px;font-weight:600;">${desc}</p>`;
    if (next.action !== 'all_done') {
      const btn = document.createElement('button');
      btn.textContent = 'Go';
      btn.style.cssText = 'margin-top:12px;padding:6px 16px;cursor:pointer;';
      btn.setAttribute('aria-label', `Proceed with ${desc}`);
      btn.addEventListener('click', () => onClick?.(next.action));
      tile.appendChild(btn);
    }
    return tile;
  }

  private progressTile(p: { done_tasks: number; total_tasks: number; ratio: number }): HTMLElement {
    const tile = this.tile('Progress');
    const pct = Math.round(p.ratio * 100);
    tile.innerHTML = `<h3 style="margin:0 0 8px;font-size:14px;text-transform:uppercase;color:#666;">Progress</h3>
      <p style="margin:0;font-size:24px;font-weight:700;">${pct}%</p>
      <p style="margin:4px 0 0;color:#666;">${p.done_tasks}/${p.total_tasks} tasks done</p>`;
    return tile;
  }

  private healthTile(h: { parsing_ok: boolean; open_unknowns: number; orphan_count: number }): HTMLElement {
    const tile = this.tile('Health');
    const icon = h.parsing_ok ? '✓' : '✗';
    const color = h.parsing_ok ? '#16a34a' : '#dc2626';
    tile.innerHTML = `<h3 style="margin:0 0 8px;font-size:14px;text-transform:uppercase;color:#666;">Health</h3>
      <p style="margin:0;"><span style="color:${color};font-weight:700;" aria-hidden="true">${icon}</span> ${h.parsing_ok ? 'Parsing OK' : 'Parse errors'}</p>
      <p style="margin:4px 0 0;color:#666;">${h.open_unknowns} unknowns · ${h.orphan_count} orphans</p>`;
    return tile;
  }

  private bindingTile(b: { name: string | null; drift: boolean }): HTMLElement {
    const tile = this.tile('Branch binding');
    const driftLabel = b.drift ? ' <span style="color:#dc2626;">(drift)</span>' : '';
    tile.innerHTML = `<h3 style="margin:0 0 8px;font-size:14px;text-transform:uppercase;color:#666;">Branch binding</h3>
      <p style="margin:0;font-family:monospace;">${b.name ?? '(unbound)'}${driftLabel}</p>`;
    return tile;
  }

  private artifactsTile(arts: Array<{ path: string; exists: boolean }>, onClick?: (t: string) => void): HTMLElement {
    const tile = this.tile('Artifacts');
    const list = arts
      .map(
        (a) =>
          `<li style="list-style:none;padding:4px 0;"><button data-path="${a.path}" style="background:none;border:none;color:#2563eb;cursor:pointer;text-decoration:underline;font-family:monospace;">${a.path}</button></li>`,
      )
      .join('');
    tile.innerHTML = `<h3 style="margin:0 0 8px;font-size:14px;text-transform:uppercase;color:#666;">Artifacts</h3><ul style="margin:0;padding:0;">${list}</ul>`;
    // Wire artifact clicks.
    tile.querySelectorAll('button[data-path]').forEach((btn) => {
      btn.addEventListener('click', (e) => {
        const path = (e.currentTarget as HTMLElement).getAttribute('data-path') ?? '';
        onClick?.(path);
      });
    });
    return tile;
  }

  private activityTile(activity: Array<{ record_type: string }>): HTMLElement {
    const tile = this.tile('Recent activity');
    if (activity.length === 0) {
      tile.innerHTML = `<h3 style="margin:0 0 8px;font-size:14px;text-transform:uppercase;color:#666;">Recent activity</h3><p style="margin:0;color:#999;">No activity yet.</p>`;
      return tile;
    }
    const list = activity
      .slice(0, 5)
      .map((a) => `<li style="list-style:none;padding:2px 0;font-size:13px;color:#555;">${escapeHtml(a.record_type)}</li>`)
      .join('');
    tile.innerHTML = `<h3 style="margin:0 0 8px;font-size:14px;text-transform:uppercase;color:#666;">Recent activity</h3><ul style="margin:0;padding:0;">${list}</ul>`;
    return tile;
  }
}

function describeNextAction(next: NextAction): string {
  switch (next.action) {
    case 'unblock':
      return `Unblock "${next.step_id}" — ${next.reason}`;
    case 'refresh':
      return `Re-run "${next.step_id}" (stale output)`;
    case 'recover':
      return `Recover "${next.step_id}" (failed)`;
    case 'run':
      return `Run "${next.step_id}"`;
    case 'all_done':
      return 'All steps complete ✓';
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
