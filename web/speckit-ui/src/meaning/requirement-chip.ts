// Requirement chip widget (T039, FR-009/022).
// Renders Requirement with modality-driven color + derived coverage chip.

export interface RequirementChipData {
  id: string;
  modality: 'Must' | 'Should' | 'May' | 'MustNot' | 'Unparsed';
  text: string;
  coverage: { task_count: number };
}

const MODALITY_COLORS: Record<string, string> = {
  Must: '#dc2626',
  MustNot: '#dc2626',
  Should: '#d97706',
  May: '#16a34a',
  Unparsed: '#9ca3af',
};

const MODALITY_LABELS: Record<string, string> = {
  Must: 'MUST',
  MustNot: 'MUST NOT',
  Should: 'SHOULD',
  May: 'MAY',
  Unparsed: '?',
};

export class RequirementChip {
  constructor(private root: HTMLElement) {}

  render(data: RequirementChipData): void {
    this.root.innerHTML = '';
    const color = MODALITY_COLORS[data.modality] ?? '#999';
    const modality = MODALITY_LABELS[data.modality] ?? data.modality;

    const chip = document.createElement('span');
    chip.setAttribute('role', 'listitem');
    chip.setAttribute('aria-label', `${data.id} (${modality}): ${data.text}. Coverage: ${data.coverage.task_count} tasks.`);
    chip.style.cssText = `display:inline-flex;align-items:center;gap:6px;padding:4px 10px;margin:2px;border-radius:12px;background:#f3f4f6;font-size:13px;`;

    const mod = document.createElement('span');
    mod.textContent = modality;
    mod.style.cssText = `color:${color};font-weight:700;font-size:11px;`;
    chip.appendChild(mod);

    const id = document.createElement('span');
    id.textContent = data.id;
    id.style.cssText = 'font-family:monospace;font-weight:600;';
    chip.appendChild(id);

    const text = document.createElement('span');
    text.textContent = data.text.length > 60 ? data.text.slice(0, 57) + '…' : data.text;
    text.style.cssText = 'color:#555;';
    chip.appendChild(text);

    // Coverage chip (derived, color + icon + text — never color alone).
    const coverage = document.createElement('span');
    const covColor = data.coverage.task_count > 0 ? '#16a34a' : '#dc2626';
    const covIcon = data.coverage.task_count > 0 ? '✓' : '⚠';
    coverage.style.cssText = `color:${covColor};font-size:11px;font-weight:600;`;
    coverage.textContent = `${covIcon} ${data.coverage.task_count} task(s)`;
    chip.appendChild(coverage);

    this.root.appendChild(chip);
  }
}
