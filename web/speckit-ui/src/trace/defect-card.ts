// Defect card widget (T066, FR-023).
// Renders the four defect classes with one-click fix. Deterministic scaffold
// applies instantly; generative follow-on offers agent-generated staged patch.

export interface DefectCardData {
  defects: Array<{
    id: string;
    class: 'OrphanRequirement' | 'RogueTask' | 'Unverified' | 'ConstitutionBreach';
    impact: string;
    scaffold: { target_artifact: string; stub_bytes: string };
    has_generative_followon: boolean;
  }>;
}

const CLASS_META: Record<string, { icon: string; color: string; label: string }> = {
  OrphanRequirement: { icon: '🔗', color: '#dc2626', label: 'Orphan requirement' },
  RogueTask: { icon: '🎯', color: '#d97706', label: 'Rogue task' },
  Unverified: { icon: '✓', color: '#2563eb', label: 'Unverified' },
  ConstitutionBreach: { icon: '⚖', color: '#7c3aed', label: 'Constitution breach' },
};

export class DefectCard {
  constructor(private root: HTMLElement) {}

  render(
    data: DefectCardData,
    onFix?: (defectId: string, generative: boolean) => void,
  ): void {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'region');
    this.root.setAttribute('aria-label', 'Traceability defects');

    if (data.defects.length === 0) {
      const ok = document.createElement('p');
      ok.textContent = '✓ No defects detected — full traceability.';
      ok.style.cssText = 'color:#16a34a;padding:12px;';
      this.root.appendChild(ok);
      return;
    }

    data.defects.forEach((d) => {
      const meta = CLASS_META[d.class] ?? CLASS_META.OrphanRequirement;
      const card = document.createElement('section');
      card.setAttribute('aria-label', `${meta.label}: ${d.impact}`);
      card.style.cssText = `border-left:4px solid ${meta.color};padding:12px;margin:8px 0;border-radius:0 4px 4px 0;background:#fafafa;`;

      card.innerHTML = `<p style="margin:0 0 4px;"><span aria-hidden="true">${meta.icon}</span> <strong>${meta.label}</strong></p>
        <p style="margin:0 0 8px;color:#555;font-size:13px;">${escapeHtml(d.impact)}</p>`;

      // One-click deterministic fix (instant, free — clarification Q3).
      const fixBtn = document.createElement('button');
      fixBtn.textContent = 'Apply scaffold (instant)';
      fixBtn.style.cssText = `margin-right:8px;padding:6px 12px;background:${meta.color};color:white;border:none;border-radius:4px;cursor:pointer;font-size:12px;`;
      fixBtn.setAttribute('aria-label', `Apply deterministic scaffold for ${meta.label}`);
      fixBtn.addEventListener('click', () => onFix?.(d.id, false));
      card.appendChild(fixBtn);

      // Generative follow-on (optional — agent-generated staged patch).
      if (d.has_generative_followon) {
        const genBtn = document.createElement('button');
        genBtn.textContent = 'Generate real content';
        genBtn.style.cssText = 'padding:6px 12px;background:white;color:#666;border:1px solid #ddd;border-radius:4px;cursor:pointer;font-size:12px;';
        genBtn.setAttribute('aria-label', `Generate real content for ${meta.label} via the agent`);
        genBtn.addEventListener('click', () => onFix?.(d.id, true));
        card.appendChild(genBtn);
      }

      this.root.appendChild(card);
    });
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
