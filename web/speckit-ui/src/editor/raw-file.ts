// Raw file editor (T047, FR-015).
// CodeMirror 6 on the whole document (⌥⇧M) — the escape hatch.

import { EditorState } from '@codemirror/state';
import { EditorView, keymap } from '@codemirror/view';
import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
import { markdown } from '@codemirror/lang-markdown';

export interface RawFileOptions {
  /** The full file content. */
  initialText: string;
  /** Called on save with the full new content. */
  onSave?: (newContent: string) => void;
  onCancel?: () => void;
}

/** Raw whole-file editor — the escape hatch (⌥⇧M). */
export class RawFileEditor {
  private view: EditorView | null = null;

  constructor(private root: HTMLElement) {}

  render(opts: RawFileOptions): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'textbox');
    this.root.setAttribute('aria-label', 'Raw markdown editor (whole file)');

    const state = EditorState.create({
      doc: opts.initialText,
      extensions: [
        history(),
        keymap.of([...defaultKeymap, ...historyKeymap]),
        markdown(),
        EditorView.lineWrapping,
      ],
    });

    this.view = new EditorView({ state, parent: this.root });

    const controls = document.createElement('div');
    controls.style.cssText = 'margin-top:8px;display:flex;gap:8px;';

    const save = document.createElement('button');
    save.textContent = 'Save';
    save.style.cssText = 'padding:6px 16px;background:#16a34a;color:white;border:none;border-radius:4px;cursor:pointer;';
    save.addEventListener('click', () => {
      if (this.view) {
        opts.onSave?.(this.view.state.doc.toString());
      }
    });

    const cancel = document.createElement('button');
    cancel.textContent = 'Cancel';
    cancel.style.cssText = 'padding:6px 16px;background:#e5e7eb;border:none;border-radius:4px;cursor:pointer;';
    cancel.addEventListener('click', () => opts.onCancel?.());

    controls.appendChild(save);
    controls.appendChild(cancel);
    this.root.appendChild(controls);
  }

  destroy(): void {
    this.view?.destroy();
    this.view = null;
    this.root.innerHTML = '';
  }
}
