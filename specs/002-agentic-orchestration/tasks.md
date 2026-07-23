---

description: "Task list for agentic orchestration engine implementation"
---

# Tasks: Agentic Orchestration Engine

**Input**: Design documents from `/specs/002-agentic-orchestration/`

**Prerequisites**: plan.md (required), spec.md (required), research.md, data-model.md, contracts/

**Tests**: Unit and integration tests are included — the constitution (Principle IV) mandates tests alongside implementation for new crates, and the spec's Success Criteria require measurable verification.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- This is a Cargo workspace project. All paths are workspace-relative.
- New crate: `crates/joey-orchestration/`
- New tools: `crates/joey-tools/src/tools/`
- Modified files: `crates/joey-agent-core/src/events.rs`, `crates/joey-tools/src/builtins.rs`, `crates/joey-tools/src/toolsets.rs`, `Cargo.toml`

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Create the new orchestration crate and wire it into the workspace.

- [X] T001 Create `crates/joey-orchestration/Cargo.toml` with dependencies on joey-core, joey-providers, joey-tools, joey-agent-core, tokio, serde, async-trait, tracing
- [X] T002 Create `crates/joey-orchestration/src/lib.rs` with module declarations and public API exports (SubagentManager, DelegationRequest, DelegationResult, ManagerConfig, SubagentRole, TaskSpec)
- [X] T003 Add `joey-orchestration = { path = "crates/joey-orchestration" }` to `[workspace.dependencies]` in root `Cargo.toml`
- [X] T004 Verify `cargo build -p joey-orchestration` compiles in workspace root `Cargo.toml` (empty stubs are OK at this stage)

**Checkpoint**: New crate exists and compiles in the workspace.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented.

**CRITICAL**: No user story work can begin until this phase is complete.

- [X] T005 Add new AgentEvent variants to `crates/joey-agent-core/src/events.rs`: SubagentSpawn, SubagentComplete, SubagentFailed, DelegationBatchComplete (per data-model.md field definitions)
- [X] T006 [P] Implement `crates/joey-orchestration/src/manager.rs`: ManagerConfig struct with fields max_concurrent_children (3), max_concurrent_requests (5), max_spawn_depth (1), default_max_turns (50), default_persist (false); SubagentManager struct holding Arc<Semaphore>, parent ToolContext clone, event sender, depth counter
- [X] T007 [P] Implement `crates/joey-orchestration/src/subagent.rs`: SubagentRole enum (Leaf, Orchestrator), DelegationRequest struct, TaskSpec struct, DelegationResult struct with all fields per data-model.md
- [X] T008 Implement SubagentManager::new() constructor in `crates/joey-orchestration/src/manager.rs` that creates the Semaphore from max_concurrent_requests and stores config; SubagentManager::config() accessor
- [X] T009 Add unit tests in `crates/joey-orchestration/src/manager.rs`: ManagerConfig defaults are correct, Semaphore has correct permit count, depth tracking initializes to 0

**Checkpoint**: Foundation ready — orchestration types and event variants exist. User story implementation can now begin.

---

## Phase 3: User Story 1 - Parallel Task Delegation (Priority: P1) TARGET MVP

**Goal**: The agent can spawn multiple subagents in parallel, each with an isolated context, and collect their summaries.

**Independent Test**: Given three independent code-modification tasks in separate files, the agent spawns three parallel subagents via delegate_task, each completes its task, and the parent receives three summaries. Wall-clock time is closer to the slowest single subtask than the sum of all three.

### Implementation for User Story 1

- [X] T010 [P] [US1] Implement Subagent::new() in `crates/joey-orchestration/src/subagent.rs`: constructs a fresh Agent via Agent::new() with a new ToolContext (independent SessionState), filtered ToolRegistry (based on requested toolsets), and AgentConfig derived from the request (model, reasoning, max_turns overrides)
- [X] T011 [P] [US1] Implement Subagent::run() in `crates/joey-orchestration/src/subagent.rs`: calls Agent::run_turn(goal, event_tx), captures TurnResult, generates summary (final_text for natural stop, or MAX_ITERATIONS_SUMMARY_REQUEST prompt for budget exhaustion), records token_usage and wall_clock, returns DelegationResult
- [X] T012 [US1] Implement SubagentManager::dispatch_single() in `crates/joey-orchestration/src/manager.rs`: creates one Subagent, acquires semaphore permit via Transport wrapper, emits SubagentSpawn event, runs Subagent::run(), emits SubagentComplete/SubagentFailed, returns DelegationResult
- [X] T013 [US1] Implement SubagentManager::dispatch_batch() in `crates/joey-orchestration/src/manager.rs`: takes Vec<TaskSpec>, spawns each as a tokio::task via JoinSet, collects results as they complete (one failure does not cancel others), emits DelegationBatchComplete, returns Vec<DelegationResult>
- [X] T014 [US1] Implement Transport wrapper for semaphore permit acquisition in `crates/joey-orchestration/src/subagent.rs`: wraps ProviderClient (or a test Transport) to acquire a semaphore permit before each transport_call and drop it after — inject via Agent::set_transport_for_tests or a new public setter
- [X] T015 [US1] Implement the delegate_task tool in `crates/joey-tools/src/tools/delegation_tool.rs`: struct DelegateTask implementing the Tool trait; parse goal/context (single) or tasks array (batch) from args; construct DelegationRequest; call SubagentManager::dispatch_single or dispatch_batch; format DelegationResult(s) as text per contracts/delegation-tool.md return contract
- [X] T016 [US1] Register DelegateTask in `crates/joey-tools/src/builtins.rs` register_all() — note: needs access to SubagentManager; add a register_orchestration() function that takes a SubagentManager Arc and registers delegate_task
- [X] T017 [US1] Wire SubagentManager construction in `crates/joey-cli/src/repl.rs`, `crates/joey-cli/src/oneshot.rs`, and `crates/joey-cli/src/cron_cmd.rs`: after creating the Agent, build a SubagentManager from config, call register_orchestration on the registry, pass the registry to Agent::new
- [X] T018 [US1] Add `delegation.*` config keys to config resolution: max_concurrent_children, max_concurrent_requests, max_spawn_depth, default_max_turns, default_model in ManagerConfig::from_config() in `crates/joey-orchestration/src/manager.rs`

**Tests for User Story 1**

- [X] T019 [P] [US1] Write unit test in `crates/joey-orchestration/src/subagent.rs`: Subagent::new() creates an Agent with empty history (no parent leak), restricted toolset excludes non-requested tools
- [X] T020 [P] [US1] Write unit test in `crates/joey-orchestration/src/manager.rs`: dispatch_single with mocked Transport returns DelegationResult with correct goal, success=true, summary text, token_usage from mock, wall_clock > 0
- [X] T021 [P] [US1] Write integration test in `crates/joey-orchestration/tests/parallel_batch.rs`: dispatch_batch with 3 TaskSpecs and mocked Transport completes all 3, wall-clock < 1.5x slowest single (use mock with configurable delay)
- [X] T022 [P] [US1] Write integration test in `crates/joey-orchestration/tests/batch_resilience.rs`: when one of 3 subagents returns an error, the other 2 results are still delivered; DelegationBatchComplete reports succeeded=2, failed=1
- [X] T023 [P] [US1] Write unit test in `crates/joey-orchestration/src/subagent.rs`: Subagent with iteration budget of 2 (mocked Transport returns tool calls then stop) produces a summary via the budget-exhaustion finalizer prompt

**Checkpoint**: delegate_task tool works for single and batch delegation. All P1 acceptance scenarios pass.

---

## Phase 4: User Story 2 - Isolated Subagent Execution Context (Priority: P2)

**Goal**: Each subagent operates in a fully isolated context — its own history, toolset, working directory, and budget — with zero leakage to the parent.

**Independent Test**: A subagent is spawned to explore a codebase (reading 20 files, running searches). The parent's context window is unaffected — it receives only a paragraph-length summary.

### Implementation for User Story 2

- [X] T024 [P] [US2] Implement toolset filtering in `crates/joey-orchestration/src/subagent.rs`: when DelegationRequest.toolsets is non-empty, build a new ToolRegistry containing only tools from those toolsets; when role=Leaf, always exclude delegate_task; when role=Orchestrator and depth < max_spawn_depth, include delegate_task
- [X] T025 [US2] Implement per-subagent working directory support in `crates/joey-orchestration/src/subagent.rs`: when DelegationRequest includes a workdir override, construct the child ToolContext with that cwd instead of the parent's
- [X] T026 [US2] Add isolation verification to Subagent::run() in `crates/joey-orchestration/src/subagent.rs`: after run_turn completes, assert child.history() is independent — the returned DelegationResult.summary is the ONLY data that crosses to the parent; log a warning if summary exceeds 500 tokens

**Tests for User Story 2**

- [X] T027 [P] [US2] Write unit test in `crates/joey-orchestration/src/subagent.rs`: subagent with restricted toolsets=["file"] has only read_file, write_file, patch, search_files available; terminal, web_search, etc. are absent from tool_schemas()
- [X] T028 [P] [US2] Write unit test in `crates/joey-orchestration/src/subagent.rs`: Leaf-role subagent does not have delegate_task in its registry; Orchestrator-role subagent at depth 0 with max_spawn_depth=2 does have delegate_task
- [X] T029 [P] [US2] Write integration test in `crates/joey-orchestration/tests/isolation.rs`: parent agent spawns a subagent that reads 5 files; after delegation completes, parent's history contains only the tool call + summary result, not the subagent's intermediate read_file calls

**Checkpoint**: Subagent isolation is provably enforced. P2 acceptance scenarios pass.

---

## Phase 5: User Story 3 - Per-Subagent Model and Capability Selection (Priority: P3)

**Goal**: Different subagents in the same batch can use different model tiers and tool capabilities, with automatic selection and user-configurable defaults.

**Independent Test**: A delegation batch of 3 tasks runs with mixed model assignment (1 heavy, 2 light). Total token cost is lower than running all three on the heavy model.

### Implementation for User Story 3

- [X] T030 [P] [US3] Implement model resolution chain in `crates/joey-orchestration/src/subagent.rs`: per-TaskSpec.model > DelegationRequest.model > config delegation.default_model > parent AgentConfig.model; record the resolved model in DelegationResult.model
- [X] T031 [US3] Implement per-TaskSpec toolset override in `crates/joey-orchestration/src/subagent.rs`: each TaskSpec in a batch can specify its own toolsets array, overriding the batch-level DelegationRequest.toolsets
- [X] T032 [US3] Update dispatch_batch() in `crates/joey-orchestration/src/manager.rs` to emit SubagentSpawn events with each subagent's resolved model and toolset summary (for SC-009 traceability)

**Tests for User Story 3**

- [X] T033 [P] [US3] Write unit test in `crates/joey-orchestration/src/subagent.rs`: model resolution chain — TaskSpec.model wins over DelegationRequest.model wins over default_model wins over parent model
- [X] T034 [P] [US3] Write integration test in `crates/joey-orchestration/tests/model_selection.rs`: batch of 3 TaskSpecs with mixed models (1 explicit heavy, 2 explicit light) via mocked Transport; each DelegationResult records its assigned model; total token_usage < 3x heavy-model usage

**Checkpoint**: Per-subagent model and toolset selection works. P3 acceptance scenarios pass. INCREMENT 1 COMPLETE.

---

## Phase 6: User Story 4 - Session History Search (Priority: P4)

**Goal**: The agent can search past session history by keyword and retrieve message windows for context recall.

**Independent Test**: In a prior session, the agent and user established that the project uses a specific test framework. In a new session, the agent searches history, finds the prior decision, and applies it without re-asking.

### Implementation for User Story 4

- [X] T035 [P] [US4] Implement the session_search tool in `crates/joey-tools/src/tools/session_search_tool.rs`: struct SessionSearch implementing Tool trait; parse query, limit, optional session_id + around_message_id + window from args; call SessionDb::search() for keyword mode or SessionDb::messages() with window slicing for scroll mode; format results per contracts/session-search-tool.md
- [X] T036 [US4] Register SessionSearch in `crates/joey-tools/src/builtins.rs` register_all() — needs access to the session DB; add a register_session_tools() function that takes an Option<Arc<Mutex<SessionDb>>> and conditionally registers the tool (skip if None)
- [X] T037 [US4] Wire SessionDb into ToolContext in `crates/joey-tools/src/context.rs`: add optional session_db field to ContextInner (Option<Arc<Mutex<SessionDb>>>); add session_db() accessor; update ToolContext::new() to accept it (or add a builder method with_session_db())
- [X] T038 [US4] Update CLI agent construction in `crates/joey-cli/src/repl.rs`, `crates/joey-cli/src/oneshot.rs`, `crates/joey-cli/src/cron_cmd.rs`: pass the session DB to ToolContext when constructing the agent

**Tests for User Story 4**

- [X] T039 [P] [US4] Write unit test in `crates/joey-tools/src/tools/session_search_tool.rs`: search with in-memory SessionDb containing 3 sessions; query matches one; result includes correct session_id, snippet, role
- [X] T040 [P] [US4] Write unit test in `crates/joey-tools/src/tools/session_search_tool.rs`: scroll mode with session_id + around_message_id returns correct window of messages around the anchor
- [X] T041 [P] [US4] Write unit test in `crates/joey-tools/src/tools/session_search_tool.rs`: when FTS is unavailable (in-memory DB without FTS), returns graceful error envelope, not a panic

**Checkpoint**: Session search tool works. P4 acceptance scenarios pass.

---

## Phase 7: User Story 5 - User Clarification Protocol (Priority: P5)

**Goal**: The agent can ask the user a structured question with multiple-choice options when genuine ambiguity blocks progress.

**Independent Test**: Given an ambiguous task ("add authentication" with no method specified), the agent presents a structured question with options and waits for the user's choice.

### Implementation for User Story 5

- [X] T042 [P] [US5] Implement the clarify tool in `crates/joey-tools/src/tools/clarify_tool.rs`: struct Clarify implementing Tool trait; parse question and optional choices array; check ToolContext::interactive() — return error if non-interactive; in interactive mode, send a ClarifyRequest event and await response via a oneshot channel
- [X] T043 [US5] Add a clarification channel to ToolContext in `crates/joey-tools/src/context.rs`: optional `clarify_tx: Option<mpsc::UnboundedSender<ClarifyRequest>>` where ClarifyRequest carries question, choices, and a oneshot::Sender<String> for the response; add with_clarify_channel() builder method
- [X] T044 [US5] Add ClarifyRequest event variant to `crates/joey-agent-core/src/events.rs`: ClarifyRequest { question: String, choices: Vec<String>, response_tx: oneshot::Sender<String> }
- [X] T045 [US5] Wire clarification channel in `crates/joey-cli/src/repl.rs`: when a ClarifyRequest event is received, render the question + choices to the user, read their selection, send it back via the oneshot channel
- [X] T046 [US5] Register Clarify in `crates/joey-tools/src/builtins.rs`: register only when ToolContext::interactive() is true (gate via check() method)

**Tests for User Story 5**

- [X] T047 [P] [US5] Write unit test in `crates/joey-tools/src/tools/clarify_tool.rs`: non-interactive session returns error envelope immediately without blocking
- [X] T048 [P] [US5] Write unit test in `crates/joey-tools/src/tools/clarify_tool.rs`: interactive session with mocked clarify channel sends ClarifyRequest and returns the user's response text

**Checkpoint**: Clarify tool works in interactive sessions. P5 acceptance scenarios pass.

---

## Phase 8: User Story 6 - Background Process Lifecycle Management (Priority: P6)

**Goal**: The agent can start, monitor, and control long-running background processes without blocking its turn loop.

**Independent Test**: The agent starts a dev server in the background, receives a session handle, polls its output to confirm startup, then runs tests against it, and finally kills the server.

### Implementation for User Story 6

- [X] T049 [P] [US6] Implement ProcessSession struct in `crates/joey-tools/src/tools/process_tool.rs`: holds tokio::process::Child, stdout/stderr RingBuffer (256KB capacity each), Option<ChildStdin>, command string, cwd, started_at, notify_on_complete flag, watch_patterns vec
- [X] T050 [P] [US6] Implement RingBuffer in `crates/joey-tools/src/tools/process_tool.rs`: VecDeque<u8> with fixed capacity, push() evicts oldest when full and sets truncated=true, drain_new() returns bytes since last poll, lines() for paginated reads
- [X] T051 [P] [US6] Implement ProcessRegistry in `crates/joey-tools/src/tools/process_tool.rs`: thread-safe HashMap<String, ProcessSession> behind Arc<Mutex>; insert/list/get/remove operations; auto-remove on process exit
- [X] T052 [US6] Implement the process tool in `crates/joey-tools/src/tools/process_tool.rs`: struct Process implementing Tool trait; action dispatch for list/poll/log/wait/kill/write/submit/close per contracts/process-tool.md; each action operates on ProcessRegistry
- [X] T053 [US6] Activate background mode in the terminal tool `crates/joey-tools/src/tools/terminal_tool.rs`: replace the "not supported" stub for background=true with actual tokio::process::Command::spawn(); capture stdout/stderr to RingBuffers; register ProcessSession in ProcessRegistry; return session_id immediately
- [X] T054 [US6] Register Process tool in `crates/joey-tools/src/builtins.rs` register_all() and update toolsets.rs to include "process" in the terminal toolset definition
- [X] T055 [US6] Wire the global ProcessRegistry: add it as a lazy_static or once_cell in `crates/joey-tools/src/tools/process_tool.rs`, accessible from both the terminal tool (for spawn) and the process tool (for management)

**Tests for User Story 6**

- [X] T056 [P] [US6] Write unit test in `crates/joey-tools/src/tools/process_tool.rs`: RingBuffer caps at configured capacity, evicts oldest data, sets truncated flag
- [X] T057 [P] [US6] Write integration test in `crates/joey-tools/src/tools/process_tool.rs`: spawn `echo hello` as background process; poll returns "hello" in stdout; process exits and is auto-cleaned from registry
- [X] T058 [P] [US6] Write integration test in `crates/joey-tools/src/tools/process_tool.rs`: spawn long-running process (`sleep 10`); kill action terminates it; registry no longer contains the session
- [X] T059 [P] [US6] Write unit test in `crates/joey-tools/src/tools/process_tool.rs`: write/submit actions send data to stdin; verify with a `cat` process that echoes back

**Checkpoint**: Background process management works. P6 acceptance scenarios pass. INCREMENT 2 COMPLETE.

---

## Phase 9: Polish & Cross-Cutting Concerns

**Purpose**: Ensure workspace-wide green builds, regression coverage, and documentation.

- [X] T060 Run `cargo build --workspace` and fix any compilation errors or warnings in `crates/joey-orchestration/` and modified files
- [X] T061 Run `cargo test --workspace` and verify all existing tests in all `crates/*/` still pass (Constitution Principle VII — non-regression)
- [X] T062 [P] Add concurrency limiter integration test in `crates/joey-orchestration/tests/concurrency_limiter.rs`: dispatch_batch of 5 tasks with max_concurrent_requests=3; verify semaphore never exceeds 3 in-flight calls; excess tasks queue, not reject (SC-008)
- [X] T063 [P] Add orchestration events test in `crates/joey-orchestration/tests/events.rs`: collect AgentEvents from a dispatch_batch; verify SubagentSpawn, SubagentComplete, DelegationBatchComplete all emitted with required fields (SC-009)
- [X] T064 [P] Add regression test for terminal tool in `crates/joey-tools/src/tools/terminal_tool.rs`: verify foreground execution still works unchanged after background mode activation — run `echo test` in foreground, confirm output matches pre-change behavior
- [X] T065 Run quickstart.md validation scenarios 1-6 against the codebase in `crates/joey-orchestration/` (Increment 1) and verify each passes
- [X] T066 Run quickstart.md validation scenarios 7-10 against the codebase in `crates/joey-tools/` (Increment 2) and verify each passes
- [X] T067 [P] Add interrupt propagation test in `crates/joey-orchestration/tests/interrupt.rs`: set interrupt flag during a batch; verify all running subagents wind down cooperatively and DelegationBatchComplete reports the interruption

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion — BLOCKS all user stories
- **US1 (Phase 3)**: Depends on Foundational — this is the MVP
- **US2 (Phase 4)**: Depends on US1 (builds on the Subagent created in US1)
- **US3 (Phase 5)**: Depends on US1 (builds on the dispatch_batch infrastructure)
- **US4 (Phase 6)**: Depends on Foundational only (independent of US1-US3 — can start after Phase 2)
- **US5 (Phase 7)**: Depends on Foundational only (independent of US1-US4)
- **US6 (Phase 8)**: Depends on Foundational only (independent of US1-US5)
- **Polish (Phase 9)**: Depends on all user stories being complete

### Increment Boundaries (per FR-016)

- **Increment 1**: Phases 1-5 (Setup + Foundational + US1 + US2 + US3)
  - MUST build and pass `cargo test --workspace` independently
- **Increment 2**: Phases 6-9 (US4 + US5 + US6 + Polish)
  - MUST build and pass `cargo test --workspace` independently

### User Story Dependencies

- **US1 (P1)**: Depends on Foundational — no dependencies on other stories
- **US2 (P2)**: Depends on US1 (extends Subagent with isolation enforcement)
- **US3 (P3)**: Depends on US1 (extends dispatch_batch with per-task model selection)
- **US4 (P4)**: Independent of US1-US3 — can start after Foundational
- **US5 (P5)**: Independent of US1-US4 — can start after Foundational
- **US6 (P6)**: Independent of US1-US5 — can start after Foundational

### Within Each User Story

- Models/entities before services
- Services before tools (the tool is the consumer of the service)
- Tool implementation before registration
- Registration before CLI wiring
- Tests can be written alongside implementation (Constitution Principle IV)

### Parallel Opportunities

- T006 and T007 (ManagerConfig + subagent types) can be written in parallel
- T010 and T011 (Subagent::new and Subagent::run) can be written in parallel
- All test tasks marked [P] within a story can run in parallel
- US4, US5, US6 can all proceed in parallel after Foundational (they touch different files)
- T062, T063, T064, T067 in Polish phase are all parallelizable

---

## Parallel Example: User Story 1

```bash
# Launch Subagent::new() and Subagent::run() in parallel (different methods, same file but different concerns):
Task: "T010 [P] [US1] Implement Subagent::new() in crates/joey-orchestration/src/subagent.rs"
Task: "T011 [P] [US1] Implement Subagent::run() in crates/joey-orchestration/src/subagent.rs"

# Launch all US1 tests in parallel:
Task: "T019 [P] [US1] Write unit test for subagent isolation in crates/joey-orchestration/src/subagent.rs"
Task: "T020 [P] [US1] Write unit test for dispatch_single in crates/joey-orchestration/src/manager.rs"
Task: "T021 [P] [US1] Write integration test for parallel_batch in crates/joey-orchestration/tests/parallel_batch.rs"
Task: "T022 [P] [US1] Write integration test for batch_resilience in crates/joey-orchestration/tests/batch_resilience.rs"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL — blocks all stories)
3. Complete Phase 3: User Story 1 (parallel delegation)
4. **STOP and VALIDATE**: Test delegate_task single + batch with mocked Transport
5. Run `cargo test --workspace` — must be green

### Increment 1 Completion

1. MVP (above)
2. Add User Story 2 (isolation enforcement)
3. Add User Story 3 (per-subagent model selection)
4. **STOP and VALIDATE**: Full workspace build + test green
5. Increment 1 is shippable

### Increment 2 Delivery

1. Add User Story 4 (session search) — can start immediately after Foundational
2. Add User Story 5 (clarify) — can start immediately after Foundational
3. Add User Story 6 (background processes) — can start immediately after Foundational
4. Complete Polish phase (regression tests, quickstart validation)
5. **STOP and VALIDATE**: Full workspace build + test green

---

## Notes

- [P] tasks = different files or independent methods, no dependencies on incomplete tasks
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify mocked-Transport tests pass before integration testing with live API
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Avoid: vague tasks, same-file conflicts across parallel tasks, cross-story dependencies that break independence

---

## Phase 10: Convergence

- [X] T068 Share the parent SubagentManager's `Arc<Semaphore>` with each batch child instead of creating a new `Semaphore::new(5)` per child in `crates/joey-orchestration/src/manager.rs` dispatch_batch (FR-018) (contradicts)
- [X] T069 Activate background mode in the terminal tool `crates/joey-tools/src/tools/terminal_tool.rs`: replace the "not supported" stub at line 241 with actual `tokio::process::Command::spawn()`, register the ProcessSession in the global ProcessRegistry from process_tool.rs, and return the session_id immediately (FR-012, T053) (missing)
- [X] T070 Pass the `context` field from DelegationRequest to the subagent's first turn in `crates/joey-orchestration/src/subagent.rs` -- prepend it to the goal or inject as the initial user message before calling run_turn (FR-003, contracts/delegation-tool.md) (missing)
- [X] T071 Implement cooperative interrupt propagation in `crates/joey-orchestration/src/manager.rs` dispatch_batch: share the parent's interrupt handle (or a batch-level AtomicBool) with each spawned subagent's Agent so user interrupts wind down all running children (FR-015) (missing)
- [X] T072 Enforce `max_concurrent_children` in dispatch_batch in `crates/joey-orchestration/src/manager.rs`: apply it as a concurrency gate (semaphore or chunked JoinSet spawning) so batches larger than the limit queue rather than spawning all at once (FR-018) (missing)
- [X] T073 Implement persist=true trace persistence in `crates/joey-orchestration/src/subagent.rs`: when DelegationRequest.persist is true, attach a SessionDb to the child Agent via set_session_store, and record the resulting session_id in DelegationResult.persisted_session_id (FR-017) (missing)
- [X] T074 Add explicit summary enforcement for subagent completion in `crates/joey-orchestration/src/subagent.rs`: verify the Agent's built-in MAX_ITERATIONS_SUMMARY_REQUEST fires on budget exhaustion, and add a summary-target instruction (<500 tokens) to the initial goal prompt for natural-completion paths (FR-004) (partial)
- [X] T075 Create integration test `crates/joey-orchestration/tests/concurrency_limiter.rs`: verify the shared semaphore caps in-flight provider requests at max_concurrent_requests across a batch (SC-008, T062) (missing)
- [X] T076 Create integration test `crates/joey-orchestration/tests/model_selection.rs`: batch of 3 TaskSpecs with mixed models via mocked Transport; each DelegationResult records its assigned model (SC-004, T034) (missing)
- [X] T077 Create integration test `crates/joey-orchestration/tests/isolation.rs`: verify parent history is unaffected after a subagent reads files -- parent history contains only the tool call + summary, not intermediate subagent calls (US2/AC1, T029) (missing)
- [X] T078 Create integration test `crates/joey-orchestration/tests/interrupt.rs`: set interrupt flag during a batch; verify all running subagents wind down cooperatively and DelegationBatchComplete reports the interruption (FR-015, T067) (missing)

---

## Phase 11: Convergence

- [X] T079 Extract `model`, `toolsets`, and `persist` from tool args in `crates/joey-orchestration/src/delegation_tool.rs` execute_batch and pass them through to dispatch_batch instead of hardcoding batch_model=None and batch_toolsets=empty (FR-006, FR-007, contracts/delegation-tool.md) (partial)
- [X] T080 Implement persist=true session persistence in `crates/joey-orchestration/src/subagent.rs`: when DelegationRequest.persist is true, open a SessionDb via SessionDb::open_default(), call agent.set_session_store(db, sid), and record the session_id in DelegationResult.persisted_session_id instead of the current unconditional None (FR-017) (partial)
- [X] T081 Add mid-turn interrupt forwarding in `crates/joey-orchestration/src/subagent.rs` run(): spawn a tokio task that polls the batch interrupt flag and propagates it to the agent's interrupt_handle while run_turn is executing, so a mid-batch interrupt reaches running subagents not just pre-started ones (FR-015) (partial)
