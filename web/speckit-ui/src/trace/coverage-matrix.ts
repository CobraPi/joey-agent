// Coverage matrix widget (T065, FR-022).
// Renders the requirement × story density grid with orphan cells visually
// distinct. Selecting a cell broadcasts the selection.

export interface CoverageMatrixData {
  requirements: string[];
  stories: string[];
  matrix: Array<{ requirement_id: string; cells: Array<{ story_id: string; task_count: number }> }>;
  orphans: Array<{ id: string; impact: string }>;
}

export class CoverageMatrix {
  constructor(private root: HTMLElement) {}

  render(data: CoverageMatrixData, onSelect?: (reqId: string, storyId: string) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'grid');
    this.root.setAttribute('aria-label', 'Coverage matrix');

    const orphanSet = new Set(data.orphans.map((o) => o.id));

    const table = document.createElement('table');
    table.style.cssText = 'border-collapse:collapse;font-size:13px;width:100%;';

    // Header row: empty corner + story ids.
    const thead = document.createElement('thead');
    const headerRow = document.createElement('tr');
    headerRow.appendChild(this.th(''));
    data.stories.forEach((s) => headerRow.appendChild(this.th(s)));
    thead.appendChild(headerRow);
    table.appendChild(thead);

    // Body rows: one per requirement.
    const tbody = document.createElement('tbody');
    data.matrix.forEach((row) => {
      const tr = document.createElement('tr');
      const isOrphan = orphanSet.has(row.requirement_id);
      const rowLabel = document.createElement('th');
      rowLabel.textContent = row.requirement_id;
      rowLabel.style.cssText = `padding:4px 8px;text-align:left;border:1px solid #ddd;background:${isOrphan ? '#fef2f2' : '#f9fafb'};${isOrphan ? 'color:#dc2626;' : ''}`;
      if (isOrphan) {
        rowLabel.title = 'Orphan requirement — no implementing tasks';
      }
      tr.appendChild(rowLabel);

      row.cells.forEach((cell) => {
        const td = document.createElement('td');
        td.setAttribute('role', 'gridcell');
        const density = Math.min(cell.task_count, 5);
        const bg = cell.task_count > 0 ? `rgba(37, 99, 235, ${0.15 + density * 0.15})` : '#fef2f2';
        td.style.cssText = `padding:4px 8px;text-align:center;border:1px solid #ddd;background:${bg};cursor:pointer;`;
        td.textContent = String(cell.task_count);
        td.setAttribute('aria-label', `${row.requirement_id} × ${cell.story_id}: ${cell.task_count} task(s)`);
        td.addEventListener('click', () => onSelect?.(row.requirement_id, cell.story_id));
        tr.appendChild(td);
      });
      tbody.appendChild(tr);
    });
    table.appendChild(tbody);

    this.root.appendChild(table);
  }

  private th(text: string): HTMLElement {
    const th = document.createElement('th');
    th.textContent = text;
    th.style.cssText = 'padding:4px 8px;border:1px solid #ddd;background:#f3f4f6;font-family:monospace;font-size:11px;';
    return th;
  }
}
