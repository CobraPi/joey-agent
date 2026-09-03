# Contract: Public API Additions

All additions are new public items; no existing public item changes signature, meaning, or default behavior (constitution Principle VII). Crate-internal wiring elided.

## joey-neurocode
- `pub trait EnterpriseTaskAnalyzer: Send + Sync` — `fn analyze(&self, request: &CodingRequest) -> TaskAnalysis;` `fn context_for(&self, task: &TaskNode) -> TaskContext;` `fn verification_for(&self, task: &TaskNode) -> VerificationPlan;` `fn record_outcome(&self, outcome: &VerifiedOutcome);` (implemented by `DefaultEngine`).
- `pub struct TaskAnalysis` — fields per data-model.md.
- `pub struct PolicyBinding`, `pub enum PolicyLayer`, `pub struct RiskAssessment`, `pub enum RiskLevel`, `pub enum RiskFactorKind`, `pub struct ExecutionHint`, `pub struct VerificationPlan`, `pub struct VerificationStep`, `pub struct OutcomeMemory`, `pub struct VerifiedOutcome`.
- New modules: `analysis`, `policy`, `risk` (module), `verification_plan`, `memory::outcomes`.
- `pub fn ComplexityTier::rank(&self) -> u8` (additive ordering helper for the escalation ladder; no trait impls added to existing types).
- Classifier: existing `classify` signature unchanged; `SignalKind` variants unchanged; new internal signals only enrich `ComplexityRoute::signals`.

## joey-orchestration
- New modules: `task_graph`, `scheduler`, `workspace`, `evaluator`, `joiner`, `evidence`.
- `pub struct TaskNode`, `pub type TaskId`, `pub enum TaskStatus`, `pub enum IsolationMode`, `pub enum WorkerRole` (re-exported alignment), `pub struct AcceptanceCriterion`, `pub struct TaskGraph` (`validate`, `ready_nodes`, `is_terminal`, `is_blocked`, `transition`, `snapshot`, `from_workstreams`, `from_strict_json`).
- `pub trait VerificationGate: Send + Sync` — `async fn run(&self, plan: &VerificationPlanView, workdir: &Path) -> GateOutcome;` with `GateOutcome::{Passed, Failed(DefectBundle), Degraded}`. Pure orchestration-side types; no neurocode imports.
- `pub struct Scheduler`, `pub struct SchedulerConfig { max_concurrent_workers: usize, max_repair_attempts: u32 }`, `pub struct ConflictAnalyzer` (write-set partitioning), `pub struct ChangeBundle`, `pub struct DefectBundle`, `pub struct CommandFailure`, `pub struct EvidenceRecord`, `pub struct RunHandle` (resume entry point).
- `pub struct WorkspaceIsolation` — `fn prepare(&self, task: &TaskNode) -> std::io::Result<IsolatedWorkspace>` (git worktree add --detach; copy fallback), `fn cleanup(...)`.
- `pub struct Joiner` — `fn integrate(&self, bundles: &[ChangeBundle]) -> Result<IntegrationReport, IntegrationConflict>`; conflicts are surfaced before any application (three-way check).

## joey-cli
- `hypercode.rs`: existing `Workstream`, `parse_workstreams`, `ModeRoute::{Subagent, Team}`, `route_mode` unchanged. Additive: `ModeRoute::{SingleWorker, DagSubagents, ParallelSubagents}` variants and `pub fn route_mode_from_graph(hint: &ExecutionHint) -> ModeRoute`; workstream→TaskGraph conversion; `VerifyLoop`→`VerificationGate` adapter; completion-gate await in the hypercode pipeline when `hypercode.execution_graph.enabled`.

## joey-core
- `config.rs`: default config text gains the keys in config-keys.md; `_config_version` → 34. No accessor changes.

## Compatibility statement
Flag-off behavior is identical to the current release across all touched surfaces (SC-001). `Workstream` parsing and the legacy router remain until the flags default on; their removal would be a separately versioned breaking change.
