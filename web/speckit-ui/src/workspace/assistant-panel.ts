// Assistant panel: chat-style QA box that drives /clarify and /analyze.

import type { AnalysisFinding, ClarifySessionEvent, SpeckitApiClient } from '../api-client';
import type { DocumentPane } from './document-pane';

export interface AssistantPanelOptions {
  container: HTMLElement;
  client: SpeckitApiClient;
  featureId: string;
  documentPane: DocumentPane;
  onFindings?: (findings: AnalysisFinding[], constitutionCompliance: 'Pass' | 'Fail') => void;
}

interface ChatMessage {
  role: 'system' | 'question' | 'answer' | 'finding';
  text: string;
}

export class AssistantPanel {
  private readonly container: HTMLElement;
  private readonly client: SpeckitApiClient;
  private readonly featureId: string;
  private readonly documentPane: DocumentPane;
  private readonly onFindings?: (
    findings: AnalysisFinding[],
    constitutionCompliance: 'Pass' | 'Fail',
  ) => void;

  private messages: ChatMessage[] = [];
  private sessionId: string | null = null;
  private unwatch: (() => void) | null = null;

  constructor(opts: AssistantPanelOptions) {
    this.container = opts.container;
    this.client = opts.client;
    this.featureId = opts.featureId;
    this.documentPane = opts.documentPane;
    this.onFindings = opts.onFindings;
    this.container.setAttribute('data-testid', 'assistant-panel');
    this.render();
  }

  private render(): void {
    this.container.innerHTML = '';

    const log = document.createElement('div');
    log.className = 'assistant-log';
    log.setAttribute('data-testid', 'assistant-log');
    for (const msg of this.messages) {
      const el = document.createElement('div');
      el.className = `assistant-msg assistant-msg--${msg.role}`;
      el.setAttribute('data-testid', `assistant-msg-${msg.role}`);
      el.textContent = msg.text;
      log.appendChild(el);
    }
    this.container.appendChild(log);

    const controls = document.createElement('div');
    controls.className = 'assistant-controls';

    const clarifyBtn = document.createElement('button');
    clarifyBtn.textContent = 'Run /speckit-clarify';
    clarifyBtn.setAttribute('data-testid', 'run-clarify');
    clarifyBtn.addEventListener('click', () => void this.startClarify());
    controls.appendChild(clarifyBtn);

    const analyzeBtn = document.createElement('button');
    analyzeBtn.textContent = 'Run /speckit-analyze';
    analyzeBtn.setAttribute('data-testid', 'run-analyze');
    analyzeBtn.addEventListener('click', () => void this.runAnalyze());
    controls.appendChild(analyzeBtn);

    const answerInput = document.createElement('input');
    answerInput.type = 'text';
    answerInput.placeholder = 'Type your answer…';
    answerInput.setAttribute('data-testid', 'answer-input');
    answerInput.disabled = !this.sessionId;
    controls.appendChild(answerInput);

    const answerBtn = document.createElement('button');
    answerBtn.textContent = 'Submit answer';
    answerBtn.setAttribute('data-testid', 'submit-answer');
    answerBtn.disabled = !this.sessionId;
    answerBtn.addEventListener('click', () => {
      if (answerInput.value.trim()) {
        void this.submitAnswer(answerInput.value.trim());
        answerInput.value = '';
      }
    });
    controls.appendChild(answerBtn);

    this.container.appendChild(controls);
  }

  private pushMessage(msg: ChatMessage): void {
    this.messages.push(msg);
    this.render();
  }

  async startClarify(): Promise<void> {
    const { session_id } = await this.client.startClarify(this.featureId);
    this.sessionId = session_id;
    this.pushMessage({ role: 'system', text: `Clarify session started (${session_id})` });

    this.unwatch?.();
    this.unwatch = this.client.watchClarifySession(this.featureId, session_id, (evt) =>
      this.handleClarifyEvent(evt),
    );
  }

  private handleClarifyEvent(evt: ClarifySessionEvent): void {
    if (evt.type === 'question') {
      this.pushMessage({ role: 'question', text: evt.question });
      if (evt.target_line) {
        this.documentPane.scrollToAnchor(evt.target_line);
      }
    } else if (evt.type === 'done') {
      this.pushMessage({ role: 'system', text: 'Clarification session complete.' });
      this.sessionId = null;
      this.unwatch?.();
      this.unwatch = null;
    }
  }

  async submitAnswer(answer: string): Promise<void> {
    if (!this.sessionId) return;
    this.pushMessage({ role: 'answer', text: answer });
    const result = await this.client.answerClarify(this.featureId, this.sessionId, answer);
    // Per contract, `updated_line` identifies the resolved marker; the
    // document pane should scroll to/highlight the replaced text.
    this.documentPane.scrollToAnchor(result.updated_line);
  }

  async runAnalyze(): Promise<void> {
    const result = await this.client.analyze(this.featureId);
    this.pushMessage({
      role: 'system',
      text: `Analyze complete: ${result.findings.length} finding(s), constitution=${result.constitution_compliance}`,
    });
    for (const finding of result.findings) {
      this.pushMessage({
        role: 'finding',
        text: `[${finding.severity}] ${finding.target_file}#${finding.target_line_or_section}: ${finding.description}`,
      });
    }
    this.onFindings?.(result.findings, result.constitution_compliance);
  }

  destroy(): void {
    this.unwatch?.();
    this.unwatch = null;
  }
}
