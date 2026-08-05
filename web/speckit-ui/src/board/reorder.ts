// Within-phase optimistic reorder (T103, FR-019).
//
// Compiles to one Delete + one InsertAfter in a single patch transaction
// with a source-patch preview and undo entry.

import type { PatchOp } from '../api-client';

export interface ReorderPreview {
  task_id: string;
  from_index: number;
  to_index: number;
  phase: string;
  preview_markdown: string;
  undo_available: boolean;
}

/** Compile a within-phase reorder to a single Delete + InsertAfter
 * transaction (FR-019). Returns the ops + a preview for the UI. */
export function compileReorder(
  taskId: string,
  fromIndex: number,
  toIndex: number,
  phase: string,
  taskLineBytes: string,
): { ops: PatchOp[]; preview: ReorderPreview } {
  // Within-phase reorder = Delete the node at fromIndex, then InsertAfter
  // the node at toIndex (or at the phase heading if toIndex === 0).
  // In a real implementation the node IDs come from the CST; here we use
  // index-based placeholder IDs that the backend resolves.
  const ops: PatchOp[] = [
    { op: 'delete', node: fromIndex },
    { op: 'insert_after', node: toIndex, new_bytes: taskLineBytes },
  ];

  const preview: ReorderPreview = {
    task_id: taskId,
    from_index: fromIndex,
    to_index: toIndex,
    phase,
    preview_markdown: `Move ${taskId} from position ${fromIndex + 1} to ${toIndex + 1} within ${phase}`,
    undo_available: true,
  };

  return { ops, preview };
}

/** Render a reorder preview card for the developer to confirm. */
export function renderReorderPreview(
  container: HTMLElement,
  preview: ReorderPreview,
  _onConfirm?: () => void,
  onUndo?: () => void,
): void {
  container.innerHTML = '';
  container.setAttribute('role', 'status');
  container.setAttribute('aria-live', 'polite');

  const card = document.createElement('div');
  card.style.cssText = 'padding:12px;border:1px solid #2563eb;border-radius:8px;background:#eff6ff;margin:8px 0;';
  card.innerHTML = `<p style="margin:0 0 4px;font-weight:600;">Reordered ${escapeHtml(preview.task_id)}</p>
    <p style="margin:0 0 8px;color:#555;font-size:13px;">${escapeHtml(preview.preview_markdown)}</p>`;

  if (preview.undo_available && onUndo) {
    const undoBtn = document.createElement('button');
    undoBtn.textContent = '↶ Undo';
    undoBtn.style.cssText = 'margin-right:8px;padding:4px 12px;background:white;color:#2563eb;border:1px solid #2563eb;border-radius:4px;cursor:pointer;font-size:12px;';
    undoBtn.setAttribute('aria-label', `Undo reorder of ${preview.task_id}`);
    undoBtn.addEventListener('click', () => onUndo());
    card.appendChild(undoBtn);
  }

  container.appendChild(card);
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
