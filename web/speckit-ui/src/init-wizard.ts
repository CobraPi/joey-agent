// Guided init wizard (T049): simple form calling POST /api/init.

import type { SpeckitApiClient } from './api-client';

export interface InitWizardOptions {
  container: HTMLElement;
  client: SpeckitApiClient;
}

const INTEGRATIONS = ['hermes', 'claude', 'copilot', 'codex', 'gemini'];
const SCRIPTS = ['sh', 'ps'];

export class InitWizard {
  private readonly container: HTMLElement;
  private readonly client: SpeckitApiClient;
  private outputEl!: HTMLElement;

  constructor(opts: InitWizardOptions) {
    this.container = opts.container;
    this.client = opts.client;
    this.render();
  }

  private render(): void {
    this.container.innerHTML = '';
    this.container.setAttribute('data-testid', 'init-wizard');

    const form = document.createElement('form');
    form.setAttribute('data-testid', 'init-form');

    const integrationSelect = document.createElement('select');
    integrationSelect.setAttribute('data-testid', 'init-integration');
    for (const opt of INTEGRATIONS) {
      const o = document.createElement('option');
      o.value = opt;
      o.textContent = opt;
      integrationSelect.appendChild(o);
    }

    const scriptSelect = document.createElement('select');
    scriptSelect.setAttribute('data-testid', 'init-script');
    for (const opt of SCRIPTS) {
      const o = document.createElement('option');
      o.value = opt;
      o.textContent = opt;
      scriptSelect.appendChild(o);
    }

    const submitBtn = document.createElement('button');
    submitBtn.type = 'submit';
    submitBtn.textContent = 'Initialize';
    submitBtn.setAttribute('data-testid', 'init-submit');

    form.appendChild(labeled('Integration', integrationSelect));
    form.appendChild(labeled('Script type', scriptSelect));
    form.appendChild(submitBtn);

    form.addEventListener('submit', (e) => {
      e.preventDefault();
      void this.submit(integrationSelect.value, scriptSelect.value);
    });

    this.container.appendChild(form);

    this.outputEl = document.createElement('pre');
    this.outputEl.setAttribute('data-testid', 'init-output');
    this.container.appendChild(this.outputEl);
  }

  private async submit(integration: string, script: string): Promise<void> {
    this.outputEl.textContent = 'Running…';
    try {
      const result = await this.client.init({ integration, script });
      this.outputEl.textContent = result.output;
      this.outputEl.setAttribute('data-success', String(result.success));
    } catch (err) {
      this.outputEl.textContent = `Error: ${(err as Error).message}`;
      this.outputEl.setAttribute('data-success', 'false');
    }
  }
}

function labeled(label: string, el: HTMLElement): HTMLElement {
  const wrap = document.createElement('label');
  wrap.textContent = `${label}: `;
  wrap.appendChild(el);
  return wrap;
}
