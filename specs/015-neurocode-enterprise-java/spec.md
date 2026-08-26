# Feature Specification: NeuroCode — Enterprise Java & Pega Rule System Coding Agent

**Feature Branch**: `015-neurocode-enterprise-java`

**Created**: 2026-08-13

**Status**: Draft

**Input**: User description: "Project NeuroCode: Enterprise Java Edition — upgrade the AI coding agent from a static pipeline into a Dynamic Routing System for large enterprise Java codebases. The system routes tasks between a small/fast model (boilerplate, tests, refactoring) and a frontier model (architecture, multi-file refactoring, debugging). It maintains a graph-structured memory of the codebase (interfaces, implementations, injected beans, dependency edges) rather than flat text chunks, assembles a dependency-aware context graph per task, and runs a build/test feedback loop that records successes as reusable patterns and failures as anti-patterns. Additionally optimize it to work with the Pega Platform rule system."

**Adapted from** the user-provided "Project NeuroCode: Enterprise Java Edition" plan. The plan names a concrete stack (Qdrant, `voyage-code-2`/`bge-m3`, `tree-sitter-java`, Qwen2.5-Coder-7B, Claude 3.5 Sonnet / GPT-4o, Spring Boot, Maven/Checkstyle). Those are *implementation proposals* recorded as source material; this specification describes **what** the feature must do and **why**, deferring the concrete technology selection to `/speckit-plan` and `research.md`, where each dependency must be justified against the joey-agent constitution (Rust workspace, lean dependencies, additive-only — Principles I, VI, VIII).

**Relationship to existing specs**: Model routing already exists in spec `011-dynamic-llm-selector`, which routes *internal agent modules* (history compression, vision, main turn) to different models per the LLMSelector paper. NeuroCode's routing is a **different and complementary axis**: it routes *coding-assistance tasks by complexity* (boilerplate → small model; architecture → frontier model) and is scoped to the code-assistant context. The confirmed integration posture (see Clarifications) is **composition**: NeuroCode's complexity-tier decision is an input/constraint to 011's per-module allocator, not a competing router — the two operate on different axes and yield one unified routing decision.

## Clarifications

### Session 2026-08-13

- **Q1 — Pega integration depth**: How deep should the Pega Platform rule-system optimization go? → **Resolved: Pattern-aware + Pega metadata ingestion (Option B).** The agent learns Pega rule conventions (rule class families such as `Rule-Obj-*` / `Data-*` / `Work-`, rule-to-rule references, directed inheritance, rule-resolution precedence) from the indexed codebase AND ingests Pega's rule-type metadata (the rule model, instance/reference semantics) as domain knowledge so generation is grounded in the actual rule system, not just observed patterns. This is chosen over static pattern-awareness (A) for stronger correctness guarantees, and over live Pega integration (C) to avoid introducing a live-system dependency and auth surface in this spec — live validation against a running Pega instance is explicitly out of scope and can be a future feature if needed.
- **Q2 — Relationship to spec `011-dynamic-llm-selector`**: How do NeuroCode's task-complexity routing and 011's per-module routing coexist? → **Resolved: Compose (Option A).** NeuroCode's complexity tier is an input/constraint to 011's per-module allocator. The two operate on different axes (task-complexity vs. agent-internal-module) and yield one unified routing decision. This reuses 011's allocation map, learning loop, and diagnostics rather than duplicating them, keeps the workspace DAG acyclic (NeuroCode is additive on top of 011), and requires 011 to land first or in parallel. If 011 is not enabled, NeuroCode applies its tier choice directly to the configured model for that tier.
- **Q3 — Local-only / privacy mode**: Must the feature guarantee a local-only (no code leaves the machine) operating mode, given the source plan names "local privacy" as a core motivation? → **Resolved: No special privacy mode (Option C).** Code may go to cloud models for either tier; privacy is governed by the existing Joey provider configuration and is the user's responsibility. The feature does not introduce a local-only enforcement point, a cloud-egress warning, or a default-to-local posture — it inherits whatever model/provider the user has configured for each tier. This keeps the feature focused on routing/context quality and avoids duplicating provider-level privacy controls.
- **Q4 — Pega Platform version scope**: Which Pega Platform version(s) should the agent be optimized for, given each major version has materially different rule types and conventions? → **Resolved: Version-adaptive (Option B).** The agent detects the target codebase's declared Pega version from the project's build files / manifests and ingests the matching rule-type metadata; its behavior is validated against the specific version present rather than against a fixed version. This avoids premature scope-locking to one release (which would age fast) and avoids the over-broad mandate to ingest every Pega version's metadata. The detection mechanism and the supported version floor (how old a Pega version is still recognized) are plan/research decisions.
- **Q5 — Subagent interaction with NeuroCode**: How should NeuroCode's capabilities (tier routing + graph context + feedback loop) apply to delegated subagents, given the workspace's orchestration layer (`joey-orchestration`, `joey-omo`) and that spec 011 already routes subagent goals as a module? → **Resolved: Inherit + share (Option A).** A delegated subagent inherits the parent's NeuroCode configuration and uses the shared (already-built) structural index — it never re-runs ingestion or builds a private index. The parent's complexity-tier decision cascades to the subagent via the 011 composition path (FR-018). This avoids wasteful per-subagent re-indexing, prevents index drift between parent and child, and matches how the workspace already treats delegation as a thin dispatch over the same underlying tooling. Subagents are therefore full participants in NeuroCode's graph-aware context, tier routing, and feedback loop, not exempt bypassers.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Complexity-Routed Code Generation for Enterprise Java (Priority: P1)

A developer working in a large enterprise Java codebase asks the agent to write code. The agent classifies the request by complexity: a "write a JUnit 5 test for this method" request is routed to a fast, economical model tier, while a "refactor this service to remove NPEs, fix the `@Transactional` boundary, and migrate to Streams/Optional" request is routed to a frontier reasoning model. In both cases the developer receives code that respects the surrounding interfaces, injected dependencies, and the project's declared framework versions, without having to manually choose a model or paste in the "right" context.

**Why this priority**: Dynamic task-to-tier routing is the headline capability of the plan and the single largest lever on speed, cost, and quality. Without it, the agent either over-spends a frontier model on boilerplate or under-equips a small model on architecture — both failures of the core premise.

**Independent Test**: On a representative enterprise Java project, issue two requests of clearly different complexity (one boilerplate/test, one architectural refactor). Verify each is routed to the appropriate model tier and that the returned code references real (not hallucinated) types, interfaces, and injected fields.

**Acceptance Scenarios**:

1. **Given** the agent is active on an indexed enterprise Java project, **When** the developer requests generation of a unit test for an existing method, **Then** the request is routed to the economical model tier and the generated test correctly mocks the method's declared dependencies.
2. **Given** the same project, **When** the developer requests a multi-file architectural refactor touching an interface, its implementation, and a repository, **Then** the request is routed to the frontier reasoning tier and the response addresses all three artifacts coherently.
3. **Given** a request whose complexity is ambiguous, **When** the router evaluates it, **Then** it resolves to a defined default tier and records the reasoning, rather than blocking or erroring.
4. **Given** routing is enabled, **When** any request is served, **Then** the developer can see which tier served the request and why (transparency), and can override the tier for that task.

---

### User Story 2 - Dependency-Graph-Aware Context Assembly (Priority: P1)

When the agent retrieves code context for any task, it does not return isolated text chunks. It assembles a **context graph**: for a method on `UserServiceImpl`, it automatically pulls the `UserService` interface it implements, the `UserRepository` it depends on, the DTOs it exchanges, and the framework annotations (`@Service`, `@Transactional`) that govern its lifecycle. A small model receives a focused slice (the method + its immediate interface + the fields to mock); a frontier model receives the fuller graph (entire class + interface + repository + DTO). The agent therefore never asks the model to reason about a type whose definition it has not been given, eliminating a primary source of hallucinated field and method names.

**Why this priority**: Graph-aware retrieval is the "Java secret sauce" the plan identifies as making enterprise Java tractable at all. Without it, retrieval returns fragments that are individually useless and the model hallucinates the missing interfaces — a failure mode that no amount of model quality fixes.

**Independent Test**: Index a project with a service that implements an interface and injects a repository; ask the agent to edit a method on the service. Verify the assembled context visibly contains the interface and the repository, and that the generated code uses real signatures from both.

**Acceptance Scenarios**:

1. **Given** a method that calls an injected dependency, **When** the agent assembles context for that method, **Then** the dependency's interface/contract is included in the context provided to the model.
2. **Given** a class that implements an interface, **When** context is assembled for any of its methods, **Then** the interface definition is included alongside the implementation.
3. **Given** the router has selected the economical tier, **When** context is formatted, **Then** it contains only the focused slice (method + immediate interface + dependencies to mock), sized to that tier's context budget.
4. **Given** the router has selected the frontier tier, **When** context is formatted, **Then** it contains the fuller graph (class + interface + repository + DTO) up to that tier's larger budget.
5. **Given** assembled context, **When** the developer inspects what was sent, **Then** they can see the graph expansion that was performed (which related artifacts were pulled in and why).

---

### User Story 3 - Pega Platform Rule System Awareness (Priority: P1)

A developer working in the **Pega Platform** codebase (a large enterprise Java application built on a rule-oriented architecture) asks the agent to create or modify a rule, a data object, an activity, or a service. The agent understands the Pega rule system's structure: rule resolution and class hierarchy (the `Rule-Obj-*` / `Data-*` / `Work-` class families), the relationship between a rule's definition and its instances, the pattern by which rules reference and delegate to other rules, and the Java/declarative backing that implements each rule type. When generating or editing Pega artifacts, the agent respects rule-system conventions (naming, class structure, directed inheritance, rule resolution precedence) so it does not produce artifacts that look plausible but violate Pega's rule model.

**Why this priority**: This is the explicit "optimize for the Pega Platform rule system" requirement and the differentiator from a generic Java agent. P1 because the user named it as a primary goal; without rule-system awareness the agent is no more useful on Pega than on any other Java codebase.

**Independent Test**: On a Pega Platform codebase, ask the agent to create a new rule instance that must follow rule resolution and class-hierarchy conventions. Verify the generated artifact honors the Pega rule model (correct class family, valid references to other rules, conventional naming/structure).

**Acceptance Scenarios**:

1. **Given** the agent is active on a Pega Platform codebase, **When** the developer asks it to create or modify a Pega rule, **Then** the generated artifact uses the correct rule class family and follows Pega rule-resolution conventions.
2. **Given** a rule that references or delegates to another rule, **When** the agent edits it, **Then** the assembled context includes the referenced rule and the agent preserves the reference correctly.
3. **Given** the agent is generating Pega artifacts, **When** it proposes structure, **Then** it respects directed inheritance and rule-precedence semantics rather than producing generic Java that ignores the rule model.
4. **Given** the agent has access to project-specific Pega standards, **When** it generates code, **Then** the output conforms to those standards (e.g., conventions on when to use a data transform vs an activity, naming of rule instances).

---

### User Story 4 - Build/Test Feedback Loop with Learned Patterns (Priority: P2)

When the agent generates code, it does not stop at generation. It runs the project's verification tooling — static analysis/style checks, compilation, and the targeted test — and feeds any failure back to itself for an immediate fix (typically on the economical tier). Successful generations are recorded as reusable patterns; failures (a `NullPointerException`, a bean-creation failure, a missing-`@Transactional` incident) are recorded as anti-patterns that surface as warnings the next time the agent edits the same area. Over time the agent accumulates the project's hard-won lessons and stops repeating mistakes a senior engineer would already know.

**Why this priority**: The feedback loop is what turns one-shot generation into a self-improving assistant. It is P2 because Stories 1–3 deliver value on the first generation; the loop compounds that value over time.

**Independent Test**: Have the agent generate code that initially fails a compile or test step; verify the agent receives the failure, produces a fix, re-verifies, and records the failure+fix as a learned anti-pattern that is surfaced on a subsequent edit of the same area.

**Acceptance Scenarios**:

1. **Given** the agent has generated code, **When** the verification step reports a failure, **Then** the failure output is fed back to the agent and a corrected version is produced without manual intervention.
2. **Given** a corrected generation passes verification, **When** the success is recorded, **Then** the generation plus its prompt is stored as a reusable successful pattern.
3. **Given** a generation failed with a specific error (e.g., a bean-creation failure), **When** the agent later edits the same area, **Then** the recorded anti-pattern is surfaced as a contextual warning.
4. **Given** the verification tooling itself is unavailable (no build tool on PATH, locked workspace), **When** the loop would run, **Then** the agent degrades gracefully (skips verification, informs the developer) rather than failing the whole task.

---

### User Story 5 - Domain Knowledge Ingestion (Frameworks, Entities, Postmortems) (Priority: P3)

To act like a senior engineer, the agent ingests three bodies of domain knowledge: (a) the specific framework versions the enterprise uses (so when it writes an integration it uses the exact configuration syntax for that version, not a generic or newer one); (b) the project's entities and DTOs (so a new endpoint reuses real `@Entity` field names instead of inventing them); and (c) historical incident postmortems (so when it writes code resembling a past production outage, it surfaces the prior lesson as a warning). This knowledge is queryable by the retrieval graph and applied automatically during generation.

**Why this priority**: This is what makes the agent "frontier" in an enterprise setting, but it is P3 because it is an enrichment layer on top of the core routing + graph retrieval + feedback loop, each of which is independently valuable.

**Independent Test**: Ingest the project's framework docs, an entity definition, and one historical postmortem; ask the agent to write an endpoint for that entity in a style matching a past incident. Verify it uses the real entity fields, the correct version-specific configuration, and surfaces the postmortem as a warning.

**Acceptance Scenarios**:

1. **Given** the project's framework documentation has been ingested, **When** the agent writes an integration, **Then** it uses configuration syntax matching the enterprise's declared framework version.
2. **Given** the project's entities/DTOs have been ingested, **When** the agent creates a new endpoint or API surface, **Then** it uses the real fields of the relevant entity rather than hallucinating them.
3. **Given** a historical postmortem has been ingested, **When** the agent generates code resembling the incident's pattern, **Then** it surfaces the postmortem as a contextual warning to the developer.
4. **Given** a body of domain knowledge has been ingested, **When** the developer queries what the agent "knows," **Then** they can list and inspect the ingested knowledge sources.

### Edge Cases

- What happens when the target project is not an enterprise Java/Pega project (e.g., a small Kotlin or Python repo)? The agent detects that the structural-graph machinery does not apply and falls back to ordinary retrieval/generation, clearly noting the degraded mode rather than forcing a Java graph onto non-Java code.
- What happens when the structural knowledge store is empty (fresh project, nothing indexed yet)? The agent operates in a "cold" mode using only the active file and immediate imports, and informs the developer that indexing would improve results.
- What happens when the router and the developer disagree on tier (the router picks the economical tier but the task turns out to need architecture)? The developer can override the tier mid-task, and a task that fails verification on the economical tier can be escalated to the frontier tier automatically.
- What happens when the dependency graph is cyclic or broken (a deleted interface still referenced, a circular DI)? The agent surfaces the broken reference as a finding rather than silently assembling partial context.
- What happens when verification tooling is slow (large enterprise builds)? The feedback loop runs with a configurable timeout and the agent reports partial verification status rather than blocking indefinitely.
- What happens when two ingested knowledge sources conflict (e.g., two framework versions, or a postmortem that contradicts a current standard)? The agent prefers the most recently-ingested/recently-applied source, flags the conflict, and lets the developer resolve it.
- What happens when the same task is routed differently across two identical requests (non-deterministic routing)? The router records its reasoning so the developer can see why and pin a tier if stability is preferred.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST classify an incoming coding-assistance request by complexity and route it to one of at least two model tiers — an economical tier (suited to boilerplate, unit-test generation, simple refactoring) and a frontier tier (suited to architectural changes, multi-file refactoring, concurrency debugging, legacy comprehension) — so that the cost/quality tradeoff matches the task.
- **FR-002**: System MUST expose the tier chosen for each request and the reasoning, and MUST allow the developer to override the tier for a given task (pin to economical, pin to frontier, or revert to automatic).
- **FR-003**: System MUST make routing strictly additive and opt-in: when the feature is disabled, requests use the agent's existing single-model path with byte-identical behavior and no regression (Constitution VII). The default state MUST be disabled so existing users see no change until they enable it.
- **FR-004**: System MUST maintain a structural knowledge of the target codebase as a dependency graph — not flat text — capturing, for each code artifact, its relationships to the interfaces it implements, the implementations of interfaces it declares, the dependencies it injects, and the types it exchanges. (The concrete store and embedding approach are decided in the plan phase; the capability is mandatory here.)
- **FR-005**: System MUST attach structural metadata to each stored code artifact so the graph is queryable — at minimum: enclosing type name, implemented interface(s), package/namespace, framework annotations/declarations, and declared dependencies. For Pega Platform artifacts this metadata MUST additionally capture rule-system identity (rule class family, rule name, references to other rules) so rule resolution and class hierarchy are recoverable from the graph.
- **FR-006**: System MUST parse code with syntax awareness sufficient to extract the structural metadata in FR-005 (type/method/field boundaries, annotations/declarations, imports/dependencies) for the Java family of languages. Parsing MUST be deterministic and not rely on the LLM to guess structure.
- **FR-007**: System MUST, when assembling context for any task, perform graph expansion: starting from the directly-retrieved artifact(s), it MUST pull in the related interfaces, implementations, and injected dependencies defined in the graph, so the model is never asked to reason about a type whose definition it has not been given.
- **FR-008**: System MUST format the assembled context graph adaptively by tier: the economical tier receives a focused slice (the target method/artifact + its immediate interface/contract + the dependencies it must satisfy/mock), and the frontier tier receives the fuller graph (entire class + interface + repository/dependency + exchanged types), each bounded by that tier's context budget.
- **FR-009**: System MUST, for the Pega Platform rule system specifically (per Clarification Q1, Option B — pattern-aware + metadata ingestion; and Q4, Option B — version-adaptive), understand rule resolution and the rule class hierarchy well enough to (a) create or modify Pega artifacts using the correct rule class family and naming, (b) preserve references between rules during edits, and (c) respect directed inheritance and rule-precedence semantics rather than producing generic Java that ignores the rule model. This understanding MUST be grounded in ingested Pega rule-type metadata (the rule model, instance/reference semantics), not solely in patterns observed in the indexed codebase. The agent MUST detect the target codebase's declared Pega version and ingest the rule-type metadata matching that version, validating behavior against the specific version present rather than a fixed version. Live validation against a running Pega instance is explicitly out of scope.
- **FR-010**: System MUST, after generating code, run the project's verification tooling (static analysis/style check, compilation, and the targeted test) and feed any failure back into the agent for an automatic correction pass. The verification step set, ordering, and timeouts MUST be configurable per project.
- **FR-011**: System MUST record verified-successful generations (generation + prompt) as reusable patterns, and MUST record failures with their fixes (error signature + resolution) as anti-patterns. Anti-patterns MUST be surfaced as contextual warnings when the agent later edits the same area of the codebase.
- **FR-012**: System MUST degrade gracefully when verification tooling is unavailable (no build tool, locked workspace, timeout): it skips verification, informs the developer, and still delivers the generated code — verification is an enhancement, not a blocker.
- **FR-013**: System MUST allow ingestion of domain knowledge in three categories: (a) project-specific framework documentation keyed to the enterprise's declared versions, (b) the project's entity/DTO definitions, and (c) historical incident postmortems. Ingested knowledge MUST be queryable by the retrieval graph and applied automatically during generation (version-correct configuration, real entity fields, postmortem warnings).
- **FR-014**: System MUST surface ingested knowledge provenance: the developer can list and inspect ingested sources, and when ingested knowledge is applied to a generation, its source is identifiable (which doc, which entity, which postmortem).
- **FR-015**: System MUST detect when the target project is not an enterprise Java/Pega project and fall back to ordinary retrieval/generation with a clear notice, rather than forcing the structural-graph machinery onto a project where it does not apply.
- **FR-016**: System MUST detect when the structural knowledge store is empty (cold/un-indexed) and operate in a degraded mode using only the active file and immediate imports, informing the developer that indexing would improve results.
- **FR-017**: System MUST resolve routing decisions without blocking the interactive turn on the hot path — complexity classification and graph queries are resolved from cached/indexed state, and any expensive ingestion or verification runs asynchronously (never blocking a developer's turn).
- **FR-018**: Routing of coding tasks by complexity (this feature) MUST compose with the existing dynamic per-module model selector (spec `011-dynamic-llm-selector`) per Clarification Q2: when both are enabled, NeuroCode's complexity-tier decision is an input/constraint to 011's per-module allocator (the two operate on different axes — task-complexity vs. agent-internal-module — and yield one unified routing decision). If 011 is not enabled, NeuroCode applies its tier choice directly to the configured model for that tier. NeuroCode MUST NOT duplicate 011's allocation map, learning loop, or diagnostics — it reuses them.
- **FR-019**: All new logic MUST live in dedicated new crate(s) under `crates/` added to the workspace `members`, independently buildable and testable (`cargo build -p <crate>` / `cargo test -p <crate>`), and `joey-agent-core` MUST consume the capability through a narrow trait rather than having NeuroCode logic threaded through the turn loop's shared core paths (Constitution I, VI). No new runtime dependency is introduced without a recorded justification of weight vs. benefit in the feature's `research.md` (Constitution VIII).
- **FR-020**: The feature MUST ship with regression coverage asserting that, when disabled, the agent's existing single-model generation path is byte-identical (no behavioral change, no new messages injected into conversation history, no alteration of the byte-stable system prompt), satisfying Constitution VII.
- **FR-021**: When work is delegated to a subagent (per Clarification Q5, Option A — inherit + share), the subagent MUST inherit the parent's NeuroCode configuration and MUST use the shared, already-built structural index — it MUST NOT re-run ingestion or build a private index. The parent's complexity-tier decision MUST cascade to the subagent via the 011 composition path (FR-018). A delegated subagent is therefore a full participant in NeuroCode's graph-aware context assembly, tier routing, and feedback loop, not an exempt bypasser. This composes with the existing subagent dispatch path in `joey-orchestration` without threading NeuroCode logic through that crate's internals (Constitution VI).

### Key Entities *(include if feature involves data)*

- **Complexity Route (Tier Assignment)**: The result of classifying a coding request — which model tier (economical/frontier) will serve it, the classification reasoning, and whether the developer overrode it. This is the routing artifact the developer inspects.
- **Code Artifact Node**: A unit of parsed code (a class, interface, method, or Pega rule) stored in the structural knowledge graph, carrying its structural metadata (FR-005) and its graph edges to related artifacts.
- **Dependency Graph Edge**: A typed relationship between two Code Artifact Nodes — implements, is-implemented-by, injects/depends-on, exchanges-type, and (for Pega) references-rule / inherits-rule. Edges drive graph expansion during context assembly.
- **Context Graph**: The transient, per-task assembly of Code Artifact Nodes (directly retrieved + graph-expanded) formatted for a specific tier's context budget. This is what the model actually sees.
- **Learned Pattern (Success)**: A recorded successful generation (prompt + output + the verification that passed), reusable as a reference for similar future tasks.
- **Learned Anti-Pattern (Failure)**: A recorded failure (error signature + resolution) attached to a codebase area, surfaced as a warning when that area is edited again.
- **Domain Knowledge Source**: An ingested body of knowledge (framework docs versioned to the enterprise's stack, an entity/DTO catalog, or a postmortem) that the retrieval graph can draw on during generation, with identifiable provenance.
- **Pega Rule Artifact**: A Code Artifact Node specialized for the Pega Platform rule system — carrying rule class family, rule name, and references to other rules — so the agent can honor rule resolution and class hierarchy when generating Pega artifacts.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: On a representative set of coding requests, the router assigns the economical tier to boilerplate/test requests and the frontier tier to architectural requests with at least 90% agreement with a human senior engineer's tier choice.
- **SC-002**: For every generation, the developer can see which tier served it and why, and can override the tier in a single action — 100% of generations are transparent and overridable.
- **SC-003**: When the agent assembles context for a method that uses an injected dependency or implements an interface, the assembled context provably includes that dependency's contract and that interface — measured as zero generations where the model was asked to reason about a referenced type whose definition was absent from its context.
- **SC-004**: Hallucinated type/field/method names in generated enterprise Java code drop by at least 50% versus the same agent without the dependency-graph context assembly (measured on a fixed evaluation set of real edits).
- **SC-005**: On the Pega Platform codebase, generated Pega artifacts (rules, data objects, services) conform to rule-system conventions (correct class family, valid rule references, conventional naming) on at least 90% of generations, as validated against Pega rule-resolution semantics for the specific version the target codebase declares (per Clarification Q4).
- **SC-006**: When the agent generates code and verification tooling is available, at least one verification pass (static analysis, compile, test) runs automatically and any failure produces a corrected generation without manual intervention — the loop runs end-to-end on 100% of generations where tooling is present.
- **SC-007**: After a recorded anti-pattern, re-editing the same codebase area surfaces the prior failure as a warning 100% of the time.
- **SC-008**: With the feature disabled, the agent's generation behavior is byte-identical to today — zero observable change in output, conversation history, or system prompt (Constitution VII regression gate).
- **SC-009**: Domain knowledge ingestion is reflected in generations: when the project's entities and framework versions are ingested, generated code uses real entity fields and version-correct configuration on at least 90% of relevant generations, and ingested postmortems surface as warnings when matching code patterns recur.
- **SC-010**: Every expensive operation (full ingestion, verification) runs off the interactive hot path — a developer's turn is never blocked waiting for ingestion or verification to complete.

## Assumptions

- The feature is implemented as an **additive extension of the existing joey agent** (new crate(s) + toolset + opt-in enablement), not as a separate standalone service or binary. This matches the constitution's workspace-first, additive-only discipline (Principles I, VII); a standalone deployment would be a larger scope decision and is out of bounds for this spec.
- The dynamic per-module model selector (spec `011-dynamic-llm-selector`) is the **existing routing substrate** NeuroCode composes with (per Clarification Q2, Option A): 011 routes *agent-internal modules* to models; NeuroCode routes *coding tasks by complexity* to tiers. The two are complementary axes. NeuroCode's tier decision is an input/constraint to 011's allocator; if 011 is not enabled, NeuroCode applies its tier choice directly to the configured model for that tier.
- Reasonable defaults are chosen for the tier definitions, the complexity classifier, and the verification step set, so the feature is useful with zero configuration; all are configurable via the existing dotted-key `config.yaml` mechanism.
- "Enterprise Java" is interpreted to include the Java-family languages and frameworks dominant in enterprise codebases (Java with Spring/Jakarta EE, build tools such as Maven/Gradle, JPA/Hibernate). The spec describes capabilities; which specific parsers/stores are used is a plan/research decision constrained by the constitution.
- "The Pega Platform rule system" refers to Pega's rule-oriented architecture — rule resolution, the `Rule-Obj-*` / `Data-*` / `Work-` class hierarchy, directed inheritance, and rule-to-rule references. Per Clarification Q1 (Option B), integration depth is pattern-aware + Pega metadata ingestion (grounded in ingested rule-type metadata); live validation against a running Pega instance is explicitly out of scope.
- Existing invariants — per-conversation prompt caching, byte-stable system prompt, strict message-role alternation, credential handling — are preserved untouched; NeuroCode changes *what context and which tier* serve a coding task, not message structure or auth.
- The feature introduces no special privacy/data-egress mode (Clarification Q3, Option C): whether code is sent to a cloud or local model for a given tier is governed entirely by the existing Joey provider configuration the user has chosen. Privacy is the user's responsibility via provider selection, not a feature-level enforcement point.
- The concrete technology proposals in the source plan (Qdrant, `voyage-code-2`/`bge-m3`, `tree-sitter-java`, named LLMs) are **candidate implementations** evaluated in `research.md` against the constitution's dependency-justification and Rust-workspace constraints (Principles I, VI, VIII); none is assumed adopted by this specification.
- Ingested domain knowledge and learned patterns/anti-patterns are stored under `~/.joey/` (honouring `JOEY_HOME`), project-scoped, with atomic writes and human-readable formats for debuggability — consistent with how the workspace already persists state.
