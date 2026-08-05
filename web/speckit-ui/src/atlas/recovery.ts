// Recovery states (T035, FR-002).
//
// Renders each empty/failed/disconnected state with exactly one primary
// recovery action. No stack traces, no bare "command failed".

export interface RecoveryData {
  feature_id: string;
  recovery_states: Array<{
    state: string;
    description: string;
    primary_action: string;
    step_id?: string;
    touches_files: string[];
  }>;
}

const ACTION_LABELS: Record<string, string> = {
  create_spec: 'Create spec.md',
  recover_step: 'Recover step',
  install_agent: 'Install agent',
};

/** Renders recovery cards — each with exactly one primary action. */
export class RecoveryView {
  private root: HTMLElement;

  constructor(root: HTMLElement) {
    this.root = root;
  }

  render(data: RecoveryData, onAction?: (action: string, payload?: unknown) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', 'Recovery actions');

    if (data.recovery_states.length === 0) {
      const ok = document.createElement('p');
      ok.textContent = 'No recovery needed — everything is healthy.';
      ok.style.cssText = 'color:#16a34a;padding:16px;';
      this.root.appendChild(ok);
      return;
    }

    data.recovery_states.forEach((rs) => {
      const card = document.createElement('section');
      card.className = 'recovery-card';
      card.setAttribute('aria-label', rs.description);
      card.style.cssText = 'border:1px solid #f59e0b;border-radius:8px;padding:16px;margin:8px 0;background:#fffbeb;';

      const desc = document.createElement('p');
      desc.textContent = rs.description;
      desc.style.cssText = 'margin:0 0 12px;font-weight:600;color:#92400e;';
      card.appendChild(desc);

      const actionLabel = ACTION_LABELS[rs.primary_action] ?? rs.primary_action;
      const btn = document.createElement('button');
      btn.textContent = actionLabel;
      btn.style.cssText = 'padding:8px 16px;background:#d97706;color:white;border:none;border-radius:4px;cursor:pointer;font-weight:600;';
      btn.setAttribute('aria-label', `${actionLabel} — the primary recovery action`);
      btn.addEventListener('click', () => onAction?.(rs.primary_action, rs));
      card.appendChild(btn);

      if (rs.touches_files.length > 0) {
        const files = document.createElement('p');
        files.textContent = `Touches: ${rs.touches_files.join(', ')}`;
        files.style.cssText = 'margin:8px 0 0;font-size:12px;color:#92400e;font-family:monospace;';
        card.appendChild(files);
      }

      this.root.appendChild(card);
    });
  }
}
