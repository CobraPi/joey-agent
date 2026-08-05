// Inline markdown editor (T046, FR-015).
// CodeMirror 6 (`codemirror` + `@codemirror/lang-markdown`, framework-free —
// research.md §2) on just the selected node's byte range (⌥M). Maps CodeMirror
// line offsets back to CST byte anchors.

import { EditorState } from '@codemirror/state';
import { EditorView, keymap } from '@codemirror/view';
import { defaultKeymap, history, historyKeymap } from '@codemirror/commands';
import { markdown } from '@codemirror/lang-markdown';
import type { PatchOp } from '../api-client';

export interface InlineMarkdownOptions {
  /** The node's source bytes (from the CST expected_bytes). */
  initialText: string;
  /** The node's byte_start in the file (for offset mapping). */
  byteStart: number;
  /** Called on save with the new bytes. Compiles to a Replace op. */
  onSave?: (newBytes: string) => void;
  /** Called on cancel. */
  onCancel?: () => void;
}

/** Inline markdown editor on a node's byte range (⌥M). Uses CodeMirror 6. */
export class InlineMarkdownEditor {
  private view: EditorView | null = null;
  private byteStart: number;

  constructor(private root: HTMLElement) {
    this.byteStart = 0;
  }

  render(opts: InlineMarkdownOptions): void {
    this.root.innerHTML = '';
    this.byteStart = opts.byteStart;
    this.root.setAttribute('role', 'textbox');
    this.root.setAttribute('aria-label', 'Inline markdown editor');

    const state = EditorState.create({
      doc: opts.initialText,
      extensions: [
        history(),
        keymap.of([...defaultKeymap, ...historyKeymap]),
        markdown(),
        EditorView.lineWrapping,
        EditorView.updateListener.of((update: { docChanged: boolean }) => {
          if (update.docChanged) {
            // Document changed — the save handler reads the current doc.
          }
        }),
      ],
    });

    this.view = new EditorView({ state, parent: this.root });

    // Save / Cancel buttons.
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

  /** Map a CodeMirror offset to a file byte offset (CST anchor mapping). */
  mapToFileOffset(cmOffset: number): number {
    return this.byteStart + cmOffset;
  }

  /** Clean up the editor. */
  destroy(): void {
    this.view?.destroy();
    this.view = null;
    this.root.innerHTML = '';
  }
}

/** Compile inline-markdown changes to a Replace op. */
export function compileInlineReplace(nodeId: number, newBytes: string): PatchOp {
  return { op: 'replace', node: nodeId, new_bytes: newBytes };
}
