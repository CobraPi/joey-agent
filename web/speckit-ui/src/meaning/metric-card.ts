// Metric card widget (T040, FR-010).
// Renders SuccessCriterion with target/unit/direction + OriginTag-distinguished
// evidence. "Not measured" when no evidence source; no decorative bars.

export interface MetricCardData {
  id: string;
  target_value: number | null;
  unit: string | null;
  direction: 'HigherIsBetter' | 'LowerIsBetter' | 'Equal' | null;
  text: string;
  evidence: { origin: 'Source' | 'Derived' | 'Overlay'; value: number | null; source_name: string | null } | null;
}

export class MetricCard {
  constructor(private root: HTMLElement) {}

  render(data: MetricCardData): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', `Success criterion ${data.id}`);

    const card = document.createElement('div');
    card.style.cssText = 'border:1px solid #e5e7eb;border-radius:8px;padding:12px;margin:8px 0;background:white;';

    // Header: id + target.
    const header = document.createElement('div');
    header.style.cssText = 'display:flex;justify-content:space-between;';
    header.innerHTML = `<strong style="font-family:monospace;">${escapeHtml(data.id)}</strong>`;
    if (data.target_value !== null) {
      const dir = data.direction === 'HigherIsBetter' ? '↑' : data.direction === 'LowerIsBetter' ? '↓' : '=';
      header.innerHTML += `<span style="color:#666;">Target: <strong>${data.target_value}${data.unit ? ' ' + escapeHtml(data.unit) : ''}</strong> ${dir}</span>`;
    }
    card.appendChild(header);

    // Description.
    const desc = document.createElement('p');
    desc.textContent = data.text;
    desc.style.cssText = 'margin:8px 0 0;color:#555;font-size:13px;';
    card.appendChild(desc);

    // Evidence — origin-tagged. "Not measured" when no evidence (FR-010).
    const evidenceDiv = document.createElement('div');
    evidenceDiv.style.cssText = 'margin-top:8px;padding:8px;background:#f9fafb;border-radius:4px;font-size:12px;';
    if (data.evidence && data.evidence.value !== null) {
      const originColor = data.evidence.origin === 'Overlay' ? '#2563eb' : data.evidence.origin === 'Derived' ? '#d97706' : '#16a34a';
      const originLabel = data.evidence.origin;
      evidenceDiv.innerHTML = `<span style="color:${originColor};font-weight:600;">${originLabel}</span>: ${data.evidence.value}${data.unit ? ' ' + escapeHtml(data.unit) : ''}${data.evidence.source_name ? ` <span style="color:#999;">(${escapeHtml(data.evidence.source_name)})</span>` : ''}`;
    } else {
      // FR-010: "not measured" with NO decorative bar implying absent data.
      evidenceDiv.innerHTML = `<span style="color:#9ca3af;">Not measured</span>`;
    }
    card.appendChild(evidenceDiv);

    this.root.appendChild(card);
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
