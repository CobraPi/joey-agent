// Story card widget (T038, FR-009).
// Renders UserStory + nested AcceptanceScenario (Given/When/Then flow) with
// priority color and move controls. Edits compile to PatchOp and POST to /patch.

export interface StoryCardData {
  id: string;
  title: string;
  priority: 'P1' | 'P2' | 'P3' | 'Unparsed';
  acceptance_scenarios: Array<{ given: string; when: string; then: string }>;
}

const PRIORITY_COLORS: Record<string, string> = {
  P1: '#dc2626',
  P2: '#d97706',
  P3: '#2563eb',
  Unparsed: '#9ca3af',
};

export class StoryCard {
  constructor(private root: HTMLElement) {}

  render(data: StoryCardData, onEdit?: (op: import('../api-client').PatchOp[]) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'article');
    this.root.setAttribute('aria-label', `User story ${data.id}: ${data.title}`);

    const card = document.createElement('div');
    const color = PRIORITY_COLORS[data.priority] ?? '#999';
    card.style.cssText = `border-left:4px solid ${color};padding:12px;margin:8px 0;border-radius:0 4px 4px 0;background:white;`;

    const header = document.createElement('div');
    header.style.cssText = 'display:flex;justify-content:space-between;align-items:center;';
    header.innerHTML = `<h4 style="margin:0;font-size:14px;"><span style="color:${color};font-weight:700;">${data.priority}</span> ${escapeHtml(data.id)}: ${escapeHtml(data.title)}</h4>`;

    const editBtn = document.createElement('button');
    editBtn.textContent = '✎ Edit';
    editBtn.style.cssText = 'background:none;border:1px solid #ddd;border-radius:4px;padding:4px 8px;cursor:pointer;font-size:12px;';
    editBtn.setAttribute('aria-label', `Edit story ${data.id}`);
    editBtn.addEventListener('click', () => this.openEditForm(data, onEdit));
    header.appendChild(editBtn);
    card.appendChild(header);

    // Acceptance scenarios.
    data.acceptance_scenarios.forEach((s, i) => {
      const flow = document.createElement('div');
      flow.style.cssText = 'padding:8px;background:#f9fafb;margin-top:8px;border-radius:4px;font-size:13px;';
      flow.innerHTML = `<p style="margin:0 0 4px;color:#666;">Scenario ${i + 1}</p>
        <p style="margin:2px 0;"><strong>Given</strong> ${escapeHtml(s.given)}</p>
        <p style="margin:2px 0;"><strong>When</strong> ${escapeHtml(s.when)}</p>
        <p style="margin:2px 0;"><strong>Then</strong> ${escapeHtml(s.then)}</p>`;
      card.appendChild(flow);
    });

    this.root.appendChild(card);
  }

  private openEditForm(
    data: StoryCardData,
    onEdit?: (op: import('../api-client').PatchOp[]) => void,
  ): void {
    // Simple inline edit of the title — compiles to a Replace op.
    const input = document.createElement('input');
    input.type = 'text';
    input.value = data.title;
    input.style.cssText = 'width:100%;padding:4px;margin:4px 0;';
    input.setAttribute('aria-label', `Edit title for ${data.id}`);

    const save = document.createElement('button');
    save.textContent = 'Save';
    save.style.cssText = 'padding:4px 12px;background:#2563eb;color:white;border:none;border-radius:4px;cursor:pointer;';
    save.addEventListener('click', () => {
      onEdit?.([{ op: 'replace', node: 0, new_bytes: input.value }]);
    });

    this.root.innerHTML = '';
    this.root.appendChild(input);
    this.root.appendChild(save);
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
