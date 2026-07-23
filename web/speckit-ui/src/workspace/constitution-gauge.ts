// Constitution Compliance gauge (FR-016): green/red indicator driven by
// plan.constitution_gates and/or the latest /analyze result.

import type { ConstitutionGate } from '../api-client';

export interface ConstitutionGaugeOptions {
  container: HTMLElement;
}

export class ConstitutionGauge {
  private readonly container: HTMLElement;

  constructor(opts: ConstitutionGaugeOptions) {
    this.container = opts.container;
    this.container.setAttribute('data-testid', 'constitution-gauge');
    this.renderUnknown();
  }

  private renderUnknown(): void {
    this.container.innerHTML = '';
    this.container.setAttribute('data-status', 'unknown');
    this.container.style.background = '#616161';
    this.container.textContent = 'Constitution: unknown';
  }

  /** Derives Pass/Fail from a plan's constitution_gates table. */
  setFromGates(gates: ConstitutionGate[]): void {
    const pass = gates.length > 0 && gates.every((g) => g.result === 'Pass');
    this.setStatus(pass ? 'Pass' : 'Fail');
  }

  /** Derives Pass/Fail from the latest /analyze result — this takes
   * precedence as the more current signal once available. */
  setFromAnalyze(constitutionCompliance: 'Pass' | 'Fail'): void {
    this.setStatus(constitutionCompliance);
  }

  private setStatus(status: 'Pass' | 'Fail'): void {
    this.container.innerHTML = '';
    this.container.setAttribute('data-status', status);
    this.container.style.background = status === 'Pass' ? '#2e7d32' : '#c62828';
    this.container.style.color = '#fff';
    this.container.textContent = status === 'Pass' ? 'Constitution: Pass' : 'Constitution: Fail';
  }
}
