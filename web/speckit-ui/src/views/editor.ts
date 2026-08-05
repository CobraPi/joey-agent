import type { ArtifactContent, PatchArtifactRequest, ValidationFinding, OutlineEntry } from '../api-client';
import { SpeckitApiClient, ApiError } from '../api-client';

type EditorSaveState = 'clean' | 'dirty' | 'saving' | 'saved' | 'invalid' | 'externally_changed' | 'read_only';

/** Source + rendered reading view with outline navigation (FR-006).
 * Save-state transitions: dirty→saving→saved→invalid→externally_changed (FR-005). */
export class EditorView {
  private el: HTMLElement;
  private api: SpeckitApiClient;
  private featureId: string;
  private currentPath: string | null = null;
  private currentHash: string | null = null;
  private textarea: HTMLTextAreaElement | null = null;
  private outlineEl: HTMLElement | null = null;
  private statusEl: HTMLElement | null = null;
  private saveState: EditorSaveState = 'clean';
  private dirtyText: string | null = null;

  constructor(api: SpeckitApiClient, featureId: string) {
    this.api = api;
    this.featureId = featureId;
    this.el = document.createElement('div');
    this.el.className = 'editor-view';
    this.el.setAttribute('role', 'region');
    this.el.setAttribute('aria-label', 'Artifact editor');
  }

  get element(): HTMLElement {
    return this.el;
  }

  get isDirty(): boolean {
    return this.saveState === 'dirty' && this.dirtyText !== null;
  }

  /** Open an artifact for editing (FR-006). */
  async open(path: string): Promise<boolean> {
    if (this.isDirty) {
      const proceed = confirm('You have unsaved changes. Discard them?');
      if (!proceed) return false;
    }

    try {
      const content = await this.api.getArtifact(this.featureId, path);
      this.currentPath = path;
      this.currentHash = content.content_hash;
      this.saveState = 'clean';
      this.dirtyText = null;
      this.render(content);
      return true;
    } catch (e) {
      if (e instanceof Error && e.message.includes('404')) {
        // Non-existent artifact: offer creation.
        this.currentPath = path;
        this.currentHash = null;
        this.renderEmpty(path);
        return true;
      }
      this.el.innerHTML = `<p class="error">Failed to open: ${esc(String(e))}</p>`;
      return false;
    }
  }

  private render(content: ArtifactContent): void {
    this.el.innerHTML = `
      <div class="editor-toolbar">
        <span class="editor-path">${esc(content.path)}</span>
        <span class="editor-state ${this.saveState}" role="status">${this.saveState}</span>
        <button class="save-btn" type="button" aria-label="Save artifact">Save</button>
      </div>
      <div class="editor-body">
        <div class="editor-outline" role="navigation" aria-label="Document outline"></div>
        <textarea class="editor-source" spellcheck="false" aria-label="Artifact source"></textarea>
      </div>
      <div class="editor-validity" role="region" aria-label="Validation findings"></div>
    `;

    this.textarea = this.el.querySelector('.editor-source');
    this.outlineEl = this.el.querySelector('.editor-outline');
    this.statusEl = this.el.querySelector('.editor-state');

    if (this.textarea) {
      this.textarea.value = content.text;
      this.textarea.addEventListener('input', () => {
        this.markDirty();
      });
    }

    this.renderOutline(content.outline);
    this.renderValidity(content.validity);

    const saveBtn = this.el.querySelector('.save-btn');
    if (saveBtn) {
      saveBtn.addEventListener('click', () => this.save());
    }
  }

  private renderEmpty(path: string): void {
    this.el.innerHTML = `
      <div class="editor-empty">
        <p>Artifact <code>${esc(path)}</code> does not exist yet.</p>
        <button class="create-btn" type="button">Create it</button>
      </div>
    `;
    const createBtn = this.el.querySelector('.create-btn');
    if (createBtn) {
      createBtn.addEventListener('click', () => {
        this.currentHash = ''; // empty file, no conflict check
        this.render({
          path,
          kind: 'supporting',
          text: '',
          content_hash: '',
          outline: [],
          save_state: 'dirty',
          validity: [],
        });
        this.markDirty();
      });
    }
  }

  private renderOutline(outline: OutlineEntry[]): void {
    if (!this.outlineEl || outline.length === 0) return;
    let html = '<ul class="outline-list">';
    for (const entry of outline) {
      const indent = (entry.level - 1) * 12;
      html += `<li class="outline-entry" style="margin-left:${indent}px" data-line="${entry.line}">`;
      html += `<a href="#line-${entry.line}">${esc(entry.title)}</a></li>`;
    }
    html += '</ul>';
    this.outlineEl.innerHTML = html;

    this.outlineEl.querySelectorAll<HTMLElement>('.outline-entry').forEach(item => {
      item.addEventListener('click', () => {
        const line = parseInt(item.dataset.line || '0', 10);
        if (this.textarea && line > 0) {
          const lines = this.textarea.value.split('\n');
          const offset = lines.slice(0, line - 1).join('\n').length;
          this.textarea.focus();
          this.textarea.setSelectionRange(offset, offset);
        }
      });
    });
  }

  private renderValidity(findings: ValidationFinding[]): void {
    const el = this.el.querySelector('.editor-validity');
    if (!el) return;
    if (findings.length === 0) {
      el.innerHTML = '';
      return;
    }
    let html = '<ul class="findings-list">';
    for (const f of findings) {
      const cls = f.severity.toLowerCase();
      html += `<li class="finding ${cls}" role="${f.severity === 'Critical' ? 'alert' : 'listitem'}">`;
      html += `<span class="finding-severity">${f.severity}</span> `;
      html += `<span class="finding-desc">${esc(f.description)}</span>`;
      if (f.remediation) {
        html += `<span class="finding-remediation">${esc(f.remediation)}</span>`;
      }
      html += '</li>';
    }
    html += '</ul>';
    el.innerHTML = html;
  }

  private markDirty(): void {
    if (this.saveState === 'clean') {
      this.saveState = 'dirty';
      this.dirtyText = this.textarea?.value ?? null;
      this.updateStateDisplay();
    }
  }

  private updateStateDisplay(): void {
    if (this.statusEl) {
      this.statusEl.textContent = this.saveState;
      this.statusEl.className = `editor-state ${this.saveState}`;
    }
  }

  /** Save the current artifact (FR-004/005/020). */
  async save(): Promise<void> {
    if (!this.textarea || !this.currentPath) return;
    if (this.saveState !== 'dirty') return;

    this.saveState = 'saving';
    this.updateStateDisplay();

    const req: PatchArtifactRequest = {
      new_text: this.textarea.value,
      based_on_hash: this.currentHash || '',
      scope: { whole: true },
    };

    try {
      const resp = await this.api.patchArtifact(this.featureId, this.currentPath, req);
      this.currentHash = resp.content_hash;
      this.saveState = 'saved';
      this.dirtyText = null;
      this.updateStateDisplay();
      setTimeout(() => {
        if (this.saveState === 'saved') {
          this.saveState = 'clean';
          this.updateStateDisplay();
        }
      }, 2000);
    } catch (e) {
      if (e instanceof ApiError && e.isConflict) {
        this.saveState = 'externally_changed';
        this.updateStateDisplay();
        const reload = confirm(
          'This file was changed externally. Reload (discarding your edits) or compare?'
        );
        if (reload && this.currentPath) {
          await this.open(this.currentPath);
        }
      } else if (e instanceof ApiError && e.code === 'invalid_request') {
        this.saveState = 'invalid';
        this.updateStateDisplay();
      } else {
        this.saveState = 'dirty';
        this.updateStateDisplay();
        this.el.querySelector('.editor-validity')!.innerHTML =
          `<p class="error" role="alert">Save failed: ${esc(String(e))}</p>`;
      }
    }
  }

  /** Handle external file-change notification (FR-020 acceptance 3). */
  notifyExternalChange(): void {
    if (this.saveState === 'dirty' || this.saveState === 'clean') {
      this.saveState = 'externally_changed';
      this.updateStateDisplay();
    }
  }
}

function esc(s: string): string {
  const div = document.createElement('div');
  div.textContent = s;
  return div.innerHTML;
}
