import type { WorkflowStep, HistoryAttempt } from '../api-client';
import type { SpeckitApiClient } from '../api-client';
import { StatusBadge } from '../components/status-badges';

/** Lifecycle/readiness summary, stale propagation display, end-to-end progress trace (FR-021/022/032). */
export class ReadinessView {
  private el: HTMLElement;
  private api: SpeckitApiClient;
  private featureId: string;

  constructor(api: SpeckitApiClient, featureId: string) {
    this.api = api;
    this.featureId = featureId;
    this.el = document.createElement('div');
    this.el.className = 'readiness-view';
    this.el.setAttribute('role', 'region');
    this.el.setAttribute('aria-label', 'Readiness overview');
  }

  get element(): HTMLElement {
    return this.el;
  }

  async load(): Promise<void> {
    try {
      const [wf, history] = await Promise.all([
        this.api.getWorkflow(this.featureId),
        this.api.getHistory(this.featureId),
      ]);
      this.render(wf.steps, history.attempts);
    } catch (e) {
      this.el.innerHTML = `<p class="error">Failed to load readiness: ${esc(String(e))}</p>`;
    }
  }

  private render(steps: WorkflowStep[], attempts: HistoryAttempt[]): void {
    const ready = steps.filter(s => s.state === 'ready').length;
    const blocked = steps.filter(s => s.state === 'blocked').length;
    const succeeded = steps.filter(s => s.state === 'succeeded').length;
    const stale = steps.filter(s => s.state === 'stale').length;
    const failed = steps.filter(s => s.state === 'failed').length;

    let html = '<div class="readiness-summary">';

    html += '<div class="readiness-stats">';
    html += `<div class="stat ready"><span class="stat-num">${ready}</span><span class="stat-label">Ready</span></div>`;
    html += `<div class="stat blocked"><span class="stat-num">${blocked}</span><span class="stat-label">Blocked</span></div>`;
    html += `<div class="stat succeeded"><span class="stat-num">${succeeded}</span><span class="stat-label">Succeeded</span></div>`;
    html += `<div class="stat stale"><span class="stat-num">${stale}</span><span class="stat-label">Stale</span></div>`;
    html += `<div class="stat failed"><span class="stat-num">${failed}</span><span class="stat-label">Failed</span></div>`;
    html += '</div>';

    html += '<div class="readiness-steps" role="list">';
    for (const step of steps) {
      const badge = StatusBadge.forStep(step);
      const nextAction = this.nextActionFor(step);
      html += `<div class="readiness-step" role="listitem">`;
      html += `<span class="step-id">${esc(step.id)}</span>`;
      html += badge.html;
      if (nextAction) {
        html += `<span class="step-next-action">${esc(nextAction)}</span>`;
      }
      html += `</div>`;
    }
    html += '</div>';

    if (attempts.length > 0) {
      html += '<div class="readiness-history">';
      html += '<h4>Recent Attempts</h4>';
      html += '<ul class="history-list" role="list">';
      for (const a of attempts.slice(0, 10)) {
        const badge = StatusBadge.forAttempt(a.status);
        html += `<li class="history-item" role="listitem">`;
        html += `<span class="history-step">${esc(a.step_id)}</span>`;
        html += badge.html;
        html += `<span class="history-time">${esc(a.started_at)}</span>`;
        html += `</li>`;
      }
      html += '</ul>';
      html += '</div>';
    }

    html += '</div>';
    this.el.innerHTML = html;
  }

  private nextActionFor(step: WorkflowStep): string | null {
    switch (step.state) {
      case 'ready':
        return `Ready to run ${step.id}`;
      case 'blocked':
        return step.blocking_reason || 'Resolve blocking prerequisites';
      case 'stale':
        return 'An upstream artifact changed — re-run to update';
      case 'failed':
        return 'Review the failure and recover or re-run';
      case 'unavailable':
        return 'Install the required skill to enable this step';
      default:
        return null;
    }
  }
}

function esc(s: string): string {
  const div = document.createElement('div');
  div.textContent = s;
  return div.innerHTML;
}
