// Entity graph widget (T041, FR-011).
// Renders KeyEntity + EntityRelationship as a vanilla-SVG graph (no @xyflow —
// research.md §1). Proposed edges dashed and requiring confirmation.
// Relationship-table view as the keyboard/mobile alternative (FR-037/039).

export interface EntityGraphData {
  entities: Array<{ name: string; fields: string[] }>;
  relationships: Array<{
    source: string;
    verb: string;
    target: string;
    confidence: 'Explicit' | 'Proposed';
  }>;
}

export class EntityGraph {
  constructor(private root: HTMLElement) {}

  render(data: EntityGraphData, onConfirmEdge?: (idx: number) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', 'Entity relationship graph');

    // Toggle between SVG graph and table view (FR-037 keyboard alternative).
    const toggle = document.createElement('button');
    toggle.textContent = 'Switch to table view';
    toggle.style.cssText = 'margin-bottom:8px;padding:4px 12px;background:#e5e7eb;border:none;border-radius:4px;cursor:pointer;';
    let showGraph = true;
    toggle.addEventListener('click', () => {
      showGraph = !showGraph;
      toggle.textContent = showGraph ? 'Switch to table view' : 'Switch to graph view';
      this.render(data, onConfirmEdge);
      if (!showGraph) {
        this.renderTable(data, onConfirmEdge);
      }
    });
    this.root.appendChild(toggle);

    if (showGraph) {
      this.renderSvg(data, onConfirmEdge);
    }
  }

  private renderSvg(data: EntityGraphData, onConfirmEdge?: (idx: number) => void): void {
    const svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('width', '100%');
    svg.setAttribute('height', '300');
    svg.setAttribute('role', 'img');
    svg.setAttribute('aria-label', `${data.entities.length} entities, ${data.relationships.length} relationships`);

    // Position entities in a circle.
    const cx = 250;
    const cy = 150;
    const r = 100;
    const positions = new Map<string, { x: number; y: number }>();
    data.entities.forEach((e, i) => {
      const angle = (i / Math.max(data.entities.length, 1)) * 2 * Math.PI;
      const x = cx + r * Math.cos(angle);
      const y = cy + r * Math.sin(angle);
      positions.set(e.name, { x, y });

      const rect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
      rect.setAttribute('x', String(x - 50));
      rect.setAttribute('y', String(y - 15));
      rect.setAttribute('width', '100');
      rect.setAttribute('height', '30');
      rect.setAttribute('fill', '#dbeafe');
      rect.setAttribute('stroke', '#2563eb');
      rect.setAttribute('rx', '4');
      svg.appendChild(rect);

      const text = document.createElementNS('http://www.w3.org/2000/svg', 'text');
      text.setAttribute('x', String(x));
      text.setAttribute('y', String(y + 5));
      text.setAttribute('text-anchor', 'middle');
      text.setAttribute('font-size', '12');
      text.textContent = e.name;
      svg.appendChild(text);
    });

    // Draw edges.
    data.relationships.forEach((rel, idx) => {
      const from = positions.get(rel.source);
      const to = positions.get(rel.target);
      if (!from || !to) return;

      const line = document.createElementNS('http://www.w3.org/2000/svg', 'line');
      line.setAttribute('x1', String(from.x));
      line.setAttribute('y1', String(from.y));
      line.setAttribute('x2', String(to.x));
      line.setAttribute('y2', String(to.y));
      line.setAttribute('stroke', rel.confidence === 'Proposed' ? '#d97706' : '#333');
      line.setAttribute('stroke-width', '2');
      if (rel.confidence === 'Proposed') {
        line.setAttribute('stroke-dasharray', '5,3');
        line.style.cursor = 'pointer';
        line.addEventListener('click', () => onConfirmEdge?.(idx));
      }
      svg.appendChild(line);
    });

    this.root.appendChild(svg);
  }

  private renderTable(data: EntityGraphData, onConfirmEdge?: (idx: number) => void): void {
    const table = document.createElement('table');
    table.style.cssText = 'width:100%;border-collapse:collapse;font-size:13px;';
    table.innerHTML = '<thead><tr><th style="text-align:left;padding:4px;border-bottom:1px solid #ddd;">Source</th><th>Verb</th><th>Target</th><th>Status</th></tr></thead>';

    const tbody = document.createElement('tbody');
    data.relationships.forEach((rel, idx) => {
      const tr = document.createElement('tr');
      tr.innerHTML = `<td style="padding:4px;">${escapeHtml(rel.source)}</td><td>${escapeHtml(rel.verb)}</td><td>${escapeHtml(rel.target)}</td>`;
      const statusTd = document.createElement('td');
      if (rel.confidence === 'Proposed') {
        const btn = document.createElement('button');
        btn.textContent = 'Confirm';
        btn.style.cssText = 'padding:2px 8px;background:#d97706;color:white;border:none;border-radius:3px;cursor:pointer;';
        btn.addEventListener('click', () => onConfirmEdge?.(idx));
        statusTd.appendChild(btn);
      } else {
        statusTd.textContent = '✓ Confirmed';
        statusTd.style.color = '#16a34a';
      }
      tr.appendChild(statusTd);
      tbody.appendChild(tr);
    });
    table.appendChild(tbody);
    this.root.appendChild(table);
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
