// Clarify queue widget (T067, FR-024).
// Renders the batched clarify queue (all markers, not serial). Each marker
// has source line + owning requirement + downstream blockers. Answering
// previews a staged patch.

export interface ClarifyQueueData {
  markers: Array<{
    id: string;
    text: { text: string; owning_requirement: string | null };
    origin: string;
  }>;
}

export class ClarifyQueue {
  constructor(private root: HTMLElement) {}

  render(data: ClarifyQueueData, onAnswer?: (markerId: string, answer: string) => void): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', 'Batched clarify queue');

    if (data.markers.length === 0) {
      const ok = document.createElement('p');
      ok.textContent = '✓ No open clarifications.';
      ok.style.cssText = 'color:#16a34a;padding:12px;';
      this.root.appendChild(ok);
      return;
    }

    const heading = document.createElement('p');
    heading.innerHTML = `<strong>${data.markers.length}</strong> clarification(s) need answers:`;
    heading.style.cssText = 'margin:0 0 12px;';
    this.root.appendChild(heading);

    // Batched — all markers shown at once, not serially.
    data.markers.forEach((m) => {
      const card = document.createElement('section');
      card.setAttribute('aria-label', `Clarification: ${m.text.text}`);
      card.style.cssText = 'border:1px solid #f59e0b;border-radius:8px;padding:12px;margin:8px 0;background:#fffbeb;';

      card.innerHTML = `<p style="margin:0 0 4px;font-weight:600;">${escapeHtml(m.text.text)}</p>
        <p style="margin:0 0 8px;font-size:12px;color:#92400e;">${escapeHtml(m.origin)}${m.text.owning_requirement ? ` · owning: ${escapeHtml(m.text.owning_requirement)}` : ''}</p>`;

      // Inline answer input with staged-patch preview.
      const input = document.createElement('input');
      input.type = 'text';
      input.placeholder = 'Type the answer…';
      input.style.cssText = 'width:calc(100% - 80px);padding:6px;border:1px solid #ccc;border-radius:4px;';
      input.setAttribute('aria-label', `Answer for: ${m.text.text}`);

      const submit = document.createElement('button');
      submit.textContent = 'Answer';
      submit.style.cssText = 'margin-left:8px;padding:6px 12px;background:#d97706;color:white;border:none;border-radius:4px;cursor:pointer;';
      submit.addEventListener('click', () => {
        if (input.value.trim()) {
          onAnswer?.(m.id, input.value);
          input.disabled = true;
          submit.disabled = true;
          submit.textContent = '✓ Staged';
        }
      });

      card.appendChild(input);
      card.appendChild(submit);
      this.root.appendChild(card);
    });
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
