// Cross-phase move preview (T058-T059, FR-019).
//
// On a cross-phase drop, shows source/destination/impact/exact-markdown-change
// and a confirm/cancel pair. The move proceeds only after confirmation.
// Also adds a Move menu equivalent for keyboard/AT users (FR-019/037).

export interface MovePreviewData {
  task_id: string;
  source_phase: string;
  destination_phase: string;
  impact: {
    affected_checkpoints: string[];
    dependency_inversions: string[];
    dependency_violations: string[];
  };
  exact_markdown_change: {
    before: string;
    after: string;
  };
}

export class MovePreview {
  constructor(private root: HTMLElement) {}

  render(data: MovePreviewData, onConfirm?: () => void, onCancel?: () => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'dialog');
    this.root.setAttribute('aria-label', `Move ${data.task_id} from ${data.source_phase} to ${data.destination_phase}`);

    const card = document.createElement('div');
    card.style.cssText = 'border:2px solid #d97706;border-radius:8px;padding:16px;background:#fffbeb;max-width:500px;';

    // Header.
    card.innerHTML = `<h3 style="margin:0 0 12px;color:#92400e;">Cross-phase move preview</h3>
      <p style="margin:0 0 8px;"><strong>${escapeHtml(data.task_id)}</strong>: ${escapeHtml(data.source_phase)} → ${escapeHtml(data.destination_phase)}</p>`;

    // Impact summary.
    const impact = document.createElement('div');
    impact.style.cssText = 'margin:12px 0;padding:8px;background:white;border-radius:4px;font-size:13px;';
    if (data.impact.dependency_violations.length > 0) {
      impact.innerHTML += `<p style="color:#dc2626;margin:4px 0;">⚠ Violations: ${data.impact.dependency_violations.map(escapeHtml).join(', ')}</p>`;
    }
    if (data.impact.dependency_inversions.length > 0) {
      impact.innerHTML += `<p style="color:#d97706;margin:4px 0;">⏵ Inversions: ${data.impact.dependency_inversions.map(escapeHtml).join(', ')}</p>`;
    }
    if (data.impact.affected_checkpoints.length > 0) {
      impact.innerHTML += `<p style="color:#666;margin:4px 0;">Checkpoints: ${data.impact.affected_checkpoints.map(escapeHtml).join(', ')}</p>`;
    }
    if (data.impact.dependency_violations.length === 0 && data.impact.dependency_inversions.length === 0) {
      impact.innerHTML += `<p style="color:#16a34a;margin:4px 0;">✓ No dependency issues detected.</p>`;
    }
    card.appendChild(impact);

    // Exact markdown change.
    const md = document.createElement('div');
    md.style.cssText = 'margin:12px 0;font-family:monospace;font-size:12px;';
    md.innerHTML = `<p style="margin:4px 0;color:#dc2626;">- ${escapeHtml(data.exact_markdown_change.before)}</p>
      <p style="margin:4px 0;color:#16a34a;">+ ${escapeHtml(data.exact_markdown_change.after)}</p>`;
    card.appendChild(md);

    // Confirm / Cancel pair — the move proceeds ONLY after confirmation.
    const controls = document.createElement('div');
    controls.style.cssText = 'display:flex;gap:8px;margin-top:12px;';

    const confirm = document.createElement('button');
    confirm.textContent = 'Confirm move';
    confirm.style.cssText = 'padding:8px 16px;background:#d97706;color:white;border:none;border-radius:4px;cursor:pointer;font-weight:600;';
    confirm.setAttribute('aria-label', `Confirm moving ${data.task_id} to ${data.destination_phase}`);
    confirm.addEventListener('click', () => onConfirm?.());

    const cancel = document.createElement('button');
    cancel.textContent = 'Cancel';
    cancel.style.cssText = 'padding:8px 16px;background:#e5e7eb;border:none;border-radius:4px;cursor:pointer;';
    cancel.addEventListener('click', () => onCancel?.());

    controls.appendChild(confirm);
    controls.appendChild(cancel);
    card.appendChild(controls);

    this.root.appendChild(card);
  }
}

/** Move menu — the keyboard/AT alternative to drag (FR-019/037). */
export class MoveMenu {
  constructor(private root: HTMLElement) {}

  render(taskId: string, phases: string[], onMove?: (destination: string) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'menu');
    this.root.setAttribute('aria-label', `Move ${taskId} to phase`);

    const label = document.createElement('p');
    label.textContent = `Move ${taskId} to:`;
    label.style.cssText = 'margin:0 0 4px;font-size:13px;font-weight:600;';
    this.root.appendChild(label);

    phases.forEach((phase) => {
      const btn = document.createElement('button');
      btn.textContent = phase;
      btn.setAttribute('role', 'menuitem');
      btn.style.cssText = 'display:block;width:100%;text-align:left;padding:6px 12px;background:none;border:none;cursor:pointer;font-size:13px;';
      btn.addEventListener('click', () => onMove?.(phase));
      this.root.appendChild(btn);
    });
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
