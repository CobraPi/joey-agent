// Shared types + typed API client for the SpecKit Visual UI backend.
// Matches specs/001-speckit-visual-ui/contracts/speckit-ui-api.md exactly.

export type Status = 'Draft' | 'InProgress' | 'Completed' | 'Unparsed';

export interface FeatureSummary {
  id: string;
  title: string;
  status: Status;
}

export interface ClarificationEntry {
  id: string;
  question: string;
  answer: string | null;
}

export interface Specification {
  title: string;
  status: Status;
  user_stories: UserStory[];
  functional_requirements: string[];
  clarifications: ClarificationEntry[];
  content_hash: string;
}

export interface UserStory {
  id: string;
  title: string;
  priority: string;
  acceptance_scenarios: string[];
}

export interface ConstitutionGate {
  principle: string;
  result: 'Pass' | 'Fail';
  notes: string;
}

export interface Plan {
  summary: string;
  technical_context: string;
  constitution_gates: ConstitutionGate[];
  content_hash: string;
}

export interface Task {
  id: string;
  description: string;
  status: 'Todo' | 'InProgress' | 'Completed' | 'Unparsed';
  parallel_eligible: boolean;
  target_files: string[];
  user_story_ref: string | null;
}

export interface FeatureDetail {
  id: string;
  spec: Specification | null;
  plan: Plan | null;
  tasks: Task[];
  tasks_content_hash: string | null;
  missing?: Array<'plan' | 'tasks'>;
}

export interface ApiErrorBody {
  error: 'not_found' | 'conflict' | 'invalid_request' | 'internal_error' | string;
  message: string;
  current_hash?: string;
}

export class ApiError extends Error {
  readonly code: string;
  readonly current_hash?: string;
  readonly status: number;

  constructor(status: number, body: ApiErrorBody) {
    super(body.message);
    this.name = 'ApiError';
    this.status = status;
    this.code = body.error;
    this.current_hash = body.current_hash;
  }

  get isConflict(): boolean {
    return this.code === 'conflict';
  }
}

export interface PatchSpecRequest {
  target: { type: string; id: string };
  new_text: string;
  based_on_hash: string;
}

export interface PatchResponse {
  content_hash: string;
  [key: string]: unknown;
}

export interface AnalysisFinding {
  target_file: string;
  target_line_or_section: string;
  description: string;
  severity: 'Info' | 'Warning' | 'Critical';
}

export interface AnalyzeResponse {
  findings: AnalysisFinding[];
  constitution_compliance: 'Pass' | 'Fail';
}

export interface ClarifySessionStart {
  session_id: string;
}

export interface ClarifyAnswerResponse {
  updated_line: string;
  spec_content_hash: string;
}

export interface ExecuteResponse {
  run_id: string;
}

export interface InitRequest {
  integration: string;
  script: string;
}

export interface InitResponse {
  success: boolean;
  output: string;
}

export type WatchEvent = {
  type: 'file_changed';
  file: 'spec.md' | 'plan.md' | 'tasks.md';
  content_hash: string;
};

/** Message shapes for the clarify session WebSocket. Backend contract leaves
 * exact framing loosely specified beyond "question/answer exchange"; we model
 * the minimal shape needed by the assistant panel. */
export interface ClarifyQuestionEvent {
  type: 'question';
  question: string;
  target_line?: string;
}

export interface ClarifyDoneEvent {
  type: 'done';
}

export type ClarifySessionEvent = ClarifyQuestionEvent | ClarifyDoneEvent;

/** Message shapes for the task run WebSocket. */
export interface RunOutputEvent {
  type: 'output';
  text: string;
}

export interface RunStatusEvent {
  type: 'status';
  status: 'succeeded' | 'failed';
}

export type RunEvent = RunOutputEvent | RunStatusEvent;

export interface SpeckitApiClientOptions {
  baseUrl?: string;
  wsBaseUrl?: string;
  fetchImpl?: typeof fetch;
  WebSocketImpl?: typeof WebSocket;
}

async function parseJsonOrThrow<T>(res: Response): Promise<T> {
  const text = await res.text();
  const body = text ? (JSON.parse(text) as unknown) : {};
  if (!res.ok) {
    throw new ApiError(res.status, body as ApiErrorBody);
  }
  return body as T;
}

export class SpeckitApiClient {
  private readonly baseUrl: string;
  private readonly wsBaseUrl: string;
  private readonly fetchImpl: typeof fetch;
  private readonly WebSocketImpl: typeof WebSocket;

  constructor(options: SpeckitApiClientOptions = {}) {
    this.baseUrl = options.baseUrl ?? '';
    this.wsBaseUrl =
      options.wsBaseUrl ??
      (typeof window !== 'undefined'
        ? `${window.location.protocol === 'https:' ? 'wss' : 'ws'}://${window.location.host}`
        : 'ws://127.0.0.1:8787');
    this.fetchImpl = options.fetchImpl ?? fetch.bind(globalThis);
    this.WebSocketImpl = options.WebSocketImpl ?? (globalThis.WebSocket as typeof WebSocket);
  }

  async listFeatures(): Promise<FeatureSummary[]> {
    const res = await this.fetchImpl(`${this.baseUrl}/api/features`);
    const body = await parseJsonOrThrow<{ features: FeatureSummary[] }>(res);
    return body.features;
  }

  async getFeature(id: string): Promise<FeatureDetail> {
    const res = await this.fetchImpl(`${this.baseUrl}/api/features/${encodeURIComponent(id)}`);
    return parseJsonOrThrow<FeatureDetail>(res);
  }

  /** PATCH spec.md. Throws ApiError with isConflict=true on 409 — caller MUST
   * surface a visible message and prompt reload; never silently retry/merge. */
  async patchSpec(id: string, req: PatchSpecRequest): Promise<PatchResponse> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/spec`,
      {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(req),
      },
    );
    return parseJsonOrThrow<PatchResponse>(res);
  }

  /** PATCH a single task's description. Same conflict semantics as patchSpec. */
  async patchTask(
    id: string,
    taskId: string,
    newText: string,
    basedOnHash: string,
  ): Promise<PatchResponse> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/tasks/${encodeURIComponent(taskId)}`,
      {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ new_text: newText, based_on_hash: basedOnHash }),
      },
    );
    return parseJsonOrThrow<PatchResponse>(res);
  }

  async startClarify(id: string): Promise<ClarifySessionStart> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/clarify`,
      { method: 'POST' },
    );
    return parseJsonOrThrow<ClarifySessionStart>(res);
  }

  async answerClarify(
    id: string,
    sessionId: string,
    answer: string,
  ): Promise<ClarifyAnswerResponse> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/clarify/${encodeURIComponent(sessionId)}/answer`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ answer }),
      },
    );
    return parseJsonOrThrow<ClarifyAnswerResponse>(res);
  }

  async analyze(id: string): Promise<AnalyzeResponse> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/analyze`,
      { method: 'POST' },
    );
    return parseJsonOrThrow<AnalyzeResponse>(res);
  }

  /** Execute exactly ONE task. Never call this for more than one taskId in
   * response to a single user click — the backend contract and the product
   * Clarifications explicitly forbid cascading execution. */
  async executeTask(id: string, taskId: string): Promise<ExecuteResponse> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/tasks/${encodeURIComponent(taskId)}/execute`,
      { method: 'POST' },
    );
    return parseJsonOrThrow<ExecuteResponse>(res);
  }

  async init(req: InitRequest): Promise<InitResponse> {
    const res = await this.fetchImpl(`${this.baseUrl}/api/init`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(req),
    });
    return parseJsonOrThrow<InitResponse>(res);
  }

  /** Subscribe to file-change events for a feature. Returns an unsubscribe fn. */
  watchFeature(id: string, onEvent: (evt: WatchEvent) => void): () => void {
    const ws = new this.WebSocketImpl(
      `${this.wsBaseUrl}/api/features/${encodeURIComponent(id)}/watch`,
    );
    ws.addEventListener('message', (ev: MessageEvent) => {
      const data = JSON.parse(String(ev.data)) as WatchEvent;
      onEvent(data);
    });
    return () => ws.close();
  }

  /** Subscribe to a clarify session's question/answer exchange. */
  watchClarifySession(
    id: string,
    sessionId: string,
    onEvent: (evt: ClarifySessionEvent) => void,
  ): () => void {
    const ws = new this.WebSocketImpl(
      `${this.wsBaseUrl}/api/features/${encodeURIComponent(id)}/session/${encodeURIComponent(sessionId)}`,
    );
    ws.addEventListener('message', (ev: MessageEvent) => {
      const data = JSON.parse(String(ev.data)) as ClarifySessionEvent;
      onEvent(data);
    });
    return () => ws.close();
  }

  /** Subscribe to a task run's live output + terminal status. */
  watchRun(runId: string, onEvent: (evt: RunEvent) => void): () => void {
    const ws = new this.WebSocketImpl(`${this.wsBaseUrl}/api/runs/${encodeURIComponent(runId)}`);
    ws.addEventListener('message', (ev: MessageEvent) => {
      const data = JSON.parse(String(ev.data)) as RunEvent;
      onEvent(data);
    });
    return () => ws.close();
  }
}
