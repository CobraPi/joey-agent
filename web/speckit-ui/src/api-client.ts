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

  /** Public read-only access to the base URL (for extension clients). */
  getBase_url(): string {
    return this.baseUrl;
  }

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

  // ===================================================================
  // Feature 010: Spec-Kit Development IDE
  // ===================================================================

  async getArtifacts(id: string): Promise<Artifact[]> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/artifacts`,
    );
    const body = await parseJsonOrThrow<{ artifacts: Artifact[] }>(res);
    return body.artifacts;
  }

  async getArtifact(id: string, path: string): Promise<ArtifactContent> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/artifacts/${encodeURIComponent(path)}`,
    );
    return parseJsonOrThrow<ArtifactContent>(res);
  }

  async patchArtifact(
    id: string,
    path: string,
    req: PatchArtifactRequest,
  ): Promise<PatchResponse> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/artifacts/${encodeURIComponent(path)}`,
      {
        method: 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(req),
      },
    );
    return parseJsonOrThrow<PatchResponse>(res);
  }

  async getWorkflow(id: string): Promise<WorkflowResponse> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/workflow`,
    );
    return parseJsonOrThrow<WorkflowResponse>(res);
  }

  async getOptions(): Promise<OptionsCatalog> {
    const res = await this.fetchImpl(`${this.baseUrl}/api/options`);
    return parseJsonOrThrow<OptionsCatalog>(res);
  }

  async getStepConfig(id: string, step: string): Promise<StepConfig> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/workflow/${encodeURIComponent(step)}/config`,
    );
    return parseJsonOrThrow<StepConfig>(res);
  }

  async putOverride(id: string, step: string, instructions: string): Promise<{ override_id: string }> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/workflow/${encodeURIComponent(step)}/override`,
      {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ instructions }),
      },
    );
    return parseJsonOrThrow(res);
  }

  async deleteOverride(id: string, step: string): Promise<void> {
    await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/workflow/${encodeURIComponent(step)}/override`,
      { method: 'DELETE' },
    );
  }

  async runWorkflowStep(id: string, step: string, req: RunRequest): Promise<RunStartResponse> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/workflow/${encodeURIComponent(step)}/run`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(req),
      },
    );
    return parseJsonOrThrow<RunStartResponse>(res);
  }

  async answerAttempt(attemptId: string, interactionId: string, answer: string): Promise<{ confirmed: boolean }> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/attempts/${encodeURIComponent(attemptId)}/answer`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ interaction_id: interactionId, answer }),
      },
    );
    return parseJsonOrThrow(res);
  }

  async approveAttempt(attemptId: string, interactionId: string, decision: 'approve' | 'reject', note?: string): Promise<{ confirmed: boolean }> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/attempts/${encodeURIComponent(attemptId)}/approve`,
      {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ interaction_id: interactionId, decision, note }),
      },
    );
    return parseJsonOrThrow(res);
  }

  async cancelAttempt(attemptId: string): Promise<void> {
    await this.fetchImpl(
      `${this.baseUrl}/api/attempts/${encodeURIComponent(attemptId)}/cancel`,
      { method: 'POST' },
    );
  }

  async getAttemptChanges(attemptId: string): Promise<ChangeSetResponse> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/attempts/${encodeURIComponent(attemptId)}/changes`,
    );
    return parseJsonOrThrow<ChangeSetResponse>(res);
  }

  async getHistory(id: string, limit?: number, before?: string): Promise<HistoryResponse> {
    const params = new URLSearchParams();
    if (limit) params.set('limit', String(limit));
    if (before) params.set('before', before);
    const qs = params.toString();
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/history${qs ? `?${qs}` : ''}`,
    );
    return parseJsonOrThrow<HistoryResponse>(res);
  }

  async getPreferences(id: string): Promise<WorkspacePreference> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/preferences`,
    );
    return parseJsonOrThrow<WorkspacePreference>(res);
  }

  async putPreferences(id: string, prefs: WorkspacePreference): Promise<WorkspacePreference> {
    const res = await this.fetchImpl(
      `${this.baseUrl}/api/features/${encodeURIComponent(id)}/preferences`,
      {
        method: 'PUT',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(prefs),
      },
    );
    return parseJsonOrThrow<WorkspacePreference>(res);
  }

  async getHealth(): Promise<HealthStatus> {
    const res = await this.fetchImpl(`${this.baseUrl}/api/health`);
    return parseJsonOrThrow<HealthStatus>(res);
  }

  /** Subscribe to an attempt's live run/interaction event stream. */
  watchAttempt(attemptId: string, onEvent: (evt: RunnerEvent) => void): () => void {
    const ws = new this.WebSocketImpl(
      `${this.wsBaseUrl}/api/attempts/${encodeURIComponent(attemptId)}/stream`,
    );
    ws.addEventListener('message', (ev: MessageEvent) => {
      const data = JSON.parse(String(ev.data)) as RunnerEvent;
      onEvent(data);
    });
    return () => ws.close();
  }
}

// --- Feature 010 types ---

export type ArtifactKind = 'spec' | 'plan' | 'tasks' | 'checklist' | 'research' | 'data_model' | 'contract' | 'quickstart' | 'constitution' | 'supporting';
export type SaveState = 'clean' | 'dirty' | 'saving' | 'saved' | 'invalid' | 'externally_changed' | 'read_only';
export type StepState = 'ready' | 'blocked' | 'running' | 'attention_needed' | 'succeeded' | 'failed' | 'stale' | 'unavailable';
export type ChangeMode = 'staged' | 'direct';

export interface ValidationFinding {
  finding_id: string;
  severity: 'Info' | 'Warning' | 'Critical';
  code: string;
  description: string;
  location: { path: string; line_or_section: string };
  remediation?: string;
}

export interface Artifact {
  path: string;
  kind: ArtifactKind;
  exists: boolean;
  content_hash: string | null;
  dirty: boolean;
  save_state: SaveState;
  validity: ValidationFinding[];
  workflow_phase: string;
  stale: boolean;
  stale_reason?: string;
}

export interface OutlineEntry {
  title: string;
  line: number;
  level: number;
}

export interface ArtifactContent {
  path: string;
  kind: ArtifactKind;
  text: string;
  content_hash: string;
  outline: OutlineEntry[];
  save_state: SaveState;
  validity: ValidationFinding[];
}

export interface PatchArtifactRequest {
  new_text: string;
  based_on_hash: string;
  scope?: { whole?: boolean } | { section: string };
}

export interface WorkflowStep {
  id: string;
  order: number;
  purpose: string;
  inputs: { path: string; kind?: ArtifactKind }[];
  outputs: { path: string; kind?: ArtifactKind }[];
  prerequisites: string[];
  available: boolean;
  state: StepState;
  blocking_reason: string | null;
  latest_attempt_id: string | null;
  installed_definition_ref: string;
}

export interface WorkflowResponse {
  steps: WorkflowStep[];
}

export interface OptionsCatalog {
  revision: string;
  models: string[];
  reasoning_efforts: string[];
  max_iterations: { min: number; max: number; default: number };
}

export interface StepConfig {
  step_id: string;
  installed: { instructions: string };
  override: { override_id: string; instructions: string } | null;
  effective_instructions: string;
}

export interface RunRequest {
  effective_instructions?: string;
  scope?: { targets: { path: string }[]; task_ids?: string[] };
  options?: { model?: string; reasoning_effort?: string; max_iterations?: number };
  option_catalog_rev: string;
  change_mode?: ChangeMode;
  override_id?: string;
  prior_attempt_id?: string;
}

export interface RunStartResponse {
  attempt_id: string;
  ws: string;
}

export interface ChangeSetResponse {
  attempt_id: string;
  mode: ChangeMode;
  recovery_action: string | null;
  files: ChangedFile[];
}

export interface ChangedFile {
  path: string;
  status: 'added' | 'modified' | 'removed';
  additions: number;
  removals: number;
  why?: string;
  accept_state: 'pending' | 'accepted' | 'rejected';
  hunks: Hunk[];
}

export interface Hunk {
  hunk_id: string;
  old_range: string;
  new_range: string;
  accept_state: 'pending' | 'accepted' | 'rejected';
  depends_on: string[];
}

export interface HistoryAttempt {
  attempt_id: string;
  step_id: string;
  status: string;
  started_at: string;
  ended_at: string | null;
  prior_attempt_id: string | null;
  changes_count: number;
}

export interface HistoryResponse {
  attempts: HistoryAttempt[];
  next_cursor: string | null;
}

export interface WorkspacePreference {
  last_feature_id?: string;
  open_artifacts?: string[];
  active_view?: string;
  pane_layout?: unknown;
  filters?: unknown;
}

export interface HealthStatus {
  backend_reachable: boolean;
  agent_binary_discovered: boolean;
  credentials_present: boolean;
  repo_writable: boolean;
  read_only: boolean;
}

// =====================================================================
// Feature 012: Spec Studio — Atlas, stage-bar, setup, recovery (T037).
// Additive over specs/001/010.
// =====================================================================

export interface RepoScan {
  repo_root: string;
  exists: boolean;
  writable: boolean;
  has_specs_dir: boolean;
  has_specify_dir: boolean;
  setup_gaps: string[];
}

export interface SetupPreview {
  feature_id: string;
  branch: string;
  paths: string[];
  staged_mode: boolean;
  nothing_written: boolean;
}

export interface SetupCommitResult {
  feature_id: string;
  created_paths: string[];
  staged: boolean;
}

export type NextAction =
  | { action: 'unblock'; step_id: string; reason: string }
  | { action: 'refresh'; step_id: string }
  | { action: 'recover'; step_id: string }
  | { action: 'run'; step_id: string }
  | { action: 'all_done' };

export interface AtlasResponse {
  feature_id: string;
  next_action: NextAction;
  progress: { done_tasks: number; total_tasks: number; ratio: number };
  health: { parsing_ok: boolean; open_unknowns: number; orphan_count: number };
  branch: { name: string | null; drift: boolean };
  artifacts: Array<{ path: string; exists: boolean }>;
  recent_activity: Array<{ record_type: string; feature_id: string }>;
}

export interface StageBarResponse {
  feature_id: string;
  stages: Array<{
    name: string;
    state: 'pending' | 'ready' | 'active' | 'done' | 'blocked';
    gate_reason: string | null;
    step_ids: string[];
  }>;
}

export interface RecoveryStatesResponse {
  feature_id: string;
  recovery_states: Array<{
    state: string;
    description: string;
    primary_action: string;
    step_id?: string;
    touches_files: string[];
  }>;
}

/** A patch operation compiled by an editing widget (FR-014). */
export interface PatchOp {
  op: 'replace' | 'insert_after' | 'delete';
  node: number;
  new_bytes?: string;
}

/** Extension methods for the Spec Studio (012) endpoints. */
export class SpecStudioClient {
  constructor(private base: SpeckitApiClient) {}

  private url(path: string): string {
    return `${this.base.getBase_url()}${path}`;
  }

  async scanRepo(): Promise<RepoScan> {
    const res = await fetch(this.url('/api/setup/scan-repo'));
    return parseJsonOrThrow<RepoScan>(res);
  }

  async previewSetup(brief: string): Promise<SetupPreview> {
    const res = await fetch(this.url('/api/setup/preview'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ brief }),
    });
    return parseJsonOrThrow<SetupPreview>(res);
  }

  async commitSetup(featureId: string, brief: string): Promise<SetupCommitResult> {
    const res = await fetch(this.url('/api/setup/commit'), {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ feature_id: featureId, brief }),
    });
    return parseJsonOrThrow<SetupCommitResult>(res);
  }

  async getAtlas(featureId: string): Promise<AtlasResponse> {
    const res = await fetch(this.url(`/api/features/${featureId}/atlas`));
    return parseJsonOrThrow<AtlasResponse>(res);
  }

  async getStageBar(featureId: string): Promise<StageBarResponse> {
    const res = await fetch(this.url(`/api/features/${featureId}/stage-bar`));
    return parseJsonOrThrow<StageBarResponse>(res);
  }

  async getRecoveryStates(featureId: string): Promise<RecoveryStatesResponse> {
    const res = await fetch(this.url(`/api/features/${featureId}/recovery-states`));
    return parseJsonOrThrow<RecoveryStatesResponse>(res);
  }
}

export type RunnerEvent =
  | { type: 'progress'; attempt_id: string; text: string }
  | { type: 'tool'; attempt_id: string; name: string; summary: string }
  | { type: 'question'; attempt_id: string; interaction_id: string; prompt: string; choices?: string[] }
  | { type: 'approval'; attempt_id: string; interaction_id: string; impact: string; boundary: string }
  | { type: 'output'; attempt_id: string; file: string; added: number; removed: number }
  | { type: 'status'; attempt_id: string; terminal: 'succeeded' | 'failed' | 'cancelled'; duration_ms: number }
  | { type: 'error'; attempt_id: string; message: string; recoverable: boolean };
