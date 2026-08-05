import type { Artifact } from '../api-client';
import type { SpeckitApiClient } from '../api-client';

/** Feature/artifact navigator grouped by workflow phase (FR-003). */
export class ExplorerView {
  private el: HTMLElement;
  private api: SpeckitApiClient;
  private featureId: string;
  private onSelect: (path: string) => void;
  private artifacts: Artifact[] = [];

  constructor(api: SpeckitApiClient, featureId: string, onSelect: (path: string) => void) {
    this.api = api;
    this.featureId = featureId;
    this.onSelect = onSelect;
    this.el = document.createElement('div');
    this.el.className = 'explorer';
    this.el.setAttribute('role', 'navigation');
    this.el.setAttribute('aria-label', 'Artifact explorer');
  }

  get element(): HTMLElement {
    return this.el;
  }

  async load(): Promise<void> {
    try {
      this.artifacts = await this.api.getArtifacts(this.featureId);
      this.render();
    } catch (e) {
      this.el.innerHTML = `<p class="error">Failed to load artifacts: ${esc(String(e))}</p>`;
    }
  }

  private render(): void {
    const grouped = this.groupByPhase();
    let html = '<ul class="explorer-tree" role="tree">';

    for (const [phase, items] of grouped) {
      html += `<li class="phase-group" role="treeitem" aria-expanded="true">`;
      html += `<span class="phase-label">${esc(phase)}</span>`;
      html += '<ul role="group">';
      for (const art of items) {
        const icon = art.exists ? '📄' : '➕';
        const staleCls = art.stale ? ' stale' : '';
        const invalidCls = art.validity.some(f => f.severity === 'Critical') ? ' invalid' : '';
        html += `<li class="artifact-item${staleCls}${invalidCls}" role="treeitem" tabindex="0" data-path="${esc(art.path)}">`;
        html += `<span class="artifact-icon">${icon}</span>`;
        html += `<span class="artifact-name">${esc(this.shortName(art))}</span>`;
        if (!art.exists) {
          html += '<span class="artifact-badge create">create</span>';
        }
        if (art.stale) {
          html += '<span class="artifact-badge stale">stale</span>';
        }
        html += '</li>';
      }
      html += '</ul></li>';
    }
    html += '</ul>';
    this.el.innerHTML = html;

    this.el.querySelectorAll<HTMLElement>('.artifact-item').forEach(item => {
      item.addEventListener('click', () => {
        const path = item.dataset.path;
        if (path) this.onSelect(path);
      });
      item.addEventListener('keydown', (e) => {
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          const path = item.dataset.path;
          if (path) this.onSelect(path);
        }
      });
    });
  }

  private groupByPhase(): Map<string, Artifact[]> {
    const map = new Map<string, Artifact[]>();
    for (const art of this.artifacts) {
      const phase = art.workflow_phase || 'supporting';
      if (!map.has(phase)) map.set(phase, []);
      map.get(phase)!.push(art);
    }
    return map;
  }

  private shortName(art: Artifact): string {
    const parts = art.path.split('/');
    return parts[parts.length - 1] || art.path;
  }

  markStale(paths: Set<string>): void {
    for (const art of this.artifacts) {
      if (paths.has(art.path)) {
        art.stale = true;
      }
    }
    this.render();
  }
}

function esc(s: string): string {
  const div = document.createElement('div');
  div.textContent = s;
  return div.innerHTML;
}
