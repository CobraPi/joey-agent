// Structured form editor (T045, FR-015).
// Typed form fields per node kind — the default depth, impossible to produce
// malformed markdown. Compiles to a single Replace op.

import type { PatchOp } from '../api-client';

export type NodeKind = 'requirement' | 'task' | 'user_story' | 'success_criterion';

export interface StructuredFormData {
  kind: NodeKind;
  nodeId: string;
  fields: Record<string, string | boolean | null>;
}

/** Typed form fields per node kind. Produces a single Replace op. */
export class StructuredForm {
  constructor(private root: HTMLElement) {}

  render(data: StructuredFormData, onSubmit?: (op: PatchOp) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'form');
    this.root.setAttribute('aria-label', `Edit ${data.kind} ${data.nodeId}`);

    const form = document.createElement('form');
    form.style.cssText = 'padding:12px;border:1px solid #ddd;border-radius:8px;';

    const fieldValues: Record<string, HTMLInputElement | HTMLSelectElement> = {};

    Object.entries(data.fields).forEach(([key, _value]) => {
      const label = document.createElement('label');
      label.style.cssText = 'display:block;margin-bottom:8px;font-weight:600;font-size:13px;';
      label.textContent = this.labelFor(data.kind, key);

      let input: HTMLInputElement | HTMLSelectElement;
      if (key === 'modality') {
        const select = document.createElement('select');
        select.style.cssText = 'display:block;width:100%;padding:4px;margin-top:4px;';
        ['Must', 'Should', 'May', 'MustNot'].forEach((m) => {
          const opt = document.createElement('option');
          opt.value = m;
          opt.textContent = m;
          select.appendChild(opt);
        });
        input = select;
      } else if (key === 'priority') {
        const select = document.createElement('select');
        select.style.cssText = 'display:block;width:100%;padding:4px;margin-top:4px;';
        ['P1', 'P2', 'P3'].forEach((p) => {
          const opt = document.createElement('option');
          opt.value = p;
          opt.textContent = p;
          select.appendChild(opt);
        });
        input = select;
      } else if (typeof data.fields[key] === 'boolean') {
        const cb = document.createElement('input');
        cb.type = 'checkbox';
        input = cb;
      } else {
        const ti = document.createElement('input');
        ti.type = 'text';
        ti.style.cssText = 'display:block;width:100%;padding:4px;margin-top:4px;border:1px solid #ccc;border-radius:4px;';
        input = ti;
      }
      input.value = String(data.fields[key] ?? '');
      input.setAttribute('aria-label', this.labelFor(data.kind, key));
      fieldValues[key] = input;

      label.appendChild(input);
      form.appendChild(label);
    });

    const submit = document.createElement('button');
    submit.textContent = 'Save';
    submit.type = 'button';
    submit.style.cssText = 'margin-top:8px;padding:6px 16px;background:#16a34a;color:white;border:none;border-radius:4px;cursor:pointer;';
    submit.addEventListener('click', () => {
      const compiled = this.compile(data, fieldValues);
      onSubmit?.(compiled);
    });
    form.appendChild(submit);

    this.root.appendChild(form);
  }

  private labelFor(_kind: NodeKind, key: string): string {
    const labels: Record<string, string> = {
      text: 'Requirement text',
      modality: 'Modality',
      priority: 'Priority',
      title: 'Title',
      given: 'Given',
      when: 'When',
      then: 'Then',
      target_value: 'Target value',
      unit: 'Unit',
    };
    return labels[key] ?? key;
  }

  /** Compile the form to a single Replace op. */
  private compile(data: StructuredFormData, fields: Record<string, HTMLInputElement | HTMLSelectElement>): PatchOp {
    const newBytes = this.compileBytes(data.kind, fields);
    return { op: 'replace', node: Number(data.nodeId), new_bytes: newBytes };
  }

  /** Produce markdown bytes from the form fields. */
  private compileBytes(kind: NodeKind, fields: Record<string, HTMLInputElement | HTMLSelectElement>): string {
    switch (kind) {
      case 'requirement': {
        const modality = (fields['modality'] as HTMLSelectElement).value;
        const text = (fields['text'] as HTMLInputElement).value;
        return `- **FR-001**: The system ${modality.toUpperCase()} ${text}.\n`;
      }
      case 'user_story': {
        const title = (fields['title'] as HTMLInputElement).value;
        return `### User Story 1: ${title}\n`;
      }
      default:
        return (fields['text'] as HTMLInputElement)?.value ?? '';
    }
  }
}
