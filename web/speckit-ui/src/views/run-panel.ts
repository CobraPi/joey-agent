import type { RunnerEvent } from '../api-client';
import type { SpeckitApiClient } from '../api-client';

/** Streamed progress/tool/question/approval/output events over WS (FR-012/013/014). */
export class RunPanelView {
  private el: HTMLElement;
  private api: SpeckitApiClient;
  private attemptId: string | null = null;
  private unsubscribe: (() => void) | null = null;
  private events: RunnerEvent[] = [];

  constructor(api: SpeckitApiClient) {
    this.api = api;
    this.el = document.createElement('div');
    this.el.className = 'run-panel';
    this.el.setAttribute('role', 'region');
    this.el.setAttribute('aria-label', 'Run progress');
  }

  get element(): HTMLElement {
    return this.el;
  }

  /** Start streaming events for an attempt. */
  watch(attemptId: string): void {
    this.attemptId = attemptId;
    this.events = [];
    if (this.unsubscribe) this.unsubscribe();

    this.unsubscribe = this.api.watchAttempt(attemptId, (evt) => {
      this.events.push(evt);
      this.renderEvent(evt);
    });

    this.render();
  }

  stop(): void {
    if (this.unsubscribe) {
      this.unsubscribe();
      this.unsubscribe = null;
    }
  }

  private render(): void {
    this.el.innerHTML = `
      <div class="run-header">
        <h3>Run Output</h3>
        <button class="cancel-btn" type="button" aria-label="Cancel run">Cancel</button>
      </div>
      <div class="run-events" role="log" aria-live="polite" aria-label="Run events"></div>
      <div class="run-interaction" role="region" aria-label="Interaction prompt" hidden></div>
    `;

    const cancelBtn = this.el.querySelector('.cancel-btn');
    if (cancelBtn) {
      cancelBtn.addEventListener('click', () => {
        if (this.attemptId) {
          this.api.cancelAttempt(this.attemptId);
        }
      });
    }

    // Replay events.
    for (const evt of this.events) {
      this.renderEvent(evt);
    }
  }

  private renderEvent(evt: RunnerEvent): void {
    const container = this.el.querySelector('.run-events');
    if (!container) return;

    const div = document.createElement('div');
    div.className = `run-event run-event-${evt.type}`;

    switch (evt.type) {
      case 'progress':
        div.textContent = evt.text;
        break;
      case 'tool':
        div.innerHTML = `<strong>${esc(evt.name)}</strong>: ${esc(evt.summary)}`;
        break;
      case 'output':
        div.textContent = `${evt.file}: +${evt.added} -${evt.removed}`;
        break;
      case 'status':
        div.innerHTML = `<strong class="terminal-status ${esc(evt.terminal)}">${esc(evt.terminal)}</strong> (${evt.duration_ms}ms)`;
        div.setAttribute('role', 'status');
        break;
      case 'error':
        div.innerHTML = `<span class="error" role="alert">Error: ${esc(evt.message)}</span>`;
        break;
      case 'question':
        this.showInteraction(evt.interaction_id, evt.prompt, 'answer');
        return;
      case 'approval':
        this.showInteraction(evt.interaction_id, `${evt.impact} (${evt.boundary})`, 'approval');
        return;
    }

    container.appendChild(div);
    container.scrollTop = container.scrollHeight;
  }

  private showInteraction(interactionId: string, prompt: string, kind: 'answer' | 'approval'): void {
    const el = this.el.querySelector('.run-interaction') as HTMLElement;
    if (!el) return;
    el.hidden = false;

    if (kind === 'answer') {
      el.innerHTML = `
        <div class="interaction-prompt" role="alert">
          <p>${esc(prompt)}</p>
          <input type="text" class="interaction-answer" placeholder="Your answer..." aria-label="Answer">
          <button class="interaction-submit" type="button">Submit</button>
        </div>
      `;
      const submit = el.querySelector('.interaction-submit');
      const input = el.querySelector('.interaction-answer') as HTMLInputElement;
      if (submit && input && this.attemptId) {
        submit.addEventListener('click', async () => {
          await this.api.answerAttempt(this.attemptId!, interactionId, input.value);
          el.hidden = true;
        });
        input.addEventListener('keydown', (e) => {
          if (e.key === 'Enter') submit.dispatchEvent(new Event('click'));
        });
      }
    } else {
      el.innerHTML = `
        <div class="interaction-prompt" role="alert">
          <p>${esc(prompt)}</p>
          <button class="approve-btn" type="button">Approve</button>
          <button class="reject-btn" type="button">Reject</button>
        </div>
      `;
      const approve = el.querySelector('.approve-btn');
      const reject = el.querySelector('.reject-btn');
      if (approve && this.attemptId) {
        approve.addEventListener('click', async () => {
          await this.api.approveAttempt(this.attemptId!, interactionId, 'approve');
          el.hidden = true;
        });
      }
      if (reject && this.attemptId) {
        reject.addEventListener('click', async () => {
          await this.api.approveAttempt(this.attemptId!, interactionId, 'reject');
          el.hidden = true;
        });
      }
    }
  }
}

function esc(s: string): string {
  const div = document.createElement('div');
  div.textContent = s;
  return div.innerHTML;
}
