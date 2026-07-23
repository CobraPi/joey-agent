// Kanban board (Pillar 3): Todo / In Progress / Done columns from Task.status.

import type { SpeckitApiClient, Task } from '../api-client';
import { DependencyView } from './dependency-view';
import { TaskCard } from './task-card';

export interface BoardOptions {
  container: HTMLElement;
  client: SpeckitApiClient;
  featureId: string;
}

const COLUMNS: Array<{ key: Task['status']; label: string }> = [
  { key: 'Todo', label: 'Todo' },
  { key: 'InProgress', label: 'In Progress' },
  { key: 'Completed', label: 'Done' },
];

export class Board {
  private readonly container: HTMLElement;
  private readonly client: SpeckitApiClient;
  private readonly featureId: string;
  private tasks: Task[] = [];
  private cards = new Map<string, TaskCard>();
  private dependencyView!: DependencyView;
  private columnEls = new Map<Task['status'], HTMLElement>();

  constructor(opts: BoardOptions) {
    this.container = opts.container;
    this.client = opts.client;
    this.featureId = opts.featureId;
    this.buildSkeleton();
  }

  private buildSkeleton(): void {
    this.container.innerHTML = '';
    this.container.setAttribute('data-testid', 'board');

    const toolbar = document.createElement('div');
    const toggleBtn = document.createElement('button');
    toggleBtn.textContent = 'Toggle Dependency View';
    toggleBtn.setAttribute('data-testid', 'toggle-dependency-view');
    toggleBtn.addEventListener('click', () => this.dependencyView.toggle(this.tasks));
    toolbar.appendChild(toggleBtn);
    this.container.appendChild(toolbar);

    const depContainer = document.createElement('div');
    this.container.appendChild(depContainer);
    this.dependencyView = new DependencyView({ container: depContainer });

    const columns = document.createElement('div');
    columns.style.display = 'grid';
    columns.style.gridTemplateColumns = `repeat(${COLUMNS.length}, 1fr)`;
    columns.style.gap = '12px';

    for (const col of COLUMNS) {
      const colEl = document.createElement('div');
      colEl.setAttribute('data-testid', `column-${col.key}`);
      const heading = document.createElement('h3');
      heading.textContent = col.label;
      colEl.appendChild(heading);
      const list = document.createElement('div');
      list.className = 'column-list';
      colEl.appendChild(list);
      columns.appendChild(colEl);
      this.columnEls.set(col.key, list);
    }
    this.container.appendChild(columns);
  }

  setTasks(tasks: Task[]): void {
    this.tasks = tasks;
    for (const card of this.cards.values()) card.destroy();
    this.cards.clear();
    for (const list of this.columnEls.values()) list.innerHTML = '';

    for (const task of tasks) {
      // Unparsed tasks are grouped visually in Todo but never dropped.
      const columnKey: Task['status'] =
        task.status === 'Unparsed' ? 'Todo' : task.status;
      const card = new TaskCard({
        client: this.client,
        featureId: this.featureId,
        task,
        onStatusMoved: (taskId, status) => this.moveCard(taskId, status),
      });
      this.cards.set(task.id, card);
      this.columnEls.get(columnKey)?.appendChild(card.el);
    }
  }

  private moveCard(taskId: string, status: 'succeeded' | 'failed'): void {
    const card = this.cards.get(taskId);
    if (!card) return;
    const targetColumn: Task['status'] = status === 'succeeded' ? 'Completed' : 'Todo';
    this.columnEls.get(targetColumn)?.appendChild(card.el);
  }

  /** Returns the count of cards currently in a "running" state — used by
   * tests to assert that clicking one Execute button never starts others. */
  runningCount(): number {
    let count = 0;
    for (const card of this.cards.values()) if (card.isRunning) count++;
    return count;
  }

  destroy(): void {
    for (const card of this.cards.values()) card.destroy();
  }
}
