# Feature Specification: Dynamic LLM Model Selector

**Feature Branch**: `011-dynamic-llm-selector`

**Created**: 2026-08-03

**Status**: Draft

**Input**: User description: "Fully implement a dynamic LLM model selector as proposed by the paper 'Optimizing Model Selection for Compound AI Systems' (arXiv:2502.14815). Add it as a configurable option (`/llm-selector`) that can toggle this mode on and off. It should choose the right model for the corresponding task." The feature is engaged when the user selects the `auto` model, and each allocated model defaults to its highest available context window.

**Adapted from** upstream Hermes Agent `specs/003-dynamic-llm-selector`, re-targeted to the Joey Agent Rust workspace. Upstream is GitHub-Copilot-specific; Joey supports a multi-provider catalog (`joey-providers`: GitHub Copilot, OpenRouter, Anthropic, OpenAI, Z.AI, xAI, Gemini, …), so the candidate pool is generalized to *the active provider's live model catalog* (`/models`), with GitHub Copilot as the canonical source to preserve upstream fidelity.

**Source**: Chen et al., *Optimizing Model Selection for Compound AI Systems* (LLMSelector), arXiv:2502.14815v1, Feb 2025. Core finding: in a compound AI system (multiple LLM calls), allocating *different* models to *different* modules — chosen by an LLM diagnoser that estimates per-module performance — yields 5%–70% quality gains over using a single model everywhere.

## Clarifications

### Session 2026-08-03

- Q: Which models make up the candidate pool? → A: All chat-capable models exposed by the active provider's live catalog (the `/models` endpoint — GitHub Copilot or OpenRouter), spanning every vendor and capability tier the account exposes. The user explicitly wants the full catalog utilized.
- Q: What does "the right model for the task" map to inside Joey? → A: A "task" is a distinct LLM call site — the main agent turn plus the auxiliary/side-LLM modules (history compression, vision analysis, title generation, web extraction, session search, curator review) and delegated subagent goals. Each is a node in the compound system's call graph.
- Q: Should the toggle change behaviour for the running conversation or only new ones? → A: Toggle takes effect on the next conversation turn without mutating prior context (preserves per-conversation prompt caching — the Joey system prompt is built once per session and must stay byte-stable); allocation learning runs asynchronously so it never blocks an interactive turn.
- Q: Is the LLM diagnoser a separate model or one of the candidates? → A: One of the candidate models acts as the diagnoser; it defaults to a versatile-tier model and is itself configurable.
- Q: How does a user turn the feature on? → A: Selecting the `auto` model (in the `joey model` picker or as the configured default `model.model`) is the primary activation — `auto` engages dynamic per-module allocation rather than a single fixed model. The `/llm-selector` command (chat slash command, also reachable from the CLI per Constitution II) controls behaviour (inspect allocations, pin modules, set the learning budget, view diagnostics) and can force-disable the dynamic allocation so that `auto` falls back to the existing cost-only routing.
- Q: What context window does an allocated model use? → A: Each model defaults to its highest configurable context window (the maximum the provider catalog exposes for that model, e.g. Copilot `max_prompt_tokens`), so every module gets the full room its assigned model offers without an artificially low cap.
- Q: How should the selector weigh cost against quality when allocating a model to a module? → A: Quality-first with a cost tie-break — allocate the best-performing model per module (preserving the paper's quality gains), but when two models are comparably good on estimated per-module performance, prefer the cheaper one. This stays consistent with how the existing `auto` cost-bias reasons so a user selecting `auto` sees predictable behaviour.
- Q: What triggers a diagnoser run? → A: Observable failure only — the diagnoser evaluates per-module performance when a turn errors, an auxiliary/side-LLM call fails, or the result is flagged low-quality. Turns that succeed are not diagnosed, so the learning budget is spent where it can actually improve allocations.
- Q: How is each module's model chosen at cold start, before the diagnoser has learned anything? → A: Capability-scored default — reuse the existing provider `auto` feasibility+cost scorer to assign each module the cheapest capable model for its role on the first turn. This makes the feature useful immediately with zero warm-up, respects capability hard-gates (vision/tools/context window) from the first turn, and gives the diagnoser a safe, deterministic baseline to refine from.
- Q: At what scope is the allocation map stored and applied? → A: Global — one shared allocation map across all profiles on the machine, stored at the machine/user level under `~/.joey/` (not inside a single profile's home, honouring `JOEY_HOME`), so learning transfers between profiles and maximizes the compound-system improvement.
- Q: How long does an allocation decision stay cached and applied before it is re-evaluated? → A: Per-turn — allocations are cached for the duration of one turn so every module in a single turn sees a consistent allocation map (deterministic, debuggable behavior), and refreshed at the start of the next turn so diagnoser-driven reallocations take effect on the next user turn rather than sitting stale or churning mid-turn.
- Q: What counts as "comparably good" for the cost tie-break? → A: Two models are comparably good when their estimated per-module performance (`p_j`) is within 5% of the top performer for that module. Within that band, the cheaper model (lower capability tier, or lower billing multiplier within the same tier) is preferred.
- Q: What signals count as "observable failure" for triggering the diagnoser? → A: Four concrete signals: (1) a turn error or exception, (2) an auxiliary/side-LLM call failure or exception, (3) an empty or null model response, (4) a retry triggered by the existing retry mechanism (the first attempt was deemed unsatisfactory). Turns that produce a non-empty, non-error response are not diagnosed.

### Session 2026-08-03 (Joey adaptation)

- Q: Which crate owns the dynamic selector logic (engine, diagnoser, allocation map, `ModelAllocator` trait)? → A: A new dedicated crate `joey-llm-selector` (under `crates/`). It depends on `joey-core` (for `joey_home()` + config) and the provider-catalog surface of `joey-providers`, while `joey-agent-core` consumes only the narrow `ModelAllocator` trait the new crate defines — keeping the workspace DAG strictly acyclic and matching how the workspace already factors cross-cutting concerns (`joey-mcp`, `joey-cron` are each their own crate). The learning engine, diagnoser, persisted map, and `/llm-selector` query logic all live in one independently buildable/testable unit (`cargo build -p joey-llm-selector` / `cargo test -p joey-llm-selector`).
- Q: What on-disk format backs the global allocation map (FR-014)? → A: A single JSON file at `~/.joey/llm-selector/allocations.json`, written atomically via write-temp + rename (the same pattern `joey-core::auth_store` already uses for `auth.json`). This adds no new dependency (`serde_json` is already workspace-standard), stays human-readable for `/llm-selector` debugging, and the atomic rename guarantees a concurrent turn-start read (FR-007) never observes a half-written map. SQLite is rejected as unjustified for a small flat map that is read once per turn and rewritten occasionally (Constitution VIII).
- Q: How is the diagnoser's async LLM call dispatched (FR-009)? → A: A detached `tokio::spawn` task inside `joey-llm-selector`, calling the provider client directly via `joey-providers`. This keeps the diagnoser self-contained in the new crate, reuses the existing hardened chat-completions path (auth, retries, backoff — no second implementation), and detaches the call from the turn's lifetime so it never blocks the hot path; its result writes to the allocation map atomically on completion. A CLI subprocess would duplicate the chat-client path and add IPC; an agent-core callback would thread selector logic back through shared core paths (Constitution VI).

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Enable via the `auto` Model and Control with `/llm-selector` (Priority: P1)

A user runs the agent on a provider that exposes a live model catalog (GitHub Copilot or OpenRouter). They want the agent to automatically pick the best model for each sub-task instead of always using one model. They do this by selecting the `auto` model (in the `joey model` picker or as their configured default). Selecting `auto` engages dynamic per-module model allocation across the compound system — each module is routed to the model best suited to it, using the full catalog. The `/llm-selector` command is the control surface: it reports the current state and candidate pool, lets the user inspect allocations and pin modules, and can force-disable the dynamic allocation so that `auto` falls back to the existing cost-only routing. Disabling or switching away from `auto` restores single-model routing with no changes to past conversation context.

**Why this priority**: The `auto` model selection is the single, natural activation point for the entire feature, and `/llm-selector` is the reliable control/override path. Without a cache-safe way to engage and disengage the allocator in a long-lived conversation, no other capability can be safely used.

**Independent Test**: With a catalog-exposing provider active, select the `auto` model, run `/llm-selector` to confirm the dynamic selector is engaged and the candidate pool is reported, run a turn, then disable the dynamic allocation via `/llm-selector` (or switch to a concrete model) and verify routing falls back without altering earlier messages.

**Acceptance Scenarios**:

1. **Given** the agent is configured on a catalog-exposing provider, **When** the user selects the `auto` model (or runs `/llm-selector` to inspect), **Then** the system reports whether dynamic selection is active, the number of candidate models discovered in the catalog, and the active diagnoser model.
2. **Given** the active model is `auto` with dynamic selection engaged, **When** subsequent turns run, **Then** each module is routed to a model chosen by the selector without rewriting, reordering, or dropping any earlier message in the conversation.
3. **Given** dynamic selection is engaged, **When** the user disables it via `/llm-selector` or switches to a concrete (non-`auto`) model, **Then** every module reverts to a single fixed model (the user's configured default), and no synthetic message is injected into the conversation history.
4. **Given** the active provider exposes no live catalog or no models are available, **When** the user selects `auto` or runs `/llm-selector`, **Then** the system clearly explains that the dynamic selector requires a provider catalog and does not partially enable.

---

### User Story 2 - Automatic Per-Task Model Allocation Using the Full Catalog (Priority: P1)

With the selector enabled, the agent no longer sends every LLM call to the same model. Instead, each distinct task module is routed to the model best suited to it, drawn from the full set of models the user's account exposes. A history-compression call, a vision-analysis call, a code-generation turn, and a fact-checking subagent can each land on a different model, chosen so that the compound system's overall outcome improves. The user does not have to manually pin models per task — the selector allocates them, and a lightweight trace records which model served which module and why.

**Why this priority**: This is the core value of the paper and the feature — choosing the right model for the corresponding task. The toggle (Story 1) is meaningless without this allocation actually happening.

**Independent Test**: Enable the selector on an account whose catalog contains models from multiple vendors/tiers; run a multi-step task that exercises at least two different modules (e.g., a turn that also triggers vision analysis and title generation); verify each module was served by an explicitly selected model and that the selection reasons are recorded.

**Acceptance Scenarios**:

1. **Given** the selector is enabled and the catalog contains multiple eligible models, **When** a turn triggers more than one LLM module, **Then** each module is routed to a model explicitly chosen by the selector (not a blanket default), and at least two modules can be served by different models.
2. **Given** a module requires a capability (e.g., vision, tool-calling, a large context window), **When** the selector allocates a model, **Then** the chosen model is guaranteed to satisfy that requirement — the selector never assigns an incapable model just because it scored well on quality, and each allocated model runs at its highest available context window rather than an artificially capped value.
3. **Given** the full catalog is available, **When** the user inspects the candidate pool, **Then** every chat-capable model the account exposes is considered for allocation, spanning all vendors and capability tiers present in the catalog.
4. **Given** a turn is in progress, **When** a module is about to be called, **Then** the allocation is resolved without blocking the interactive turn (allocation decisions are cached/looked up, not computed synchronously on the hot path unless a cache miss forces it).

---

### User Story 3 - Learn and Refine Allocations Over Time (Priority: P2)

The selector improves its allocations as the user works. Following the paper's approach, a model from the candidate pool acts as an LLM "diagnoser": after compound-system runs, it reviews which module produced errors or weak output and updates that module's assigned model toward the one with the best estimated per-module performance. This optimization runs in the background, bounded by a configurable budget, and converges toward an allocation where each module uses its strongest model. Users see allocations get better without manual tuning, and a stale or never-benefiting allocation is eventually replaced.

**Why this priority**: Learning is what produces the paper's 5%–70% gains; a one-shot static allocation captures only a fraction of the value. It is P2 because the feature is useful (P1) even with a sensible initial allocation, while learning unlocks the full benefit.

**Independent Test**: Enable the selector with a small learning budget; run several turns that exercise a repeating module; verify that the diagnoser runs within budget and that at least one module's allocation changes in the direction of better estimated per-module performance, with the budget usage recorded.

**Acceptance Scenarios**:

1. **Given** the selector is enabled with a non-zero learning budget, **When** the agent completes turns that exercise the compound system, **Then** the diagnoser evaluates per-module performance using the module inputs/outputs and updates at least one allocation toward a better-performing model.
2. **Given** the diagnoser has run, **When** a module's currently-assigned model is not its best estimated performer, **Then** the selector nominates that module and reallocates it to the model with the highest estimated module-wise performance, repeating until no improvement is found or the budget is exhausted.
3. **Given** a learning budget is set, **When** optimization runs, **Then** it never exceeds the configured number of diagnoser/model calls and never blocks an interactive turn.
4. **Given** allocations have changed, **When** the user reviews the allocation history, **Then** each change shows the module, the previous and new model, and the diagnoser's reasoning.

---

### User Story 4 - Transparency and Control Over Allocations (Priority: P2)

A user wants to understand and steer the selector. They can view the current allocation map (which model is assigned to which module and why), see the diagnoser's recent judgments, and override the selector for any single module by pinning a specific model. Pinning lets a user lock in a known-good model for a task they care about while letting the selector continue to optimize everything else. The selector respects pins and never overrides them during learning.

**Why this priority**: Trust and debuggability. Users will not leave an opaque auto-allocator running on their account without visibility and an override path.

**Independent Test**: Enable the selector; pin one module to a specific catalog model via `/llm-selector`; run the diagnoser/learning step; verify the pinned module's model is unchanged while others may change, and the pin is visible in the allocation report.

**Acceptance Scenarios**:

1. **Given** the selector is enabled, **When** the user requests the current allocation, **Then** a report lists every module, its assigned model, whether it is pinned, and a short reason for the choice.
2. **Given** the user wants a specific model for one module, **When** they pin it through `/llm-selector`, **Then** the pin is persisted, applied immediately to subsequent calls of that module, and exempt from all automatic reallocation.
3. **Given** one or more modules are pinned, **When** the learning loop runs, **Then** pinned modules are never reallocated and the diagnoser only considers unpinned modules for improvement.
4. **Given** the diagnoser has produced judgments, **When** the user inspects recent diagnostics, **Then** they can see, per evaluated module, whether the diagnoser flagged an error, the model it implicated, and its rationale.

---

### User Story 5 - Graceful Degradation and Safe Coexistence (Priority: P3)

The selector must coexist safely with everything else: the existing provider `auto` cost-routing, manual per-task model configuration, credential handling, and per-conversation prompt caching. When the catalog is unreachable, a model is rate-limited or removed, or the diagnoser fails, the selector degrades gracefully — falling back to the most recent good allocation, a sensible default tier, or the user's configured model — without crashing a turn or corrupting the conversation. It never sends an unroutable or non-existent model id to the API.

**Why this priority**: Robustness is required for the feature to ship, but the happy path (Stories 1–2) carries the primary value; this governs failure modes once enabled.

**Independent Test**: Enable the selector; simulate the catalog fetch failing or a previously-allocated model returning a permanent error; verify the affected module falls back to a valid model and the turn completes, and that the failed allocation is marked for re-evaluation.

**Acceptance Scenarios**:

1. **Given** the selector is enabled, **When** the live catalog cannot be fetched, **Then** the selector falls back to the last-known-good allocation or the provider's curated `fallback_models` list and continues operating, logging the fallback.
2. **Given** a module's allocated model returns a permanent error (not found / removed), **When** the module is next called, **Then** the selector substitutes a feasible fallback model for that call and re-evaluates the allocation.
3. **Given** the user has manually configured a specific model for an auxiliary task (e.g., an `auxiliary.<task>.model` config key), **When** the selector is enabled, **Then** the explicit manual configuration is respected as an implicit pin and not overridden.
4. **Given** a long-running conversation is active, **When** the selector is toggled or reallocates a module, **Then** past messages are never mutated, reordered, or supplemented with synthetic messages, and the system prompt remains byte-stable for the life of the conversation.

### Edge Cases

- What happens when the catalog contains only a single eligible model? The selector detects that no cross-module diversity is possible, reports this, and effectively becomes a no-op pass-through while remaining "enabled."
- What happens when two modules would be allocated the same model but the user expects diversity (e.g., a multi-agent debate where identical generators reduce coverage)? The selector can assign distinct models to sibling modules of the same type when diversity improves estimated compound-system performance.
- What happens when the diagnoser model is itself removed from the catalog or errors? The selector selects a replacement diagnoser from the versatile tier and continues; if none is available it suspends learning (allocation still works from the last map).
- What happens when a turn's request exceeds every model's context window? The selector picks the roomiest feasible model for that module and lets the normal context-compression path engage, rather than failing.
- What happens when the user switches to a provider that exposes no live catalog while the selector is enabled? The selector auto-disables with a clear notice, since its candidate pool is catalog-specific.
- How does the selector interact with rate limits / premium-request budgets across many models? Allocations should account for feasibility signals and prefer models that are reachable, surfacing a warning if the candidate pool is constrained by account limits.
- What happens when a globally-shared allocation references a model not in the active profile's catalog? Because the map is global across profiles (which may have different accounts/entitlements), a learned allocation can reference a model the active profile cannot access. The selector detects this at load time, treats the entry as stale, and re-resolves that module via the capability-scored cold-start default before any call, never sending an unavailable model id to the API.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: System MUST provide a `/llm-selector` slash command (with an alias, and reachable from the CLI per Constitution II) that reports the current enabled/disabled state, the candidate model pool size, and the active diagnoser model.
- **FR-002**: System MUST engage dynamic per-module model selection when the active model is `auto` (selected in the `joey model` picker or set as the configured default `model.model`) on a provider that exposes a live model catalog, and MUST allow the user to disable the dynamic allocation via `/llm-selector` (falling back to the existing cost-only `auto` routing) or by switching to a concrete model — in every case taking effect on the next turn without altering any prior conversation context.
- **FR-003**: System MUST treat the active provider's live model catalog as the candidate pool, considering every chat-capable model the account exposes across all vendors and capability tiers (GitHub Copilot `/models`, OpenRouter `/models`, or the equivalent endpoint of the configured provider).
- **FR-004**: System MUST model the agent's compound system as a set of distinct task modules (the main turn plus each auxiliary/side-LLM call site and delegated subagent goal) and allocate a model to each module independently.
- **FR-005**: System MUST guarantee that an allocated model satisfies the hard requirements of its module (context window capacity, vision support when images are present, tool-calling support for agentic turns) before assigning it.
- **FR-006**: System MUST, when selection is enabled, route each module's LLM call to the model chosen by the selector rather than a blanket single-model default, and at least two distinct modules MUST be able to run on different models in the same turn. Selection is quality-first with a cost tie-break: the highest estimated per-module performer is chosen, and when two models are within 5% of the top estimated per-module performance (`p_j`), the cheaper model (lower cost tier, or lower billing multiplier within the same tier) is preferred.
- **FR-007**: System MUST resolve an allocation for a module without blocking the interactive turn on the hot path, using cached allocation decisions. Allocations are cached per-turn: every module in a single turn sees a consistent allocation map, and the cache is refreshed at the start of the next turn so diagnoser-driven reallocations take effect without churning mid-turn. On the first turn after enablement (cold start, before any diagnoser run), each module MUST be assigned the cheapest capable model for its role using the existing provider feasibility+cost scorer, so the feature is useful with zero warm-up and the diagnoser has a safe baseline to refine.
- **FR-008**: System MUST implement an LLM diagnoser (one of the candidate models) that estimates per-module performance from the module's inputs and outputs, and uses that estimate to reallocate modules toward better performers.
- **FR-009**: System MUST run allocation learning/optimization within a user-configurable budget of diagnoser/model calls and MUST run it asynchronously so it never blocks an interactive turn. The diagnoser MUST be triggered only by observable failure — a turn error or exception, an auxiliary/side-LLM call failure or exception, an empty or null model response, or a retry triggered by the existing retry mechanism (which signals the first attempt was unsatisfactory) — and MUST NOT fire on turns that produce a non-empty, non-error response, so the budget is spent where it can improve allocations. The diagnoser's LLM call MUST be dispatched as a detached `tokio::spawn` task inside `joey-llm-selector` that calls the provider client directly via `joey-providers` (reusing its existing auth/retry/backoff path), decoupled from the turn's lifetime so the hot path is never blocked; its result is written to the allocation map atomically on completion (FR-014).
- **FR-010**: System MUST iterate module reallocation (nominate the module with the best achievable per-module gain, reassign it, repeat) until no further improvement is found or the budget is exhausted.
- **FR-011**: System MUST record an allocation map (module → assigned model, pinned flag, reason) and expose it to the user via `/llm-selector`.
- **FR-012**: System MUST allow the user to pin a specific model to a specific module through `/llm-selector`, persist the pin, and exempt pinned modules from all automatic reallocation.
- **FR-013**: System MUST respect an existing explicit per-task model configuration (e.g., a user-set `auxiliary.<task>.model`) as an implicit pin that the selector does not override.
- **FR-014**: System MUST persist learned allocations and pins across restarts, so an enabled selector resumes with its previous allocation map. The allocation map is global — one shared map across all profiles on the machine (stored at the machine/user level under `~/.joey/`, honouring `JOEY_HOME`, not inside a single profile's home) — so learning transfers between profiles and maximizes compound-system improvement. When a map entry references a model absent from the active profile's live catalog, the selector MUST re-resolve that module's allocation via the cold-start scorer before use. The map MUST be stored as a single JSON file at `~/.joey/llm-selector/allocations.json`, written atomically (write-temp + rename, matching `joey-core::auth_store`'s pattern) so a concurrent turn-start read never sees a partial write; this JSON schema is a versioned on-disk public format (Constitution VII) and any breaking change requires a MAJOR bump + documented migration.
- **FR-015**: System MUST degrade gracefully when the catalog is unreachable (fall back to last-known-good allocation or the provider's curated `fallback_models` list), when an allocated model is removed or errors (substitute a feasible fallback and re-evaluate), and never send an unroutable model id to the API.
- **FR-016**: System MUST preserve per-conversation prompt caching and strict message-role alternation: toggling or reallocating MUST NOT mutate past messages, inject synthetic mid-loop messages, or alter the byte-stable system prompt.
- **FR-017**: System MUST auto-disable with a clear notice if the active provider exposes no live catalog, or if no eligible models exist in the catalog.
- **FR-018**: System MUST surface the diagnoser's recent judgments (per module: error flagged, implicated model, rationale) to the user for transparency.
- **FR-019**: System MUST default every allocated model to its highest available context window (the maximum the provider catalog exposes for that model), so each module uses the full capacity its assigned model offers unless the user explicitly caps it.
- **FR-020**: System MUST treat the `auto` model id as the activation sentinel for dynamic selection on a catalog-exposing provider, distinct from a concrete model, and MUST resolve it per-module at call time rather than sending the literal `auto` string to the API.
- **FR-021**: The selector engine, diagnoser, persisted allocation map, and the `ModelAllocator` trait MUST live in a new dedicated crate `joey-llm-selector` (under `crates/`), added to the workspace `members` list and independently buildable/testable (`cargo build -p joey-llm-selector` / `cargo test -p joey-llm-selector`). `joey-agent-core` (the turn loop and module call sites) MUST depend only on the narrow `ModelAllocator` trait exposed by `joey-llm-selector`, never on its internal engine — keeping coupling acyclic and minimized (Constitution VI). `joey-llm-selector` depends downward on `joey-core` (for `joey_home()` and config) and on the provider-catalog surface of `joey-providers`; `joey-cli` wires the `/llm-selector` command to the new crate's query API.

### Key Entities *(include if feature involves data)*

- **Candidate Model Pool**: The set of chat-capable models discovered in the user's provider catalog, each carrying its capability tier, context-window limit, and supported capabilities (vision, tool-calling). This is the set the selector allocates from.
- **Module (Task Node)**: A distinct LLM call site in the agent's compound system (e.g., the main reasoning turn, history compression, vision analysis, title generation, web extraction, session search, curator review, or a delegated subagent goal). Each module has hard capability requirements and is a node in the allocation map.
- **Allocation Map**: The persistent mapping of each module to its currently assigned model, plus a pinned flag and a human-readable reason. This is the selector's source of truth for routing and the artifact learning updates.
- **LLM Diagnoser**: One candidate model designated to estimate per-module performance by reviewing a module's inputs, outputs, the final result, and the desired outcome; its judgments drive reallocation.
- **Learning Budget**: A configurable bound on how many diagnoser/model calls the optimization may consume per run, ensuring cost stays predictable and the hot path is never blocked.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: Users can enable or disable dynamic selection in a single `/llm-selector` invocation, with the state change reflected on the very next turn 100% of the time.
- **SC-002**: When enabled on a catalog with multiple eligible models, the selector assigns different models to at least two distinct modules within a multi-module turn, demonstrating genuine per-task allocation.
- **SC-003**: Over a sequence of compound-system runs, a non-zero learning budget produces at least one module reallocation in the direction of higher estimated per-module performance, mirroring the paper's improving-allocation behaviour.
- **SC-004**: Every allocated model provably satisfies its module's hard capability requirements (context window, vision, tool-calling) — zero allocations assign an incapable model, including under vision and large-context turns.
- **SC-005**: The candidate pool utilization covers 100% of the chat-capable models the account exposes (no eligible model is silently excluded from consideration).
- **SC-006**: Enabling, disabling, or reallocating never alters, reorders, or supplements prior conversation messages — the conversation prefix remains byte-identical before and after any selector action.
- **SC-007**: A catalog fetch failure or an allocated-model error results in a graceful fallback that completes the turn, rather than a crash, in 100% of such failure cases.
- **SC-008**: Users can inspect the full allocation map and recent diagnoser judgments, and pin/unpin any module, entirely through `/llm-selector` without editing configuration files by hand.
- **SC-009**: Selecting the `auto` model engages dynamic per-module allocation 100% of the time on a catalog-exposing provider, with no further configuration required.
- **SC-010**: Every allocated model runs at its highest catalog-advertised context window by default — no model is silently run below its maximum capacity without an explicit user cap.

## Assumptions

- The user is running on a provider that exposes a live model catalog (GitHub Copilot or OpenRouter), since the candidate pool is that catalog; the selector is catalog-scoped by design and auto-disables otherwise.
- The existing provider catalog fetch mechanism (live `/models`, TTL-cached, used by the `joey model` picker and its `--refresh` flag) is reused as the source of the candidate pool rather than introducing a parallel model-discovery path.
- The existing auxiliary-task router (per-task provider/model resolution, and the `default_aux_model` field on each `ProviderProfile`) and the existing provider `auto` cost-routing are the integration surfaces for per-module allocation; the selector layers on top of them rather than replacing them. The `auto` sentinel already in use is reused as the activation id for dynamic selection, so selecting `auto` is the single switch users already know.
- A "module" is identified by its task role (main turn, compression, vision, title generation, etc.); the initial set of modules is the agent's current distinct LLM call sites, and the module set can grow as new call sites are added.
- The diagnoser defaults to a versatile-tier model (a good general judge) and is configurable; it does not require a separately-provisioned model.
- Allocation learning is best-effort and bounded: it improves assignments when the data supports it but always leaves the system in a usable state, even if the user disables learning (budget zero) and relies on a sensible default allocation.
- Existing prompt-caching, message-alternation, and credential-handling invariants are preserved untouched; the selector only changes *which model id* each module calls, not message structure or auth.
- Reasonable defaults are chosen for the learning budget, diagnoser model, and fallback model list so the feature is useful with zero configuration, while remaining fully configurable via `config.yaml` (dotted keys, e.g. `model.selector.enabled`, `model.selector.budget`).
- The selector lives in a new dedicated crate `joey-llm-selector` (FR-021); `joey-agent-core` consumes only its `ModelAllocator` trait, so adding the feature does not require editing the turn loop's shared core paths (Constitution VI).
- Each model's context window is read from the same catalog entry that already exposes the maximum prompt size; the selector uses the maximum value there as the default rather than introducing a separate window setting.
