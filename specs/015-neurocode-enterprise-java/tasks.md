---

description: "Task list for NeuroCode — Enterprise Java & Pega Rule System Coding Agent"
---

# Tasks: NeuroCode — Enterprise Java & Pega Rule System Coding Agent

**Input**: Design documents from `/specs/015-neurocode-enterprise-java/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: Included — Constitution IV (Test-First for New Crates) mandates tests alongside implementation, and FR-020 mandates regression coverage.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story. US2 (graph-aware context assembly) is sequenced first among the P1 stories because its infrastructure (SQLite index + tree-sitter parsing + graph traversal) is the prerequisite for US1 (routing assembles context by tier) and US3 (Pega patterns ride on the same parser/graph).

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g. US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- **New crate**: `crates/joey-neurocode/` (library crate, Constitution I)
- **Workspace root**: `Cargo.toml` (workspace members + dependencies)
- **Edits to existing crates**: `crates/joey-tools/`, `crates/joey-agent-core/`, `crates/joey-cli/`
- **Tests**: `crates/joey-neurocode/tests/` (integration) + inline `#[cfg(test)]` (unit)

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Create the new crate skeleton and register it in the workspace.

- [X] T001 Create `crates/joey-neurocode/Cargo.toml` with deps: joey-core, joey-tools, joey-llm-selector (trait only), tree-sitter, tree-sitter-java, tokio, serde, serde_json, rusqlite (bundled), tracing, chrono
- [X] T002 Create `crates/joey-neurocode/src/lib.rs` with module declarations and public re-export stubs for NeuroCodeEngine trait + types
- [X] T003 Add `crates/joey-neurocode` to `[workspace] members` and `[workspace.dependencies]` in root `Cargo.toml`; add `tree-sitter = "0.26"` and `tree-sitter-java = "0.23"` to `[workspace.dependencies]`
- [X] T004 Verify `cargo build -p joey-neurocode` succeeds (empty crate compiles and links tree-sitter)

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented — the SQLite graph store, tree-sitter Java parser, config layer, and the NeuroCodeEngine trait.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [X] T005 Define `CodeArtifactNode`, `ArtifactKind`, `DependencyGraphEdge`, `EdgeKind` types in `crates/joey-neurocode/src/graph/node.rs` and `crates/joey-neurocode/src/graph/edge.rs` per data-model.md Entities 3–4
- [X] T006 [P] Implement SQLite schema v1 (tables: code_artifacts, graph_edges, code_artifacts_fts, patterns, anti_patterns, domain_knowledge, domain_knowledge_fts, schema_meta) in `crates/joey-neurocode/src/graph/store.rs` per contracts/graph-store-schema.md
- [X] T007 Implement `DependencyGraph::open(path)`, `upsert_node()`, `upsert_edge()`, `query_fts()`, `traverse_edges()` in `crates/joey-neurocode/src/graph/mod.rs` (depends on T005, T006)
- [X] T008 [P] Write round-trip test: file → model → file for the SQLite schema in `crates/joey-neurocode/tests/graph_round_trip.rs` asserting node/edge insert+read+update preserves data (Constitution IV)
- [X] T009 [P] Implement `tree-sitter-java` AST extraction: parse a `.java` file, extract classes/interfaces/enums/methods/fields with their annotations, implemented interfaces, declared dependencies (imports + @Autowired/injection points) in `crates/joey-neurocode/src/parse/java.rs` per FR-006
- [X] T010 [P] Write tree-sitter extraction test in `crates/joey-neurocode/tests/tree_sitter_extract.rs` asserting correct extraction of structural metadata from a representative Spring Boot service/interface/repository sample
- [X] T011 Implement ingestion pipeline: walk project source tree, parse each `.java` file via T009, upsert nodes+edges into the graph store in `crates/joey-neurocode/src/parse/mod.rs` (depends on T007, T009)
- [X] T012 [P] Implement `NeuroCodeConfig` struct + load from `config.yaml` dotted keys (`neurocode.enabled`, `neurocode.tier.*`, `neurocode.verify.*`, `neurocode.classifier.*`, `neurocode.pega.*`) in `crates/joey-neurocode/src/config.rs` per contracts/neurocode-command.md
- [X] T013 Define `NeuroCodeEngine` trait (classify, assemble_context, is_active) + `CodingRequest` struct in `crates/joey-neurocode/src/engine.rs` per contracts/neurocode-engine-trait.md
- [X] T014 Implement project-hash resolution: hash of repo-root path → `~/.joey/neurocode/projects/<hash>/graph.db` via `process_joey_home()` in `crates/joey-neurocode/src/graph/store.rs` (depends on T006; uses joey-core::constants)

**Checkpoint**: Foundation ready — graph store, tree-sitter parser, config, and the trait exist. User story implementation can now begin.

---

## Phase 3: User Story 2 - Dependency-Graph-Aware Context Assembly (Priority: P1) 🎯 MVP

**Goal**: The agent assembles a dependency-aware context graph per request so the model never reasons about a type whose definition it hasn't been given (FR-004/005/006/007/008).

**Independent Test**: Index a project with a service that implements an interface and injects a repository; ask the agent to edit a method on the service. Verify the assembled context visibly contains the interface and the repository, and that the generated code uses real signatures from both.

### Implementation for User Story 2

- [X] T015 [P] [US2] Implement `ContextAssembler` in `crates/joey-neurocode/src/context/mod.rs`: given a target artifact, perform graph expansion (depth ≤ 2 edges) pulling in implemented interfaces, injected dependencies, and exchanged types per FR-007
- [X] T016 [P] [US2] Implement tier-adaptive context budget sizing in `crates/joey-neurocode/src/context/budget.rs`: economical tier gets focused slice (method + immediate interface + deps to mock), frontier tier gets fuller graph (class + interface + repository + DTO) per FR-008
- [X] T017 [US2] Implement `ContextAssembler::assemble(request, tier)` returning `AssembledContext` (formatted_context string + expanded_nodes with ExpansionReason tags) in `crates/joey-neurocode/src/context/mod.rs` (depends on T015, T016)
- [X] T018 [US2] Wire `assemble_context` into the `NeuroCodeEngine` default impl in `crates/joey-neurocode/src/engine.rs` (depends on T013, T017)
- [X] T019 [P] [US2] Implement cold/un-indexed detection (FR-016): when `graph.db` is empty or missing, operate in degraded mode using only the active file + immediate imports, and inform the developer
- [X] T020 [P] [US2] Implement non-Java-project detection (FR-015): detect when the target project has no Java/Pega artifacts and fall back to ordinary retrieval/generation with a clear notice
- [X] T021 [US2] Write context-assembly integration test in `crates/joey-neurocode/tests/context_assembly.rs`: seed a graph with UserServiceImpl→UserService→UserRepository, verify assemble_context includes all three nodes with correct ExpansionReason tags, and verify economical vs frontier budget sizing (FR-007/008)

**Checkpoint**: Graph-aware context assembly works end-to-end. The agent can index a project and assemble dependency-aware context for any code artifact.

---

## Phase 4: User Story 1 - Complexity-Routed Code Generation (Priority: P1)

**Goal**: The agent classifies requests by complexity and routes them between economical and frontier model tiers, composing with spec 011's ModelAllocator (FR-001/002/003/017/018/020).

**Independent Test**: Issue two requests of clearly different complexity (one boilerplate/test, one architectural refactor). Verify each is routed to the appropriate tier and the developer can see the reasoning and override.

### Implementation for User Story 1

- [X] T022 [P] [US1] Define `ComplexityTier` (#[non_exhaustive] enum), `ComplexityRoute`, `ClassificationSignal`, `SignalKind` types in `crates/joey-neurocode/src/classifier.rs` per contracts/complexity-route.md and data-model.md Entities 1–2
- [X] T023 [P] [US1] Implement deterministic `ComplexityClassifier` in `crates/joey-neurocode/src/classifier.rs`: keyword signals (configurable), scope fan-out, graph-hub detection → produce ComplexityRoute with reasoning (non-async, O(1), FR-017) per research.md §5
- [X] T024 [P] [US1] Implement `TierModelResolver` in `crates/joey-neurocode/src/tier_resolver.rs`: tier → model id via config lookup (Mode 2) with fallback to agent default model; Mode 1 composition path (011 constraint hint) stub per contracts/tier-routing-composition.md
- [X] T025 [US1] Wire `classify` into the `NeuroCodeEngine` default impl in `crates/joey-neurocode/src/engine.rs` (depends on T013, T023)
- [X] T026 [US1] Implement tier override: developer pins/unpins tier for next task or session, persisted in ComplexityRoute.overridden for transparency (FR-002) in `crates/joey-neurocode/src/classifier.rs`
- [X] T027 [US1] Implement the turn-loop intercept in `crates/joey-agent-core/src/agent.rs`: in `build_request()` (line ~871) and `resolve_main_turn_model()` (line ~890, where the 011 allocator is already wired via `self.model_allocator`). Before model dispatch, if `engine.is_active()` call `classify()` then `assemble_context()`, prepend context to the request, pass tier to model resolution; no-op when engine is None (FR-020)
- [X] T028 [US1] Implement regression test for disabled state in `crates/joey-neurocode/tests/regression_disabled.rs`: with `neurocode.enabled = false`, assert classify/assemble_context are not called, no messages injected, system prompt bytes unchanged (FR-020, SC-008)
- [X] T029 [P] [US1] Write classifier test in `crates/joey-neurocode/tests/classifier.rs`: assert "write a JUnit test" → Economical, "refactor to Streams + fix @Transactional boundary" → Frontier, ambiguous input → AmbiguousDefault (FR-001, SC-001)

**Checkpoint**: Complexity routing works end-to-end. The agent classifies requests, resolves tiers, assembles tier-appropriate context, and routes to the right model. Disabled state is byte-identical to today.

---

## Phase 5: User Story 3 - Pega Platform Rule System Awareness (Priority: P1)

**Goal**: The agent understands the Pega rule system well enough to generate correct Pega artifacts — version-adaptive detection, rule-class-family awareness, and rule-reference preservation (FR-009, Clarification Q1/Q4).

**Independent Test**: On a Pega Platform codebase, ask the agent to create a new rule instance. Verify the generated artifact uses the correct rule class family and follows Pega naming conventions.

### Implementation for User Story 3

- [X] T030 [P] [US3] Implement Pega version detection in `crates/joey-neurocode/src/pega/version.rs`: probe project for Pega version markers (config override → Gradle/Maven BOM → in-source markers `com.pega.*`/`Rule-*` patterns) per research.md §4 and Clarification Q4; return detected version or None (generic-Java fallback)
- [X] T031 [P] [US3] Implement Pega rule-pattern tree-sitter queries in `crates/joey-neurocode/src/parse/pega.rs`: over the Java AST, recognize `Rule-Obj-*`/`Data-*`/`Work-*` class patterns, directed-inheritance declarations, rule-reference patterns (FR-009, research.md §4)
- [X] T032 [US3] Define `PegaMetadata` struct (rule_class_family, rule_name, references_rules, inherits_from, pega_version) in `crates/joey-neurocode/src/pega/metadata.rs` per data-model.md Entity 8
- [X] T033 [US3] Implement Pega rule-type metadata ingestion in `crates/joey-neurocode/src/pega/metadata.rs`: ingest rule-type metadata (the rule model, instance/reference semantics) as domain knowledge grounded in the detected Pega version (FR-009, Clarification Q1 Option B)
- [X] T034 [US3] Extend the ingestion pipeline (`crates/joey-neurocode/src/parse/mod.rs`) to populate `PegaMetadata` on matching artifacts and emit `ReferencesRule`/`InheritsRule` edges (depends on T011, T031, T032)
- [X] T035 [US3] Implement Pega-specific context assembly: when assembling context for a Pega artifact, include referenced rules and inheritance parents; preserve rule references during edits (FR-009b) in `crates/joey-neurocode/src/context/mod.rs` (extends T017)
- [X] T036 [P] [US3] Write Pega version detection test in `crates/joey-neurocode/tests/pega_version_detect.rs`: assert correct detection from Gradle BOM, Maven dependency, in-source markers, config override; assert None for non-Pega project (FR-009, Q4)

**Checkpoint**: Pega rule awareness works. The agent detects the Pega version, recognizes rule-class families and references, and generates Pega-correct artifacts.

---

## Phase 6: User Story 4 - Build/Test Feedback Loop with Learned Patterns (Priority: P2)

**Goal**: After generating code, the agent runs verification tooling, feeds failures back for automatic correction, and records successes as patterns and failures as anti-patterns (FR-010/011/012).

**Independent Test**: Generate code that fails a compile step; verify the agent receives the failure, produces a fix, re-verifies, and records the anti-pattern which surfaces on a subsequent edit.

### Implementation for User Story 4

- [X] T037 [P] [US4] Implement verification step runner in `crates/joey-neurocode/src/verify/runner.rs`: execute configured shell commands (e.g. `mvn compile`, `mvn test`) via the existing subprocess path (joey-tools), with per-step timeout_sec and graceful degradation when tooling is absent (FR-010, FR-012)
- [X] T038 [P] [US4] Implement error-output parsing in `crates/joey-neurocode/src/verify/parse.rs`: parse Checkstyle XML, compiler errors, and test failures into structured error signatures
- [X] T039 [US4] Implement the feedback loop orchestrator in `crates/joey-neurocode/src/verify/mod.rs`: detached `tokio::spawn` task that runs steps in order, feeds failures back for a correction pass (up to max_fix_iterations), never blocks the interactive turn (FR-010, FR-017)
- [X] T040 [US4] Implement `LearnedPattern` recording in `crates/joey-neurocode/src/memory/mod.rs`: on verified success, store prompt_signature + generation_summary + verify_result + artifact_ids + tier in the patterns table (FR-011)
- [X] T041 [US4] Implement `LearnedAntiPattern` recording in `crates/joey-neurocode/src/memory/mod.rs`: on failure+fix, store error_signature + resolution + artifact_ids in the anti_patterns table; surface as warning when the same area is edited (FR-011, SC-007)
- [X] T042 [US4] Write feedback-loop test in `crates/joey-neurocode/tests/verify_loop.rs`: simulate a compile failure, verify the loop feeds it back, produces a corrected result, records an anti-pattern; verify the anti-pattern surfaces on a subsequent edit of the same area (FR-010/011, SC-006/007)

**Checkpoint**: The feedback loop works. Generated code is verified automatically, failures self-correct, and lessons are learned as patterns/anti-patterns.

---

## Phase 7: User Story 5 - Domain Knowledge Ingestion (Priority: P3)

**Goal**: The agent ingests framework docs (version-specific), entity/DTO catalogs, and historical postmortems, applying them during generation (FR-013/014).

**Independent Test**: Ingest framework docs, an entity definition, and a postmortem; ask the agent to write an endpoint for that entity in a style matching a past incident. Verify it uses real entity fields, version-correct config, and surfaces the postmortem as a warning.

### Implementation for User Story 5

- [X] T043 [P] [US5] Implement domain-knowledge ingestion in `crates/joey-neurocode/src/memory/domain.rs`: ingest a source file/dir by category (FrameworkDocs/EntityCatalog/Postmortem) into domain_knowledge table + domain_knowledge_fts, with version_tag and provenance (FR-013/014)
- [X] T044 [US5] Implement domain-knowledge retrieval in `crates/joey-neurocode/src/memory/domain.rs`: FTS5 query over ingested knowledge during context assembly — framework-version matching, entity-field lookup, postmortem-pattern matching (FR-013)
- [X] T045 [US5] Implement conflict resolution: when two sources with overlapping category+version conflict, most-recently-ingested wins, flag the conflict, allow `/neurocode domain remove` (spec edge case) in `crates/joey-neurocode/src/memory/domain.rs`
- [X] T046 [US5] Extend `ContextAssembler` (`crates/joey-neurocode/src/context/mod.rs`) to pull domain-knowledge hits into the assembled context when relevant (version-correct config for framework docs, real fields for entity catalog, warning text for matching postmortems) (depends on T017, T044)

**Checkpoint**: Domain knowledge works. Ingested docs/entities/postmortems are applied during generation with identifiable provenance.

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: CLI integration, tool registration, subagent cascade, and final validation.

- [X] T047 [P] Register the 4 NeuroCode tools (`neurocode_index`, `neurocode_query`, `neurocode_status`, `neurocode_ingest`) in `crates/joey-tools/src/builtins.rs` via a new `register_neurocode_tools(registry, engine_handle)` function; tools' check() returns false when disabled (FR-020)
- [X] T048 [P] Implement `/neurocode` slash command handler in `crates/joey-cli/src/commands/neurocode.rs`: subcommands (status, tier, index, query, ingest, patterns, anti-patterns, domain) with plain-text output (Constitution II)
- [X] T049 Add `/neurocode` to the slash REGISTRY in `crates/joey-cli/src/slash.rs` and the dispatch arm in `crates/joey-cli/src/repl.rs`
- [X] T050 Add `joey-neurocode.workspace = true` to `crates/joey-agent-core/Cargo.toml` and `crates/joey-cli/Cargo.toml`
- [X] T051 Implement + verify subagent cascade (FR-021): verify NeuroCode config (`neurocode.*` keys) already flows through `parent_config_tree` in `joey-orchestration`'s `register_orchestration_*` functions (confirmed: `register_orchestration` receives `parent_config_tree: joey_core::Config`); add any missing wiring so the subagent's engine uses the same `graph.db` (shared index by project-root identity, no re-ingestion) per contracts/subagent-cascade.md
- [X] T052 [P] Write subagent cascade test: delegate a coding task, verify the subagent sees the parent's artifact count (shared index), inherits tier config, and does not re-index (FR-021, Clarification Q5)
- [X] T053 Run full quickstart.md validation scenarios (Scenarios 1–7) against the built feature
- [X] T054 [P] Run `cargo build --workspace` (implicitly verifies DAG acyclicity — Constitution VI — since a dependency cycle fails compilation) and `cargo test -p joey-neurocode` and `cargo test -p joey-agent-core` — all must pass (Constitution VII; per-crate tests used instead of `cargo test --workspace` due to the pre-existing joey-cli test hang)
- [X] T055 Update `PORTING.md` with NeuroCode as a new subsystem (Deliberate-deviation from upstream: no Qdrant, uses SQLite+FTS5 instead; adds tree-sitter for Java AST parsing)

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion — BLOCKS all user stories
- **US2 (Phase 3)**: Depends on Foundational — graph store + tree-sitter + config + trait
- **US1 (Phase 4)**: Depends on Foundational + US2 (routing assembles context by tier via the ContextAssembler built in US2)
- **US3 (Phase 5)**: Depends on Foundational + US2 (Pega patterns ride on the same tree-sitter parser + graph traversal built in US2)
- **US4 (Phase 6)**: Depends on Foundational + US2 (feedback loop runs over generated code whose context was assembled by US2)
- **US5 (Phase 7)**: Depends on Foundational + US2 (domain knowledge retrieval extends the ContextAssembler built in US2)
- **Polish (Phase 8)**: Depends on all P1 stories (US1/US2/US3) for the CLI/tools/cascade integration

### User Story Dependencies

- **US2 (P1, MVP)**: Can start after Foundational — the graph-aware context assembly is the "Java secret sauce" that all other stories build on
- **US1 (P1)**: Depends on US2 (the classifier routes, but the turn-loop intercept calls assemble_context which US2 provides); independently testable once US2 is done
- **US3 (P1)**: Depends on US2 (extends the same tree-sitter parser + graph); independently testable once US2 is done
- **US4 (P2)**: Depends on US2; independently testable
- **US5 (P3)**: Depends on US2; independently testable

### Within Each User Story

- Types/models before services
- Services before integration/wiring
- Core implementation before tests
- Story complete before moving to next priority

### Parallel Opportunities

- T006, T009, T012 can run in parallel (different files in Foundational)
- T015, T016 can run in parallel (different context-assembly files in US2; T017 depends on both, so not parallel)
- T022, T023, T024 can run in parallel (different classifier files in US1)
- T030, T031, T032 can run in parallel (different Pega files in US3)
- T037, T038 can run in parallel (different verify files in US4)
- T043 can run in parallel within US5
- T047, T048 can run in parallel (tools vs command in Polish)

---

## Implementation Strategy

### MVP First (User Story 2 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL — blocks all stories)
3. Complete Phase 3: User Story 2 (graph-aware context assembly)
4. **STOP and VALIDATE**: Index a project, verify context assembly pulls in interfaces + dependencies
5. At this point the agent delivers real value: graph-aware context even without tier routing

### Incremental Delivery

1. Setup + Foundational → Foundation ready
2. Add US2 (context assembly) → Test independently → Deploy/Demo (MVP!)
3. Add US1 (complexity routing) → Test independently → Deploy/Demo
4. Add US3 (Pega awareness) → Test independently → Deploy/Demo
5. Add US4 (feedback loop) → Test independently → Deploy/Demo
6. Add US5 (domain knowledge) → Test independently → Deploy/Demo
7. Polish: CLI, tools, subagent cascade, PORTING.md → Final validation
8. Each story adds value without breaking previous stories

---

## Notes

- US2 is the MVP because graph-aware context assembly is the prerequisite for all other stories (the classifier needs context to size scope; Pega patterns extend the same parser; the feedback loop runs over generated code)
- [P] tasks = different files, no dependencies on incomplete tasks
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence
- Constitution IV: tests accompany implementation, not deferred
- Constitution VII: `cargo build --workspace` and `cargo test --workspace` (or affected `-p` subset) MUST stay green on every increment

---

## Phase 9: Convergence

**Purpose**: Close gaps found by `/speckit-converge` — the engine exists but is not wired into the live agent, and several mid-feature behaviors (Pega edges, verify-loop orchestration, anti-pattern surfacing, domain retrieval, non-Java fallback) are absent from the code despite tasks being marked complete.

- [X] T056 CRITICAL: Wire the NeuroCode engine into the live agent — in `crates/joey-cli/src/repl.rs` and `crates/joey-cli/src/oneshot.rs`, construct `Arc<DefaultEngine>` from config when `neurocode.enabled = true` (mirroring the `try_build_allocator` pattern at repl.rs:172) and inject via `agent.set_neurocode_engine(...)`; verify via a startup test that classify/assemble_context run on a live turn per FR-001/002/007/008, plan T027 (missing)
- [X] T057 CRITICAL: Bridge and register the 4 NeuroCode tools — implement `impl NeuroCodeBackend for DefaultEngine` (or an adapter in joey-cli) and call `register_neurocode_tools(registry, backend)` at agent startup in repl.rs/oneshot.rs so the model sees `neurocode_index`/`neurocode_query`/`neurocode_status`/`neurocode_ingest` when enabled per contracts/neurocode-tools.md, task T047 (missing)
- [X] T058 Extend `ingest_project` (`crates/joey-neurocode/src/parse/mod.rs`) to detect Pega rule artifacts via `parse::pega::extract_pega_metadata`, populate `CodeArtifactNode.pega_metadata`, and emit `ReferencesRule`/`InheritsRule` edges per FR-009, task T034; add ingestion test with a Pega-style source sample (missing)
- [X] T059 Implement Pega-specific context assembly: when the target artifact carries `pega_metadata`, include referenced rules and inheritance parents in the assembled context and preserve rule references during edits (FR-009b), extending `ContextAssembler::assemble` in `crates/joey-neurocode/src/context/mod.rs` per task T035 (missing)
- [X] T060 Implement Pega rule-type metadata ingestion as domain knowledge grounded in the detected Pega version (rule model, instance/reference semantics) in `crates/joey-neurocode/src/pega/metadata.rs`, storing via the domain-knowledge tables per FR-009/Clarification Q1 Option B, task T033 (missing)
- [X] T061 Complete the verify feedback loop: add a detached `tokio::spawn` orchestrator in `crates/joey-neurocode/src/verify/mod.rs` that runs steps in order, feeds failures back for a correction pass (up to `max_fix_iterations`), never blocks the interactive turn, and is invoked after generation completes per FR-010, task T039 (partial)
- [X] T062 Implement anti-pattern surfacing: lookup active anti-patterns by artifact area (artifact_ids) during context assembly, increment `hit_count`, and include the warning text in the assembled context when the same area is edited per FR-011/SC-007, task T041 (missing)
- [X] T063 Implement domain-knowledge ingestion and retrieval: an ingest function in `crates/joey-neurocode/src/memory/domain.rs` that reads a source file/dir by category (FrameworkDocs/EntityCatalog/Postmortem) into `domain_knowledge` + `domain_knowledge_fts` with version_tag and provenance, and extend `ContextAssembler` to pull FTS hits into the assembled context per FR-013, tasks T043/T044/T046 (missing)
- [X] T064 Implement domain-source conflict resolution: when two sources with overlapping category+version_tag conflict, most-recently-ingested wins for retrieval, the conflict is flagged (visible in `/neurocode status`), and removal is honored per spec edge case "conflicting sources", task T045 (missing)
- [X] T065 Implement non-Java-project detection (FR-015): detect when the target project has no Java/Pega artifacts and fall back to ordinary retrieval/generation with a clear notice (skip graph assembly, do not force the Java graph onto the project), task T020 (missing)
- [X] T066 Write the subagent cascade test (task T052): delegate a coding task via the orchestration path, verify the subagent's engine resolves the same `graph.db` (shared index by project-root hash), inherits `neurocode.*` config via `parent_config_tree`, and does not re-index per FR-021, Clarification Q5 (missing)
- [X] T067 Add an agent-core regression test asserting the disabled-state contract end-to-end: with the engine absent, `build_request` produces byte-identical system prompt and no classify/assemble_context calls, no injected messages per FR-020/SC-008, task T028 (partial)
- [X] T068 Emit tier transparency in the turn-loop intercept: log (tracing) the chosen tier, reasoning, and override state for every routed request per FR-002/SC-002 — the intercept currently discards the `ComplexityRoute` reasoning (partial)
- [X] T069 Fix Pega in-source version detection to return a real version or `None` (generic-Java fallback) instead of the placeholder `"Pega (in-source markers)"` in `crates/joey-neurocode/src/pega/version.rs` per research.md §4, Clarification Q4 (partial)
- [X] T070 Implement automatic economical→frontier tier escalation when a task fails verification on the economical tier (spec edge case "router/developer disagree"), hooking into the verify loop from T061 (missing)
- [X] T071 Clean up the 10 compiler warnings in joey-neurocode (dead `ensure_graph`, unused variables) and re-verify `cargo build --workspace` warning-clean for the new crate per Constitution VIII lean-code discipline (partial)
