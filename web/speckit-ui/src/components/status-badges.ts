import type { WorkflowStep, StepState } from '../api-client';

/** Status badges for step/attempt states with descriptive aria-labels (FR-027, spec US2 note). */
export class StatusBadge {
  constructor(
    public readonly state: StepState,
    public readonly label: string,
  ) {}

  get html(): string {
    const cls = this.state.replace(/_/g, '-');
    return `<span class="status-badge status-${cls}" role="status" aria-label="${esc(this.label)}">${esc(this.state.replace(/_/g, ' '))}</span>`;
  }

  static forStep(step: WorkflowStep): StatusBadge {
    let label: string = step.state;
    if (step.state === 'attention_needed') {
      label = `attention needed: ${step.blocking_reason || 'action required'}`;
    } else if (step.state === 'blocked' && step.blocking_reason) {
      label = `blocked: ${step.blocking_reason}`;
    } else if (step.state === 'unavailable') {
      label = 'unavailable: skill not installed';
    }
    return new StatusBadge(step.state, label);
  }

  static forAttempt(status: string): StatusBadge {
    return new StatusBadge(
      status as StepState,
      `attempt ${status}`,
    );
  }
}

function esc(s: string): string {
  const div = document.createElement('div');
  div.textContent = s;
  return div.innerHTML;
}
