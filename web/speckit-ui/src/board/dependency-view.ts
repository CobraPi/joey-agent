// Dependency/timeline view (Pillar 3): toggleable view linking tasks that
// share target_files.

import type { Task } from '../api-client';

export interface DependencyLink {
  fromTaskId: string;
  toTaskId: string;
  sharedFile: string;
}

/** Computes links between tasks that share at least one target file. Only
 * emits one link per unordered pair per shared file (no duplicate reverse
 * links), ordered by the tasks' position in the input array. */
export function computeDependencyLinks(tasks: Task[]): DependencyLink[] {
  const links: DependencyLink[] = [];
  for (let i = 0; i < tasks.length; i++) {
    for (let j = i + 1; j < tasks.length; j++) {
      const a = tasks[i];
      const b = tasks[j];
      const shared = a.target_files.filter((f) => b.target_files.includes(f));
      for (const file of shared) {
        links.push({ fromTaskId: a.id, toTaskId: b.id, sharedFile: file });
      }
    }
  }
  return links;
}

export interface DependencyViewOptions {
  container: HTMLElement;
}

export class DependencyView {
  private readonly container: HTMLElement;
  private visible = false;

  constructor(opts: DependencyViewOptions) {
    this.container = opts.container;
    this.container.setAttribute('data-testid', 'dependency-view');
    this.container.style.display = 'none';
  }

  get isVisible(): boolean {
    return this.visible;
  }

  toggle(tasks: Task[]): void {
    this.visible = !this.visible;
    this.container.style.display = this.visible ? 'block' : 'none';
    if (this.visible) {
      this.render(tasks);
    }
  }

  private render(tasks: Task[]): void {
    this.container.innerHTML = '';
    const links = computeDependencyLinks(tasks);
    if (links.length === 0) {
      const empty = document.createElement('div');
      empty.textContent = 'No shared-file dependencies detected.';
      this.container.appendChild(empty);
      return;
    }
    const list = document.createElement('ul');
    for (const link of links) {
      const li = document.createElement('li');
      li.setAttribute('data-testid', `dependency-link-${link.fromTaskId}-${link.toTaskId}`);
      li.textContent = `${link.fromTaskId} ↔ ${link.toTaskId} (shared: ${link.sharedFile})`;
      list.appendChild(li);
    }
    this.container.appendChild(list);
  }
}
