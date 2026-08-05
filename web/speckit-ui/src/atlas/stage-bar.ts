// Stage bar (T034, FR-006/007/008).
//
// The compact five-stage header: Define → Design → Break down → Build →
// Review. States computed from the backend; never guessed. Expandable to
// show Spec Kit command detail + gate cards.

export interface StageBarData {
  feature_id: string;
  stages: Array<{
    name: string;
    state: 'pending' | 'ready' | 'active' | 'done' | 'blocked';
    gate_reason: string | null;
    step_ids: string[];
  }>;
}

const STAGE_LABELS: Record<string, string> = {
  define: 'Define',
  design: 'Design',
  break_down: 'Break down',
  build: 'Build',
  review: 'Review',
};

const STATE_COLORS: Record<string, string> = {
  pending: '#9ca3af',
  ready: '#2563eb',
  active: '#d97706',
  done: '#16a34a',
  blocked: '#dc2626',
};

const STATE_ICONS: Record<string, string> = {
  pending: '○',
  ready: '●',
  active: '◐',
  done: '✓',
  blocked: '✗',
};

/** The five-stage indicator with expandable gate cards. */
export class StageBar {
  private root: HTMLElement;

  constructor(root: HTMLElement) {
    this.root = root;
  }

  render(data: StageBarData, onStageClick?: (stageName: string) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'navigation');
    this.root.setAttribute('aria-label', 'Lifecycle stages');

    const bar = document.createElement('div');
    bar.className = 'stage-bar';
    bar.style.cssText = 'display:flex;align-items:center;gap:0;padding:8px 16px;border-bottom:1px solid #ddd;';

    data.stages.forEach((stage, idx) => {
      if (idx > 0) {
        const arrow = document.createElement('span');
        arrow.textContent = '→';
        arrow.style.cssText = 'color:#ccc;margin:0 8px;';
        arrow.setAttribute('aria-hidden', 'true');
        bar.appendChild(arrow);
      }
      bar.appendChild(this.stageChip(stage, onStageClick));
    });

    this.root.appendChild(bar);
  }

  private stageChip(
    stage: StageBarData['stages'][number],
    onClick?: (s: string) => void,
  ): HTMLElement {
    const chip = document.createElement('button');
    chip.className = `stage-chip stage-${stage.state}`;
    const color = STATE_COLORS[stage.state] ?? '#999';
    const icon = STATE_ICONS[stage.state] ?? '?';
    chip.style.cssText = `display:inline-flex;align-items:center;gap:6px;padding:6px 12px;border:1px solid ${color};border-radius:16px;background:white;cursor:pointer;font-size:13px;color:${color};`;
    chip.setAttribute('aria-label', `${STAGE_LABELS[stage.name] ?? stage.name}: ${stage.state}${stage.gate_reason ? '. ' + stage.gate_reason : ''}`);

    const iconSpan = document.createElement('span');
    iconSpan.textContent = icon;
    iconSpan.setAttribute('aria-hidden', 'true');
    chip.appendChild(iconSpan);

    const label = document.createElement('span');
    label.textContent = STAGE_LABELS[stage.name] ?? stage.name;
    chip.appendChild(label);

    chip.addEventListener('click', () => onClick?.(stage.name));

    // Gate tooltip on hover/focus.
    if (stage.gate_reason) {
      chip.title = stage.gate_reason;
    }

    return chip;
  }
}
