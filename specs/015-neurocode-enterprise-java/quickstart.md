# Quickstart: NeuroCode — Enterprise Java & Pega Rule System Coding Agent

**Spec**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)

This is a **validation guide**, not implementation code. It documents
runnable scenarios that prove the feature works end-to-end. Implementation
details belong in `tasks.md` (Phase 2).

## Prerequisites

- A build of the workspace with the new crate:
  `cargo build -p joey-neurocode && cargo build -p joey-cli`
- A representative enterprise Java project to index (with Spring Boot
  service/interface/repository artifacts), or a Pega Platform codebase for
  the Pega scenarios.
- NeuroCode enabled in config:
  ```yaml
  neurocode:
    enabled: true
    tier:
      economical:
        model: "<an economical model id from your provider>"
      frontier:
        model: "<a frontier model id from your provider>"
  ```
- The agent running: `cargo run -p joey-cli --`

## Scenario 1 — Index a project and verify the structural graph (User Story 2)

**Validates**: FR-004/005/006 (graph + metadata + tree-sitter parsing), SC-003.

**Steps**:
1. In the agent, point at the project root and trigger indexing:
   call the `neurocode_index` tool (or run `/neurocode index`).
2. Wait for the async job to report `artifacts_seen`.
3. Query the graph: call `neurocode_query` with
   `query_type=search, symbol=UserServiceImpl` (or any service in the project).
4. Run `/neurocode` to see status.

**Expected outcome**:
- `neurocode_status` reports a non-zero artifact count and the detected
  framework version.
- `neurocode_query` returns the service with its `implemented_interfaces`
  (`UserService`) and `declared_dependencies` (`UserRepository`) populated —
  proving tree-sitter extracted the structural metadata (FR-005/006).
- The `graph.db` file exists at
  `~/.joey/neurocode/projects/<hash>/graph.db`.

## Scenario 2 — Complexity routing routes two requests to different tiers (User Story 1)

**Validates**: FR-001/002, SC-001/002.

**Steps**:
1. Ensure the project is indexed (Scenario 1).
2. Issue a boilerplate request: "Write a JUnit 5 test for
   `UserServiceImpl.findById`."
3. Observe which tier served the request (visible in the route reasoning).
4. Issue an architectural request: "Refactor `UserServiceImpl` to use
   Optional, fix the @Transactional boundary, and migrate to Streams."
5. Observe which tier served this request.

**Expected outcome**:
- The boilerplate/test request routes to `Economical`.
- The architectural refactor routes to `Frontier`.
- Both routes show their reasoning (transparency — SC-002).
- The developer can override: `/neurocode tier frontier` before a request
  forces the Frontier tier regardless of classification.

## Scenario 3 — Dependency-aware context assembly (User Story 2)

**Validates**: FR-007/008, SC-003.

**Steps**:
1. Ask the agent to edit a method on `UserServiceImpl` that calls
   `userRepository.findById(...)`.
2. Before the model is dispatched, inspect what context was assembled (the
   route reasoning logs the graph expansion).

**Expected outcome**:
- The assembled context visibly contains the `UserService` interface (because
  `UserServiceImpl` implements it) and the `UserRepository` (because the
  method injects/calls it) — SC-003: zero referenced types absent from
  context.
- On the `Economical` tier: only the focused slice (method + interface +
  fields to mock).
- On the `Frontier` tier: the fuller graph (class + interface + repository +
  DTO).

## Scenario 4 — Pega Platform rule awareness (User Story 3)

**Validates**: FR-009, SC-005.

**Steps**:
1. Point the agent at a Pega Platform codebase and run `/neurocode index`.
2. Check `/neurocode` status — it should report the auto-detected Pega version
   (e.g. "Infinity '24").
3. Ask the agent to create a new rule instance that must follow rule
   resolution (e.g. a data transform in the correct class family).

**Expected outcome**:
- `/neurocode` reports the detected Pega version (version-adaptive — Q4).
- The generated artifact uses the correct rule class family (`Rule-Obj-*` /
  `Data-*` / `Work-*`) and follows Pega naming conventions.
- If the agent edits a rule that references another rule, the referenced
  rule is included in the assembled context (FR-009b) and the reference is
  preserved.

## Scenario 5 — Build/verify feedback loop (User Story 4)

**Validates**: FR-010/011/012, SC-006/007.

**Steps**:
1. Configure verification steps in `config.yaml` (e.g. `mvn compile`,
   `mvn test -Dtest={target_class}`).
2. Ask the agent to generate code that intentionally has a compile error
   (e.g. references a non-existent method).
3. Observe the feedback loop: the agent receives the compile error,
   produces a fix, re-verifies.

**Expected outcome**:
- The verification step runs automatically after generation (SC-006).
- The failure is fed back and a corrected version is produced without manual
  intervention.
- `/neurocode anti-patterns` shows the recorded anti-pattern attached to the
  codebase area.
- Re-editing the same area surfaces the anti-pattern as a warning (SC-007).

## Scenario 6 — Disabled state is byte-identical (FR-020, SC-008)

**Validates**: Constitution VII regression gate.

**Steps**:
1. Set `neurocode.enabled: false` in config.
2. Run the agent and issue a coding request.
3. Inspect the conversation history and system prompt.

**Expected outcome**:
- The request uses the agent's existing single-model path.
- No NeuroCode tools are offered to the model (their `check()` returns false).
- No messages are injected into conversation history.
- The system prompt is byte-stable (unchanged from a pre-NeuroCode build).
- `/neurocode` reports `enabled: false`.

## Scenario 7 — Subagent inherits + shares (FR-021, Q5)

**Validates**: subagent cascade.

**Steps**:
1. With NeuroCode enabled and the project indexed, delegate a coding task to
   a subagent (via `delegate_task`).
2. Have the subagent query the graph (`neurocode_query`) and check its status
   (`neurocode_status`).

**Expected outcome**:
- The subagent sees the same artifact count and index as the parent (shared
  index — no re-ingestion).
- The subagent's model resolution reflects the parent's tier (cascaded via
  the allocator/config path).
- The subagent's `neurocode_query` returns results from the shared
  `graph.db`.

## Running the test suite

```bash
cargo test -p joey-neurocode                    # new crate's full suite
cargo test -p joey-neurocode -- classifier      # tier classification (FR-001)
cargo test -p joey-neurocode -- graph_round_trip # SQLite schema round-trip (Const. IV)
cargo test -p joey-neurocode -- tree_sitter     # Java AST extraction (FR-006)
cargo test -p joey-neurocode -- regression      # disabled-state byte-identicality (FR-020)
cargo build --workspace                          # whole workspace stays green (Const. VII)
```

Per the workspace memory note: the full `cargo test --workspace` has a
pre-existing hung test in `joey-cli`'s `--bin joey` suite unrelated to this
feature; use `cargo test -p joey-neurocode` for the new crate's tests.
