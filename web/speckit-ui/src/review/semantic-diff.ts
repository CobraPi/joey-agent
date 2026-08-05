// Semantic diff review (T075, FR-029).
// Renders staged changes as hunks labelled by semantic meaning, with
// per-hunk accept/reject.

export interface SemanticDiffData {
  hunks: Array<{
    hunk_id: string;
    semantic_label: string;
    old_bytes: string;
    new_bytes: string;
    accept_state: 'pending' | 'accepted' | 'rejected';
  }>;
}

export class SemanticDiff {
  constructor(private root: HTMLElement) {}

  render(
    data: SemanticDiffData,
    onAccept?: (hunkId: string) => void,
    onReject?: (hunkId: string) => void,
  ): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', 'Semantic diff review');

    if (data.hunks.length === 0) {
      this.root.innerHTML = '<p style="color:#999;padding:12px;">No staged changes to review.</p>';
      return;
    }

    data.hunks.forEach((h) => {
      const card = document.createElement('section');
      const borderColor = h.accept_state === 'accepted' ? '#16a34a' : h.accept_state === 'rejected' ? '#dc2626' : '#ddd';
      card.style.cssText = `border:1px solid ${borderColor};border-radius:8px;padding:12px;margin:8px 0;`;

      card.innerHTML = `<p style="margin:0 0 8px;font-weight:600;">${escapeHtml(h.semantic_label)}</p>
        <pre style="margin:0 0 8px;font-size:12px;background:#f9fafb;padding:8px;border-radius:4px;overflow-x:auto;"><span style="color:#dc2626;">- ${escapeHtml(h.old_bytes)}</span>\n<span style="color:#16a34a;">+ ${escapeHtml(h.new_bytes)}</span></pre>`;

      if (h.accept_state === 'pending') {
        const controls = document.createElement('div');
        controls.style.cssText = 'display:flex;gap:8px;';

        const accept = document.createElement('button');
        accept.textContent = '✓ Accept';
        accept.style.cssText = 'padding:6px 12px;background:#16a34a;color:white;border:none;border-radius:4px;cursor:pointer;';
        accept.setAttribute('aria-label', `Accept hunk: ${h.semantic_label}`);
        accept.addEventListener('click', () => onAccept?.(h.hunk_id));

        const reject = document.createElement('button');
        reject.textContent = '✗ Reject';
        reject.style.cssText = 'padding:6px 12px;background:#dc2626;color:white;border:none;border-radius:4px;cursor:pointer;';
        reject.setAttribute('aria-label', `Reject hunk: ${h.semantic_label}`);
        reject.addEventListener('click', () => onReject?.(h.hunk_id));

        controls.appendChild(accept);
        controls.appendChild(reject);
        card.appendChild(controls);
      } else {
        const status = document.createElement('p');
        status.textContent = h.accept_state === 'accepted' ? '✓ Accepted' : '✗ Rejected';
        status.style.cssText = `margin:0;color:${h.accept_state === 'accepted' ? '#16a34a' : '#dc2626'};font-weight:600;`;
        card.appendChild(status);
      }

      this.root.appendChild(card);
    });
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
