// Cycle detection for the dependency graph (T104, FR-020).
//
// Extends the dependency view to detect and visually distinguish cycles in
// the task dependency graph.

export interface DependencyNode {
  id: string;
  depends_on: string[];
}

export interface CycleDetectionResult {
  has_cycles: boolean;
  cycles: string[][]; // each cycle is a list of node ids
  nodes_in_cycles: Set<string>;
}

/** Detect cycles in the task dependency graph using DFS. Returns all cycles
 * found, plus the set of nodes that participate in any cycle (for visual
 * distinction — FR-020). */
export function detectCycles(nodes: DependencyNode[]): CycleDetectionResult {
  const graph = new Map<string, string[]>();
  nodes.forEach((n) => graph.set(n.id, n.depends_on));

  const visited = new Set<string>();
  const inStack = new Set<string>();
  const cycles: string[][] = [];
  const nodesInCycles = new Set<string>();

  function dfs(node: string, path: string[]): void {
    if (inStack.has(node)) {
      // Found a cycle — extract it from the path.
      const cycleStart = path.indexOf(node);
      if (cycleStart >= 0) {
        const cycle = path.slice(cycleStart).concat([node]);
        cycles.push(cycle);
        cycle.forEach((n) => nodesInCycles.add(n));
      }
      return;
    }
    if (visited.has(node)) return;

    visited.add(node);
    inStack.add(node);
    path.push(node);

    const deps = graph.get(node) ?? [];
    for (const dep of deps) {
      dfs(dep, [...path]);
    }

    inStack.delete(node);
  }

  nodes.forEach((n) => dfs(n.id, []));

  return {
    has_cycles: cycles.length > 0,
    cycles,
    nodes_in_cycles: nodesInCycles,
  };
}

/** Render cycle annotations on the dependency view. Nodes in cycles get a
 * distinct visual treatment (red ring + warning icon). */
export function annotateCycles(
  container: HTMLElement,
  result: CycleDetectionResult,
): void {
  if (!result.has_cycles) return;

  // Add a banner.
  const banner = document.createElement('div');
  banner.style.cssText = 'padding:8px 12px;background:#fef2f2;border:1px solid #dc2626;border-radius:4px;margin-bottom:8px;font-size:13px;color:#dc2626;';
  banner.setAttribute('role', 'alert');
  banner.innerHTML = `<strong>⚠ ${result.cycles.length} cycle(s) detected</strong> — these tasks have circular dependencies.`;
  container.insertBefore(banner, container.firstChild);

  // Highlight nodes in cycles.
  container.querySelectorAll('[data-task-id]').forEach((el) => {
    const taskId = el.getAttribute('data-task-id') ?? '';
    if (result.nodes_in_cycles.has(taskId)) {
      (el as HTMLElement).style.outline = '3px solid #dc2626';
      (el as HTMLElement).style.borderRadius = '4px';
      el.setAttribute('aria-label', `${el.getAttribute('aria-label') ?? ''} — part of a dependency cycle`);
    }
  });
}
