// Task card (Kanban board, Pillar 3): shows user story, Parallel badge,
// target_files, and an Execute Tasks button that runs exactly one task.

import type { RunEvent, SpeckitApiClient, Task } from '../api-client';

export interface TaskCardOptions {
  client: SpeckitApiClient;
  featureId: string;
  task: Task;
  onStatusMoved?: (taskId: string, status: 'succeeded' | 'failed') => void;
}

export class TaskCard {
  readonly el: HTMLElement;
  private readonly client: SpeckitApiClient;
  private readonly featureId: string;
  private task: Task;
  private readonly onStatusMoved?: (taskId: string, status: 'succeeded' | 'failed') => void;
  private running = false;
  private unwatch: (() => void) | null = null;
  private outputEl!: HTMLElement;
  private executeBtn!: HTMLButtonElement;
  private outputText = '';

  constructor(opts: TaskCardOptions) {
    this.client = opts.client;
    this.featureId = opts.featureId;
    this.task = opts.task;
    this.onStatusMoved = opts.onStatusMoved;
    this.el = document.createElement('div');
    this.el.className = 'task-card';
    this.el.setAttribute('data-testid', `task-card-${this.task.id}`);
    this.render();
  }

  get taskId(): string {
    return this.task.id;
  }

  get isRunning(): boolean {
    return this.running;
  }

  private render(): void {
    this.el.innerHTML = '';
    this.el.style.border = '1px solid #444';
    this.el.style.borderRadius = '6px';
    this.el.style.padding = '8px';
    this.el.style.marginBottom = '8px';
    this.el.style.background = '#2a2a2a';

    const title = document.createElement('div');
    title.textContent = `${this.task.id}: ${this.task.description}`;
    title.style.fontWeight = 'bold';
    this.el.appendChild(title);

    if (this.task.user_story_ref) {
      const story = document.createElement('div');
      story.className = 'task-card-story';
      story.textContent = `Story: ${this.task.user_story_ref}`;
      this.el.appendChild(story);
    }

    if (this.task.parallel_eligible) {
      const badge = document.createElement('span');
      badge.className = 'badge-parallel';
      badge.setAttribute('data-testid', `badge-parallel-${this.task.id}`);
      badge.textContent = 'Parallel';
      badge.style.background = '#6a1b9a';
      badge.style.color = '#fff';
      badge.style.borderRadius = '4px';
      badge.style.padding = '2px 6px';
      badge.style.fontSize = '11px';
      badge.style.marginRight = '6px';
      this.el.appendChild(badge);
    }

    if (this.task.target_files.length > 0) {
      const files = document.createElement('div');
      files.className = 'task-card-files';
      files.setAttribute('data-testid', `target-files-${this.task.id}`);
      files.textContent = `Files: ${this.task.target_files.join(', ')}`;
      files.style.fontSize = '11px';
      files.style.color = '#aaa';
      this.el.appendChild(files);
    }

    this.executeBtn = document.createElement('button');
    this.executeBtn.textContent = this.running ? 'Running…' : 'Execute Task';
    this.executeBtn.setAttribute('data-testid', `execute-task-${this.task.id}`);
    this.executeBtn.disabled = this.running || this.task.status === 'Completed';
    this.executeBtn.addEventListener('click', () => void this.execute());
    this.el.appendChild(this.executeBtn);

    this.outputEl = document.createElement('pre');
    this.outputEl.className = 'task-card-output';
    this.outputEl.setAttribute('data-testid', `task-output-${this.task.id}`);
    this.outputEl.style.fontSize = '10px';
    this.outputEl.style.maxHeight = '120px';
    this.outputEl.style.overflow = 'auto';
    this.outputEl.textContent = this.outputText;
    this.el.appendChild(this.outputEl);
  }

  /** Executes ONLY this task. Must never be invoked for any task other than
   * the one this card represents — single-task-per-click is a load-bearing
   * product constraint (spec.md Clarifications Q3). */
  async execute(): Promise<void> {
    if (this.running) return;
    this.running = true;
    this.executeBtn.disabled = true;
    this.executeBtn.textContent = 'Running…';

    const { run_id } = await this.client.executeTask(this.featureId, this.task.id);
    this.unwatch?.();
    this.unwatch = this.client.watchRun(run_id, (evt: RunEvent) => {
      if (evt.type === 'output') {
        this.outputText += evt.text + '\n';
        this.outputEl.textContent = this.outputText;
        this.outputEl.scrollTop = this.outputEl.scrollHeight;
      } else if (evt.type === 'status') {
        this.running = false;
        this.task.status = evt.status === 'succeeded' ? 'Completed' : 'Todo';
        this.render();
        this.onStatusMoved?.(this.task.id, evt.status);
        this.unwatch?.();
        this.unwatch = null;
      }
    });
  }

  updateTask(task: Task): void {
    this.task = task;
    this.render();
  }

  destroy(): void {
    this.unwatch?.();
    this.unwatch = null;
  }
}
