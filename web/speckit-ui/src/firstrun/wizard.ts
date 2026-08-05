// First-run setup wizard (T032, FR-001).
//
// The five-step setup wizard: repo → Spec Kit check → branch → brief → preview.
// Vanilla-TS web component (research.md §1 — no React). Staged-mode
// confirmation before any write.

export interface SetupPreview {
  feature_id: string;
  branch: string;
  paths: string[];
  staged_mode: boolean;
  nothing_written: boolean;
}

export interface RepoScan {
  repo_root: string;
  exists: boolean;
  writable: boolean;
  has_specs_dir: boolean;
  has_specify_dir: boolean;
  setup_gaps: string[];
}

type Step = 'repo' | 'speckit' | 'branch' | 'brief' | 'preview';

/** Handlers the wizard needs to talk to the backend. */
export interface WizardHandlers {
  scanRepo: () => Promise<RepoScan>;
  preview: (brief: string) => Promise<SetupPreview>;
  commit: (featureId: string, brief: string) => Promise<void>;
  onComplete?: (featureId: string) => void;
}

/** Five-step setup wizard with staged-mode confirmation. */
export class FirstRunWizard {
  private root: HTMLElement;
  private step: Step = 'repo';
  private brief = '';
  private scan: RepoScan | null = null;
  private preview: SetupPreview | null = null;

  constructor(root: HTMLElement) {
    this.root = root;
  }

  /** Wire the wizard to API calls. Returns the wizard controller. */
  bind(handlers: WizardHandlers): void {
    void this.render(handlers);
  }

  private async render(handlers: WizardHandlers): Promise<void> {
    this.root.innerHTML = '';
    this.root.setAttribute('role', 'dialog');
    this.root.setAttribute('aria-label', 'First-run setup wizard');

    const container = document.createElement('div');
    container.style.cssText = 'max-width:600px;margin:40px auto;padding:24px;';

    // Step indicator.
    container.appendChild(this.stepIndicator());

    switch (this.step) {
      case 'repo':
        container.appendChild(await this.repoStep(handlers));
        break;
      case 'speckit':
        container.appendChild(this.speckitStep(handlers));
        break;
      case 'branch':
        container.appendChild(this.branchStep(handlers));
        break;
      case 'brief':
        container.appendChild(this.briefStep(handlers));
        break;
      case 'preview':
        container.appendChild(this.previewStep(handlers));
        break;
    }

    this.root.appendChild(container);
  }

  private stepIndicator(): HTMLElement {
    const steps: Step[] = ['repo', 'speckit', 'branch', 'brief', 'preview'];
    const labels: Record<Step, string> = {
      repo: 'Repo',
      speckit: 'Spec Kit',
      branch: 'Branch',
      brief: 'Brief',
      preview: 'Preview',
    };
    const bar = document.createElement('div');
    bar.style.cssText = 'display:flex;gap:8px;margin-bottom:24px;';
    bar.setAttribute('role', 'progressbar');
    const currentIdx = steps.indexOf(this.step);
    steps.forEach((s, i) => {
      const dot = document.createElement('span');
      const isDone = i < currentIdx;
      const isCurrent = i === currentIdx;
      dot.textContent = isDone ? '✓' : String(i + 1);
      dot.style.cssText = `display:inline-flex;align-items:center;justify-content:center;width:28px;height:28px;border-radius:50%;font-size:13px;font-weight:700;${
        isDone ? 'background:#16a34a;color:white;' : isCurrent ? 'background:#2563eb;color:white;' : 'background:#e5e7eb;color:#666;'
      }`;
      dot.setAttribute('aria-label', `${labels[s]} ${isDone ? 'done' : isCurrent ? 'current' : 'pending'}`);
      bar.appendChild(dot);
      if (i < steps.length - 1) {
        const sep = document.createElement('span');
        sep.style.cssText = 'flex:1;height:2px;background:#e5e7eb;align-self:center;';
        sep.setAttribute('aria-hidden', 'true');
        bar.appendChild(sep);
      }
    });
    return bar;
  }

  private async repoStep(handlers: WizardHandlers): Promise<HTMLElement> {
    const wrap = document.createElement('div');
    wrap.innerHTML = `<h2 style="margin:0 0 16px;">Step 1: Scan repository</h2>
      <p style="color:#666;">Checking repository read/write access and Spec Kit setup…</p>`;
    try {
      this.scan = await handlers.scanRepo();
      const status = document.createElement('div');
      status.style.cssText = 'padding:12px;border-radius:6px;margin:12px 0;';
      if (this.scan.writable) {
        status.style.background = '#f0fdf4';
        status.innerHTML = `<p>✓ Repository writable: <code>${this.scan.repo_root}</code></p>`;
      } else {
        status.style.background = '#fef2f2';
        status.innerHTML = `<p>✗ Repository not writable.</p>`;
      }
      if (this.scan.setup_gaps.length > 0) {
        const gaps = document.createElement('ul');
        gaps.style.cssText = 'color:#d97706;';
        this.scan.setup_gaps.forEach((g) => {
          const li = document.createElement('li');
          li.textContent = g;
          gaps.appendChild(li);
        });
        status.appendChild(gaps);
      }
      wrap.appendChild(status);

      const btn = document.createElement('button');
      btn.textContent = 'Continue →';
      btn.style.cssText = 'padding:8px 24px;background:#2563eb;color:white;border:none;border-radius:4px;cursor:pointer;';
      btn.disabled = !this.scan.writable;
      btn.addEventListener('click', () => {
        this.step = 'speckit';
        this.render(handlers);
      });
      wrap.appendChild(btn);
    } catch (e) {
      wrap.innerHTML = `<p style="color:#dc2626;">Failed to scan: ${escapeHtml(String(e))}</p>`;
    }
    return wrap;
  }

  private speckitStep(handlers: WizardHandlers): HTMLElement {
    const wrap = document.createElement('div');
    const gaps = this.scan?.setup_gaps ?? [];
    wrap.innerHTML = `<h2 style="margin:0 0 16px;">Step 2: Spec Kit check</h2>`;
    if (gaps.length === 0) {
      wrap.innerHTML += `<p>✓ Spec Kit setup looks complete.</p>`;
    } else {
      wrap.innerHTML += `<p style="color:#d97706;">⚠ Gaps detected — these will be addressed on commit:</p><ul>${gaps.map((g) => `<li>${escapeHtml(g)}</li>`).join('')}</ul>`;
    }
    const btn = document.createElement('button');
    btn.textContent = 'Continue →';
    btn.style.cssText = 'padding:8px 24px;background:#2563eb;color:white;border:none;border-radius:4px;cursor:pointer;';
    btn.addEventListener('click', () => {
      this.step = 'branch';
      this.render(handlers);
    });
    wrap.appendChild(btn);
    return wrap;
  }

  private branchStep(handlers: WizardHandlers): HTMLElement {
    const wrap = document.createElement('div');
    wrap.innerHTML = `<h2 style="margin:0 0 16px;">Step 3: Branch</h2>
      <p style="color:#666;">A feature branch will be created automatically from the feature id.</p>`;
    const btn = document.createElement('button');
    btn.textContent = 'Continue →';
    btn.style.cssText = 'padding:8px 24px;background:#2563eb;color:white;border:none;border-radius:4px;cursor:pointer;';
    btn.addEventListener('click', () => {
      this.step = 'brief';
      this.render(handlers);
    });
    wrap.appendChild(btn);
    return wrap;
  }

  private briefStep(handlers: WizardHandlers): HTMLElement {
    const wrap = document.createElement('div');
    wrap.innerHTML = `<h2 style="margin:0 0 16px;">Step 4: Feature brief</h2>
      <label for="brief" style="display:block;margin-bottom:8px;font-weight:600;">Describe the feature in one line:</label>`;
    const input = document.createElement('input');
    input.id = 'brief';
    input.type = 'text';
    input.value = this.brief;
    input.placeholder = 'e.g. Visual task board for Spec Kit';
    input.style.cssText = 'width:100%;padding:8px;border:1px solid #ccc;border-radius:4px;font-size:14px;';
    input.setAttribute('aria-label', 'Feature brief');
    input.addEventListener('input', () => {
      this.brief = input.value;
    });
    wrap.appendChild(input);

    const btn = document.createElement('button');
    btn.textContent = 'Preview →';
    btn.style.cssText = 'margin-top:16px;padding:8px 24px;background:#2563eb;color:white;border:none;border-radius:4px;cursor:pointer;';
    btn.disabled = !this.brief.trim();
    input.addEventListener('input', () => {
      btn.disabled = !this.brief.trim();
    });
    btn.addEventListener('click', async () => {
      this.preview = await handlers.preview(this.brief);
      this.step = 'preview';
      this.render(handlers);
    });
    wrap.appendChild(btn);
    return wrap;
  }

  private previewStep(handlers: WizardHandlers): HTMLElement {
    const wrap = document.createElement('div');
    const p = this.preview;
    if (!p) {
      wrap.innerHTML = '<p>No preview available.</p>';
      return wrap;
    }
    wrap.innerHTML = `<h2 style="margin:0 0 16px;">Step 5: Preview</h2>
      <p style="color:#666;">Review the proposed setup. Nothing is written yet (staged mode).</p>
      <dl style="background:#f9fafb;padding:12px;border-radius:6px;margin:12px 0;">
        <dt style="font-weight:600;">Feature ID</dt><dd><code>${escapeHtml(p.feature_id)}</code></dd>
        <dt style="font-weight:600;">Branch</dt><dd><code>${escapeHtml(p.branch)}</code></dd>
        <dt style="font-weight:600;">Paths</dt><dd><code>${p.paths.map((x) => escapeHtml(x)).join('<br>')}</code></dd>
      </dl>
      <p style="color:#16a34a;font-weight:600;">✓ Staged mode — nothing is committed until you confirm.</p>`;

    const confirmBtn = document.createElement('button');
    confirmBtn.textContent = 'Confirm & create (staged)';
    confirmBtn.style.cssText = 'margin-right:8px;padding:8px 24px;background:#16a34a;color:white;border:none;border-radius:4px;cursor:pointer;font-weight:600;';
    confirmBtn.setAttribute('aria-label', 'Confirm and create the feature in staged mode');
    confirmBtn.addEventListener('click', async () => {
      await handlers.commit(p.feature_id, this.brief);
      handlers.onComplete?.(p.feature_id);
    });
    wrap.appendChild(confirmBtn);

    const backBtn = document.createElement('button');
    backBtn.textContent = '← Back';
    backBtn.style.cssText = 'padding:8px 24px;background:#e5e7eb;border:none;border-radius:4px;cursor:pointer;';
    backBtn.addEventListener('click', () => {
      this.step = 'brief';
      this.render(handlers);
    });
    wrap.appendChild(backBtn);

    return wrap;
  }
}

function escapeHtml(s: string): string {
  return s.replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]!));
}
