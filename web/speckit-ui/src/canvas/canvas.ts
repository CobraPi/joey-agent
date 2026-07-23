// Spec-to-Task mind-map canvas (Pillar 1).
// Renders Specification / UserStory / Plan / Task nodes with parent-child
// connecting lines, color-coded by Status. Plain SVG rendering — no heavy
// graph library, per plan.md constraints.

import type {
  FeatureDetail,
  PatchResponse,
  SpeckitApiClient,
  Status,
  Task,
  UserStory,
} from '../api-client';

export type CanvasNodeType = 'Specification' | 'UserStory' | 'Plan' | 'Task';

export interface CanvasNode {
  id: string;
  type: CanvasNodeType;
  label: string;
  status: Status;
  parentId: string | null;
  x: number;
  y: number;
  /** true if this node represents malformed/unparsed source content that
   * must never be silently dropped from the graph. */
  unparsed: boolean;
}

export interface CanvasEdge {
  from: string;
  to: string;
}

const STATUS_COLOR: Record<Status, string> = {
  Draft: '#9e9e9e',
  InProgress: '#2196f3',
  Completed: '#4caf50',
  Unparsed: '#e91e63',
};

const NODE_W = 160;
const NODE_H = 48;
const COL_GAP = 220;
const ROW_GAP = 70;

export interface EmptyStateAction {
  message: string;
  buttonLabel: string;
  onTrigger: () => void;
}

/** Builds the node/edge graph from a FeatureDetail. Never drops a task or
 * user story, even if malformed — malformed entries are flagged unparsed. */
export function buildGraph(detail: FeatureDetail): { nodes: CanvasNode[]; edges: CanvasEdge[] } {
  const nodes: CanvasNode[] = [];
  const edges: CanvasEdge[] = [];

  const specId = 'spec';
  if (detail.spec) {
    nodes.push({
      id: specId,
      type: 'Specification',
      label: detail.spec.title || 'Specification',
      status: detail.spec.status,
      parentId: null,
      x: 0,
      y: 0,
      unparsed: detail.spec.status === 'Unparsed',
    });

    const stories: UserStory[] = detail.spec.user_stories ?? [];
    stories.forEach((story, i) => {
      const nodeId = `story:${story.id}`;
      nodes.push({
        id: nodeId,
        type: 'UserStory',
        label: story.title || story.id || `User Story ${i + 1}`,
        status: 'Draft',
        parentId: specId,
        x: 1,
        y: i,
        unparsed: !story.id,
      });
      edges.push({ from: specId, to: nodeId });
    });
  }

  if (detail.plan) {
    const planId = 'plan';
    nodes.push({
      id: planId,
      type: 'Plan',
      label: 'Plan',
      status: 'Draft',
      parentId: specId,
      x: 1,
      y: nodes.length,
      unparsed: false,
    });
    if (detail.spec) edges.push({ from: specId, to: planId });
  }

  const tasks: Task[] = detail.tasks ?? [];
  tasks.forEach((task, i) => {
    const nodeId = `task:${task.id ?? `unparsed-${i}`}`;
    const parentStoryId = task.user_story_ref ? `story:${task.user_story_ref}` : specId;
    const malformed = !task.id || !['Todo', 'InProgress', 'Completed', 'Unparsed'].includes(task.status);
    nodes.push({
      id: nodeId,
      type: 'Task',
      label: task.description ? task.description.slice(0, 40) : `Task ${i + 1}`,
      status: malformed ? 'Unparsed' : taskStatusToNodeStatus(task.status),
      parentId: parentStoryId,
      x: 2,
      y: i,
      unparsed: malformed,
    });
    edges.push({ from: parentStoryId, to: nodeId });
  });

  return { nodes, edges };
}

function taskStatusToNodeStatus(status: Task['status']): Status {
  switch (status) {
    case 'Todo':
      return 'Draft';
    case 'InProgress':
      return 'InProgress';
    case 'Completed':
      return 'Completed';
    default:
      return 'Unparsed';
  }
}

/** Lays out nodes into a simple column-based grid by depth (x) and index (y). */
function layout(nodes: CanvasNode[]): CanvasNode[] {
  const columnCounts = new Map<number, number>();
  return nodes.map((n) => {
    const row = columnCounts.get(n.x) ?? 0;
    columnCounts.set(n.x, row + 1);
    return { ...n, x: n.x * COL_GAP + 40, y: row * ROW_GAP + 40 };
  });
}

export interface CanvasOptions {
  container: HTMLElement;
  client: SpeckitApiClient;
  featureId: string;
  onMissingPlan?: () => void;
  onMissingTasks?: () => void;
}

export class SpecTaskCanvas {
  private readonly container: HTMLElement;
  private readonly client: SpeckitApiClient;
  private featureId: string;
  private detail: FeatureDetail | null = null;
  private unwatch: (() => void) | null = null;
  private conflictMessage: string | null = null;
  private readonly onMissingPlan?: () => void;
  private readonly onMissingTasks?: () => void;

  constructor(opts: CanvasOptions) {
    this.container = opts.container;
    this.client = opts.client;
    this.featureId = opts.featureId;
    this.onMissingPlan = opts.onMissingPlan;
    this.onMissingTasks = opts.onMissingTasks;
  }

  async load(): Promise<void> {
    this.detail = await this.client.getFeature(this.featureId);
    this.render();
    this.subscribeToWatch();
  }

  destroy(): void {
    this.unwatch?.();
    this.unwatch = null;
  }

  private subscribeToWatch(): void {
    this.unwatch?.();
    this.unwatch = this.client.watchFeature(this.featureId, () => {
      // Any change to spec.md/plan.md/tasks.md triggers a full refetch and
      // re-render — the simplest correct implementation of FR-005/SC-004.
      void this.refresh();
    });
  }

  async refresh(): Promise<void> {
    this.detail = await this.client.getFeature(this.featureId);
    this.render();
  }

  private render(): void {
    this.container.innerHTML = '';
    if (!this.detail) return;

    const missing = this.detail.missing ?? [];
    if (!this.detail.spec) {
      this.renderEmptyState('No spec.md found for this feature.', 'Create Spec');
      return;
    }
    if (missing.includes('plan') || missing.includes('tasks')) {
      this.renderMissingBanner(missing);
    }

    const { nodes: rawNodes, edges } = buildGraph(this.detail);
    const nodes = layout(rawNodes);

    const width = Math.max(600, (Math.max(...nodes.map((n) => n.x)) || 0) + NODE_W + 80);
    const height = Math.max(400, (Math.max(...nodes.map((n) => n.y)) || 0) + NODE_H + 80);

    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('width', String(width));
    svg.setAttribute('height', String(height));
    svg.setAttribute('data-testid', 'canvas-svg');
    svg.style.background = '#1e1e1e';

    const byId = new Map(nodes.map((n) => [n.id, n]));

    for (const edge of edges) {
      const from = byId.get(edge.from);
      const to = byId.get(edge.to);
      if (!from || !to) continue;
      const line = document.createElementNS('http://www.w3.org/2000/svg', 'line');
      line.setAttribute('x1', String(from.x + NODE_W / 2));
      line.setAttribute('y1', String(from.y + NODE_H / 2));
      line.setAttribute('x2', String(to.x + NODE_W / 2));
      line.setAttribute('y2', String(to.y + NODE_H / 2));
      line.setAttribute('stroke', '#666');
      line.setAttribute('stroke-width', '1.5');
      line.setAttribute('class', 'canvas-edge');
      svg.appendChild(line);
    }

    for (const node of nodes) {
      svg.appendChild(this.renderNode(node));
    }

    this.container.appendChild(svg);

    if (this.conflictMessage) {
      this.container.appendChild(this.renderConflictBanner());
    }
  }

  private renderNode(node: CanvasNode): SVGElement {
    const g = document.createElementNS('http://www.w3.org/2000/svg', 'g');
    g.setAttribute('data-testid', `node-${node.id}`);
    g.setAttribute('data-node-type', node.type);
    g.setAttribute('data-node-status', node.status);
    g.setAttribute('class', `canvas-node${node.unparsed ? ' canvas-node--unparsed' : ''}`);

    const rect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
    rect.setAttribute('x', String(node.x));
    rect.setAttribute('y', String(node.y));
    rect.setAttribute('width', String(NODE_W));
    rect.setAttribute('height', String(NODE_H));
    rect.setAttribute('rx', '6');
    rect.setAttribute('fill', node.unparsed ? STATUS_COLOR.Unparsed : STATUS_COLOR[node.status]);
    rect.setAttribute('stroke', node.unparsed ? '#fff' : 'transparent');
    rect.setAttribute('stroke-dasharray', node.unparsed ? '4,2' : '0');
    g.appendChild(rect);

    const text = document.createElementNS('http://www.w3.org/2000/svg', 'text');
    text.setAttribute('x', String(node.x + 8));
    text.setAttribute('y', String(node.y + NODE_H / 2 + 4));
    text.setAttribute('fill', '#111');
    text.setAttribute('font-size', '11');
    text.textContent = `${node.unparsed ? '⚠ ' : ''}${node.type}: ${node.label}`;
    g.appendChild(text);

    g.addEventListener('dblclick', () => {
      void this.openInlineEditor(node);
    });

    return g;
  }

  private renderEmptyState(message: string, buttonLabel: string): void {
    const wrap = document.createElement('div');
    wrap.className = 'canvas-empty-state';
    wrap.setAttribute('data-testid', 'canvas-empty-state');
    const p = document.createElement('p');
    p.textContent = message;
    const btn = document.createElement('button');
    btn.textContent = buttonLabel;
    btn.setAttribute('data-testid', 'canvas-empty-state-action');
    btn.addEventListener('click', () => this.onMissingPlan?.());
    wrap.appendChild(p);
    wrap.appendChild(btn);
    this.container.appendChild(wrap);
  }

  private renderMissingBanner(missing: Array<'plan' | 'tasks'>): void {
    const banner = document.createElement('div');
    banner.className = 'canvas-missing-banner';
    banner.setAttribute('data-testid', 'canvas-missing-banner');
    for (const m of missing) {
      const row = document.createElement('div');
      row.textContent = `${m}.md not yet created. `;
      const btn = document.createElement('button');
      btn.textContent = m === 'plan' ? 'Run /speckit-plan' : 'Run /speckit-tasks';
      btn.setAttribute('data-testid', `trigger-${m}`);
      btn.addEventListener('click', () => {
        if (m === 'plan') this.onMissingPlan?.();
        else this.onMissingTasks?.();
      });
      row.appendChild(btn);
      banner.appendChild(row);
    }
    this.container.appendChild(banner);
  }

  private renderConflictBanner(): HTMLElement {
    const banner = document.createElement('div');
    banner.className = 'canvas-conflict-banner';
    banner.setAttribute('data-testid', 'conflict-banner');
    banner.style.background = '#b71c1c';
    banner.style.color = '#fff';
    banner.style.padding = '8px';

    const msg = document.createElement('span');
    msg.textContent = this.conflictMessage ?? '';
    banner.appendChild(msg);

    const reload = document.createElement('button');
    reload.textContent = 'Reload';
    reload.setAttribute('data-testid', 'conflict-reload');
    reload.addEventListener('click', () => {
      this.conflictMessage = null;
      void this.refresh();
    });
    banner.appendChild(reload);
    return banner;
  }

  private async openInlineEditor(node: CanvasNode): Promise<void> {
    if (node.type !== 'Task') return; // MVP: task inline edit only, per T027
    const task = this.detail?.tasks.find((t) => `task:${t.id}` === node.id);
    if (!task || !this.detail?.tasks_content_hash) return;

    const newText = window.prompt('Edit task description:', task.description);
    if (newText === null || newText === task.description) return;

    try {
      const result: PatchResponse = await this.client.patchTask(
        this.featureId,
        task.id,
        newText,
        this.detail.tasks_content_hash,
      );
      this.detail.tasks_content_hash = result.content_hash;
      task.description = newText;
      this.conflictMessage = null;
      this.render();
    } catch (err) {
      const anyErr = err as { isConflict?: boolean; message?: string };
      if (anyErr.isConflict) {
        this.conflictMessage =
          anyErr.message ?? 'tasks.md changed on disk. Reload and reapply your edit.';
        this.render();
      } else {
        throw err;
      }
    }
  }
}
