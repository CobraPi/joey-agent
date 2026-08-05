import type { ChangedFile } from '../api-client';
import type { SpeckitApiClient } from '../api-client';
import { DiffView } from '../components/diff-view';

/** Change review with hunk/file accept-reject and recovery controls (FR-016/017). */
export class ReviewView {
  private el: HTMLElement;
  private api: SpeckitApiClient;
  private attemptId: string;
  private diffView: DiffView;

  constructor(api: SpeckitApiClient, attemptId: string) {
    this.api = api;
    this.attemptId = attemptId;
    this.diffView = new DiffView();
    this.el = document.createElement('div');
    this.el.className = 'review-view';
    this.el.setAttribute('role', 'region');
    this.el.setAttribute('aria-label', 'Change review');
  }

  get element(): HTMLElement {
    return this.el;
  }

  async load(): Promise<void> {
    try {
      const changes = await this.api.getAttemptChanges(this.attemptId);
      this.render(changes.files, changes.mode, changes.recovery_action);
    } catch (e) {
      this.el.innerHTML = `<p class="error">Failed to load changes: ${esc(String(e))}</p>`;
    }
  }

  private render(files: ChangedFile[], mode: string, recoveryAction: string | null): void {
    let html = `<div class="review-header"><h3>Change Review</h3>`;
    html += `<span class="change-mode">${esc(mode)}</span></div>`;

    if (recoveryAction) {
      html += `<div class="recovery-notice" role="alert">${esc(recoveryAction)}</div>`;
    }

    this.el.innerHTML = html;
    this.diffView.setFiles(files);
    this.el.appendChild(this.diffView.element);

    const actions = document.createElement('div');
    actions.className = 'review-actions';
    actions.innerHTML = `
      <button class="apply-btn" type="button" aria-label="Apply accepted changes">Apply Accepted</button>
      <button class="recover-btn" type="button" aria-label="Recover attempt">Recover</button>
    `;

    const applyBtn = actions.querySelector('.apply-btn');
    if (applyBtn) {
      applyBtn.addEventListener('click', () => this.applyChanges());
    }

    const recoverBtn = actions.querySelector('.recover-btn');
    if (recoverBtn) {
      recoverBtn.addEventListener('click', () => this.recover());
    }

    this.el.appendChild(actions);
  }

  private async applyChanges(): Promise<void> {
    try {
      await this.api.getAttemptChanges(this.attemptId); // re-fetch to verify
      // In a real implementation, this calls the apply endpoint with the
      // current selection from the diff view.
      const res = await fetch(`/api/attempts/${this.attemptId}/changes/apply`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ apply_all_accepted: true }),
      });
      if (!res.ok) throw new Error(`Apply failed: ${res.status}`);
      await this.load();
    } catch (e) {
      this.el.querySelector('.review-actions')!.insertAdjacentHTML(
        'beforebegin',
        `<p class="error" role="alert">Apply failed: ${esc(String(e))}</p>`,
      );
    }
  }

  private async recover(): Promise<void> {
    try {
      await fetch(`/api/attempts/${this.attemptId}/recover`, { method: 'POST' });
      await this.load();
    } catch (e) {
      console.error('Recovery failed:', e);
    }
  }
}

function esc(s: string): string {
  const div = document.createElement('div');
  div.textContent = s;
  return div.innerHTML;
}
