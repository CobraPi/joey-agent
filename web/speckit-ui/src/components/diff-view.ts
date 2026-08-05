import type { ChangedFile, Hunk } from '../api-client';

/** Diff rendering with additions/removals per file and hunk/file accept-reject (FR-016). */
export class DiffView {
  private el: HTMLElement;
  private files: ChangedFile[] = [];
  private acceptedHunks: Set<string> = new Set();
  private rejectedHunks: Set<string> = new Set();

  constructor() {
    this.el = document.createElement('div');
    this.el.className = 'diff-view';
    this.el.setAttribute('role', 'region');
    this.el.setAttribute('aria-label', 'Diff review');
  }

  get element(): HTMLElement {
    return this.el;
  }

  setFiles(files: ChangedFile[]): void {
    this.files = files;
    this.acceptedHunks.clear();
    this.rejectedHunks.clear();
    this.render();
  }

  private render(): void {
    if (this.files.length === 0) {
      this.el.innerHTML = '<p class="empty">No changes to review.</p>';
      return;
    }

    let html = '';
    for (const file of this.files) {
      html += this.renderFile(file);
    }
    this.el.innerHTML = html;

    this.el.querySelectorAll<HTMLButtonElement>('.hunk-accept').forEach(btn => {
      btn.addEventListener('click', () => {
        const hunkId = btn.dataset.hunkId!;
        this.acceptedHunks.add(hunkId);
        this.rejectedHunks.delete(hunkId);
        this.updateHunkState(hunkId);
      });
    });

    this.el.querySelectorAll<HTMLButtonElement>('.hunk-reject').forEach(btn => {
      btn.addEventListener('click', () => {
        const hunkId = btn.dataset.hunkId!;
        this.rejectedHunks.add(hunkId);
        this.acceptedHunks.delete(hunkId);
        this.updateHunkState(hunkId);
      });
    });

    this.el.querySelectorAll<HTMLButtonElement>('.file-accept').forEach(btn => {
      btn.addEventListener('click', () => {
        const path = btn.dataset.path!;
        this.acceptAllInFile(path);
      });
    });
  }

  private renderFile(file: ChangedFile): string {
    const statusIcon = file.status === 'added' ? '➕' : file.status === 'removed' ? '➖' : '📝';
    let html = `<div class="diff-file" data-path="${esc(file.path)}">`;
    html += `<div class="diff-file-header">`;
    html += `<span class="diff-file-icon">${statusIcon}</span>`;
    html += `<span class="diff-file-path">${esc(file.path)}</span>`;
    html += `<span class="diff-file-stats">+${file.additions} -${file.removals}</span>`;
    if (file.why) {
      html += `<span class="diff-file-why" title="${esc(file.why)}">${esc(file.why)}</span>`;
    }
    html += `<button class="file-accept" type="button" data-path="${esc(file.path)}" aria-label="Accept all in file">Accept All</button>`;
    html += `</div>`;

    html += '<div class="diff-hunks">';
    for (const hunk of file.hunks) {
      html += this.renderHunk(hunk);
    }
    html += '</div></div>';
    return html;
  }

  private renderHunk(hunk: Hunk): string {
    let html = `<div class="diff-hunk" data-hunk-id="${esc(hunk.hunk_id)}">`;
    if (hunk.depends_on.length > 0) {
      html += `<div class="hunk-deps" title="Depends on: ${esc(hunk.depends_on.join(', '))}">⚠ depends on: ${esc(hunk.depends_on.join(', '))}</div>`;
    }
    html += `<div class="hunk-range">${esc(hunk.old_range)} → ${esc(hunk.new_range)}</div>`;
    html += `<div class="hunk-actions">`;
    html += `<button class="hunk-accept" type="button" data-hunk-id="${esc(hunk.hunk_id)}" aria-label="Accept hunk ${esc(hunk.hunk_id)}">✓</button>`;
    html += `<button class="hunk-reject" type="button" data-hunk-id="${esc(hunk.hunk_id)}" aria-label="Reject hunk ${esc(hunk.hunk_id)}">✗</button>`;
    html += `</div>`;
    html += `</div>`;
    return html;
  }

  private updateHunkState(hunkId: string): void {
    const el = this.el.querySelector(`[data-hunk-id="${CSS.escape(hunkId)}"]`);
    if (!el) return;
    if (this.acceptedHunks.has(hunkId)) {
      el.classList.add('accepted');
      el.classList.remove('rejected');
    } else if (this.rejectedHunks.has(hunkId)) {
      el.classList.add('rejected');
      el.classList.remove('accepted');
    } else {
      el.classList.remove('accepted', 'rejected');
    }
  }

  private acceptAllInFile(path: string): void {
    const file = this.files.find(f => f.path === path);
    if (!file) return;
    for (const hunk of file.hunks) {
      this.acceptedHunks.add(hunk.hunk_id);
      this.rejectedHunks.delete(hunk.hunk_id);
      this.updateHunkState(hunk.hunk_id);
    }
  }

  getAcceptedHunkIds(): string[] {
    return Array.from(this.acceptedHunks);
  }

  getRejectedHunkIds(): string[] {
    return Array.from(this.rejectedHunks);
  }
}

function esc(s: string): string {
  const div = document.createElement('div');
  div.textContent = s;
  return div.innerHTML;
}
