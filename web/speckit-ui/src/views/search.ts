import type { SpeckitApiClient, Artifact, WorkflowStep, HistoryAttempt } from '../api-client';

interface SearchResult {
  type: 'artifact' | 'requirement' | 'task' | 'step' | 'attempt';
  id: string;
  label: string;
  path?: string;
}

/** Search/filter across feature artifacts, requirement ids, task ids, workflow states, and run history (FR-025). */
export class SearchView {
  private el: HTMLElement;
  private api: SpeckitApiClient;
  private featureId: string;
  private results: SearchResult[] = [];
  private filter: string = '';
  private onSelect: (result: SearchResult) => void;

  constructor(api: SpeckitApiClient, featureId: string, onSelect: (result: SearchResult) => void) {
    this.api = api;
    this.featureId = featureId;
    this.onSelect = onSelect;
    this.el = document.createElement('div');
    this.el.className = 'search-view';
    this.el.setAttribute('role', 'search');
    this.el.setAttribute('aria-label', 'Cross-artifact search');
  }

  get element(): HTMLElement {
    return this.el;
  }

  async load(): Promise<void> {
    try {
      const [artifacts, workflow, history] = await Promise.all([
        this.api.getArtifacts(this.featureId),
        this.api.getWorkflow(this.featureId),
        this.api.getHistory(this.featureId),
      ]);
      this.buildIndex(artifacts, workflow.steps, history.attempts);
      this.render();
    } catch (e) {
      this.el.innerHTML = `<p class="error">Search failed: ${esc(String(e))}</p>`;
    }
  }

  private buildIndex(artifacts: Artifact[], steps: WorkflowStep[], attempts: HistoryAttempt[]): void {
    this.results = [];
    for (const a of artifacts) {
      this.results.push({
        type: 'artifact',
        id: a.path,
        label: a.path.split('/').pop() || a.path,
        path: a.path,
      });
    }
    for (const s of steps) {
      this.results.push({
        type: 'step',
        id: s.id,
        label: `${s.id}: ${s.state}`,
      });
    }
    for (const a of attempts) {
      this.results.push({
        type: 'attempt',
        id: a.attempt_id,
        label: `${a.step_id} — ${a.status} (${a.started_at})`,
      });
    }
  }

  private render(): void {
    this.el.innerHTML = `
      <input type="search" class="search-input" placeholder="Search artifacts, steps, attempts..." aria-label="Search query" autofocus>
      <ul class="search-results" role="listbox" aria-label="Search results"></ul>
    `;

    const input = this.el.querySelector('.search-input') as HTMLInputElement;
    input.value = this.filter;
    input.addEventListener('input', () => {
      this.filter = input.value.toLowerCase();
      this.renderResults();
    });

    this.renderResults();
  }

  private renderResults(): void {
    const ul = this.el.querySelector('.search-results');
    if (!ul) return;

    const filtered = this.filter
      ? this.results.filter(r =>
          r.label.toLowerCase().includes(this.filter) ||
          r.id.toLowerCase().includes(this.filter),
        )
      : this.results;

    // Virtualized: limit to first 100 results for performance (FR-031).
    const visible = filtered.slice(0, 100);

    ul.innerHTML = visible.map(r => {
      const icon = r.type === 'artifact' ? '📄' : r.type === 'step' ? '⚙' : r.type === 'attempt' ? '🏃' : '📌';
      return `<li class="search-result" role="option" tabindex="0" data-id="${esc(r.id)}" data-type="${esc(r.type)}">
        <span class="result-icon">${icon}</span>
        <span class="result-label">${esc(r.label)}</span>
      </li>`;
    }).join('');

    if (filtered.length > 100) {
      ul.insertAdjacentHTML('beforeend', `<li class="search-more">${filtered.length - 100} more results...</li>`);
    }

    ul.querySelectorAll<HTMLElement>('.search-result').forEach(item => {
      item.addEventListener('click', () => {
        const id = item.dataset.id!;
        const type = item.dataset.type as SearchResult['type'];
        const result = this.results.find(r => r.id === id && r.type === type);
        if (result) this.onSelect(result);
      });
      item.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          item.click();
        }
      });
    });
  }
}

function esc(s: string): string {
  const div = document.createElement('div');
  div.textContent = s;
  return div.innerHTML;
}
