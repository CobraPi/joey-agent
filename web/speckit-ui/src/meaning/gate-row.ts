// Gate row + complexity violation widget (T043, FR-009).
// Renders ConstitutionGate pass/fail rows with evidence + aggregate gauge,
// and ComplexityViolation as side-by-side rule/need/rejected-alternative card.

export interface GateRowData {
  gates: Array<{ principle: string; result: 'Pass' | 'Fail' | 'Warn'; evidence: string }>;
  violations: Array<{ rule: string; why_needed: string; rejected_alternative: string }>;
}

export class GateRow {
  constructor(private root: HTMLElement) {}

  render(data: GateRowData): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', 'Constitution gates and complexity tracking');

    // Aggregate gauge.
    const passed = data.gates.filter((g) => g.result === 'Pass').length;
    const total = data.gates.length;
    const pct = total > 0 ? Math.round((passed / total) * 100) : 100;
    const gaugeColor = pct === 100 ? '#16a34a' : pct >= 50 ? '#d97706' : '#dc2626';

    const gauge = document.createElement('div');
    gauge.style.cssText = `padding:12px;border-radius:8px;background:${gaugeColor}11;margin-bottom:12px;`;
    gauge.innerHTML = `<p style="margin:0;"><strong style="color:${gaugeColor};">${passed}/${total}</strong> principles passed <span style="color:#666;">(${pct}%)</span></p>`;
    this.root.appendChild(gauge);

    // Gate rows.
    const gateList = document.createElement('div');
    gateList.setAttribute('role', 'table');
    data.gates.forEach((g) => {
      const row = document.createElement('div');
      row.setAttribute('role', 'row');
      const icon = g.result === 'Pass' ? '✓' : g.result === 'Fail' ? '✗' : '⚠';
      const color = g.result === 'Pass' ? '#16a34a' : g.result === 'Fail' ? '#dc2626' : '#d97706';
      row.style.cssText = `display:flex;align-items:center;gap:8px;padding:6px 0;border-bottom:1px solid #eee;`;
      row.innerHTML = `<span style="color:${color};font-weight:700;width:20px;" aria-hidden="true">${icon}</span><strong style="width:40px;">${escapeHtml(g.principle)}</strong><span style="color:#666;font-size:13px;">${escapeHtml(g.evidence)}</span>`;
      row.setAttribute('aria-label', `Principle ${g.principle}: ${g.result}. ${g.evidence}`);
      gateList.appendChild(row);
    });
    this.root.appendChild(gateList);

    // Complexity violations.
    if (data.violations.length > 0) {
      const heading = document.createElement('h4');
      heading.textContent = 'Complexity Tracking';
      heading.style.cssText = 'margin:16px 0 8px;font-size:14px;';
      this.root.appendChild(heading);

      data.violations.forEach((v) => {
        const card = document.createElement('div');
        card.style.cssText = 'border:1px solid #f59e0b;border-radius:8px;padding:12px;margin:8px 0;background:#fffbeb;';
        card.innerHTML = `<p style="margin:0 0 8px;font-weight:600;">${escapeHtml(v.rule)}</p>
          <div style="display:grid;grid-template-columns:1fr 1fr;gap:12px;font-size:13px;">
            <div><strong style="display:block;color:#92400e;">Why needed</strong>${escapeHtml(v.why_needed)}</div>
            <div><strong style="display:block;color:#92400e;">Rejected alternative</strong>${escapeHtml(v.rejected_alternative)}</div>
          </div>`;
        this.root.appendChild(card);
      });
    }
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
