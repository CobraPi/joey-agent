import type { WorkflowStep, OptionsCatalog } from '../api-client';
import type { SpeckitApiClient } from '../api-client';
import { StatusBadge } from '../components/status-badges';

/** Step list with states, run-configuration panel, project-override management (FR-008/009/010/034). */
export class WorkflowView {
  private el: HTMLElement;
  private api: SpeckitApiClient;
  private featureId: string;
  private steps: WorkflowStep[] = [];
  private options: OptionsCatalog | null = null;
  private onRunStep: (step: WorkflowStep) => void;

  constructor(api: SpeckitApiClient, featureId: string, onRunStep: (step: WorkflowStep) => void) {
    this.api = api;
    this.featureId = featureId;
    this.onRunStep = onRunStep;
    this.el = document.createElement('div');
    this.el.className = 'workflow-view';
    this.el.setAttribute('role', 'region');
    this.el.setAttribute('aria-label', 'Workflow steps');
  }

  get element(): HTMLElement {
    return this.el;
  }

  async load(): Promise<void> {
    try {
      const [wf, opts] = await Promise.all([
        this.api.getWorkflow(this.featureId),
        this.api.getOptions(),
      ]);
      this.steps = wf.steps;
      this.options = opts;
      this.render();
    } catch (e) {
      this.el.innerHTML = `<p class="error">Failed to load workflow: ${esc(String(e))}</p>`;
    }
  }

  private render(): void {
    let html = '<div class="workflow-steps" role="list">';
    for (const step of this.steps) {
      const badge = StatusBadge.forStep(step);
      const canRun = step.state === 'ready' || step.state === 'stale';
      html += `<div class="workflow-step" role="listitem" data-step-id="${esc(step.id)}">`;
      html += `<div class="step-header">`;
      html += `<span class="step-order">${step.order}</span>`;
      html += `<span class="step-name">${esc(step.id)}</span>`;
      html += badge.html;
      html += `</div>`;
      html += `<p class="step-purpose">${esc(step.purpose)}</p>`;
      if (step.blocking_reason) {
        html += `<p class="step-blocked">${esc(step.blocking_reason)}</p>`;
      }
      if (canRun) {
        html += `<button class="run-btn" type="button" data-step-id="${esc(step.id)}" aria-label="Run ${esc(step.id)}">Run</button>`;
      }
      html += `<button class="config-btn" type="button" data-step-id="${esc(step.id)}" aria-label="Configure ${esc(step.id)}">Configure</button>`;
      html += `</div>`;
    }
    html += '</div>';
    html += '<div class="run-config-panel" role="region" aria-label="Run configuration" hidden></div>';
    this.el.innerHTML = html;

    this.el.querySelectorAll<HTMLButtonElement>('.run-btn').forEach(btn => {
      btn.addEventListener('click', () => {
        const stepId = btn.dataset.stepId!;
        const step = this.steps.find(s => s.id === stepId);
        if (step) this.onRunStep(step);
      });
    });

    this.el.querySelectorAll<HTMLButtonElement>('.config-btn').forEach(btn => {
      btn.addEventListener('click', () => this.showConfig(btn.dataset.stepId!));
    });
  }

  private async showConfig(stepId: string): Promise<void> {
    const panel = this.el.querySelector('.run-config-panel') as HTMLElement;
    if (!panel) return;
    panel.hidden = false;
    panel.innerHTML = '<p>Loading configuration...</p>';

    try {
      const config = await this.api.getStepConfig(this.featureId, stepId);
      let html = `<h3>Configuration: ${esc(stepId)}</h3>`;
      html += '<div class="config-section">';
      html += '<h4>Effective Instructions</h4>';
      html += `<textarea class="config-instructions" rows="6">${esc(config.effective_instructions)}</textarea>`;
      html += '</div>';

      if (config.override) {
        html += `<div class="config-override"><p>Override: ${esc(config.override.override_id)}</p>`;
        html += `<button class="remove-override-btn" type="button" data-step-id="${esc(stepId)}">Remove Override</button></div>`;
      } else {
        html += `<button class="save-override-btn" type="button" data-step-id="${esc(stepId)}">Save as Project Override</button>`;
      }

      if (this.options) {
        html += '<div class="config-options">';
        html += '<h4>Agent Options</h4>';
        html += `<label>Model: <select class="opt-model">`;
        for (const m of this.options.models) {
          html += `<option value="${esc(m)}">${esc(m)}</option>`;
        }
        html += '</select></label>';
        html += `<label>Reasoning: <select class="opt-reasoning">`;
        for (const r of this.options.reasoning_efforts) {
          html += `<option value="${esc(r)}">${esc(r)}</option>`;
        }
        html += '</select></label>';
        html += `<label>Max Iterations: <input type="number" class="opt-max-iter" min="${this.options.max_iterations.min}" max="${this.options.max_iterations.max}" value="${this.options.max_iterations.default}"></label>`;
        html += '</div>';
      }

      html += '<div class="config-change-mode">';
      html += '<label>Change Mode: <select class="opt-change-mode" required>';
      html += '<option value="">Select...</option>';
      html += '<option value="staged">Staged (review before apply)</option>';
      html += '<option value="direct">Direct (write live)</option>';
      html += '</select></label>';
      html += '</div>';

      panel.innerHTML = html;

      const saveBtn = panel.querySelector('.save-override-btn');
      if (saveBtn) {
        saveBtn.addEventListener('click', async () => {
          const text = (panel.querySelector('.config-instructions') as HTMLTextAreaElement).value;
          await this.api.putOverride(this.featureId, stepId, text);
          this.showConfig(stepId);
        });
      }

      const removeBtn = panel.querySelector('.remove-override-btn');
      if (removeBtn) {
        removeBtn.addEventListener('click', async () => {
          await this.api.deleteOverride(this.featureId, stepId);
          this.showConfig(stepId);
        });
      }
    } catch (e) {
      panel.innerHTML = `<p class="error">Failed: ${esc(String(e))}</p>`;
    }
  }

  getOptionsCatalogRevision(): string | null {
    return this.options?.revision ?? null;
  }
}

function esc(s: string): string {
  const div = document.createElement('div');
  div.textContent = s;
  return div.innerHTML;
}
