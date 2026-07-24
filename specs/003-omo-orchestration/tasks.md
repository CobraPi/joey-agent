---

description: "Task list for Oh My OpenAgent Orchestration implementation"
---

# Tasks: Oh My OpenAgent Orchestration

**Input**: Design documents from `/specs/003-omo-orchestration/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: Tests are included inline with each user story (Constitution Principle IV: Test-First for New Crates). The constitution mandates unit tests alongside implementation and contract/round-trip tests for file-based state.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story. The 4-increment delivery plan (plan.md) maps to phases as follows: Inc 1 = Setup + Foundational + US1 + US2 + US3; Inc 2 = US6; Inc 3 = US5 + US8; Inc 4 = US4 + US7 + US9.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- **Workspace crate**: `crates/joey-omo/`
- **Modified crates**: `crates/joey-tui/`, `crates/joey-cli/`, `crates/joey-agent-core/`
- All new code in `crates/joey-omo/src/`; additive modifications to existing crates only

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Create the new joey-omo crate and wire it into the workspace.

- [x] T001 Create `crates/joey-omo/Cargo.toml` with workspace deps: joey-core (path), joey-providers (path), joey-tools (path), joey-agent-core (path), joey-orchestration (path), tokio, serde, serde_json, regex, chrono, uuid
- [x] T002 Add `joey-omo` to workspace `members` array in `Cargo.toml` root and add `joey-omo = { path = "crates/joey-omo" }` to `[workspace.dependencies]`
- [x] T003 [P] Create `crates/joey-omo/src/lib.rs` with module declarations and public API re-exports (AgentRegistry, OmoAgent, CategoryConfig, ModelRequirement, AgentMode, BoulderState, GoalState, IntentGate)
- [x] T004 [P] Create `crates/joey-omo/src/agents/prompts/mod.rs` with a trait for prompt variant resolution and directory placeholder

**Checkpoint**: `cargo build -p joey-omo` compiles (empty crate with module stubs).

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core types, model resolution, and the agent registry that ALL user stories depend on.

**CRITICAL**: No user story work can begin until this phase is complete.

- [x] T005 [P] Implement `ModelFamily` enum + `detect()` in `crates/joey-omo/src/models.rs`: Anthropic (claude-*), Gpt (gpt-*), Kimi (kimi-*), Glm (glm-*), Gemini (gemini-*), Minimax (minimax-*, MiniMax-*), Unknown; regex/prefix matching per contracts/model-fallback.md
- [x] T006 [P] Implement `FallbackEntry` struct (providers, model, variant) and `ModelRequirement` struct (fallback_chain, requires_any_model, requires_provider) in `crates/joey-omo/src/models.rs` per data-model.md
- [x] T007 Implement `AvailableModelSet` in `crates/joey-omo/src/models.rs`: a HashSet of available model IDs built from joey-providers catalog + configured provider profiles; provides `contains_exact(model)` and `contains_family(family)` methods
- [x] T008 Implement `resolve_model()` in `crates/joey-omo/src/models.rs`: walks fallback chain, tries exact model ID match first then family-level fuzzy match per entry; returns `Option<(model, variant)>` per contracts/model-fallback.md BC-006 through BC-010
- [x] T009 [P] Implement `AgentMode` enum (Primary, Subagent) and `ToolPermissions` struct (allow/deny per-tool map with deny precedence) in `crates/joey-omo/src/mode.rs` per data-model.md
- [x] T010 [P] Implement `OmoAgent` struct (name, display_name, mode, color, description, model_requirement, resolved_model, resolved_variant, temperature, max_tokens, tool_permissions) in `crates/joey-omo/src/agents/mod.rs` per data-model.md; include `is_available()` and `system_prompt(model)` method (delegates to prompt builder)
- [x] T011 Implement `CategoryConfig` struct (name, description, model_requirement, temperature, prompt_append) and the 11 built-in category definitions in `crates/joey-omo/src/categories.rs` per contracts/category-delegation.md
- [x] T012 Implement all 11 agent `ModelRequirement` constant definitions in `crates/joey-omo/src/agents/mod.rs`: port the exact fallback chains from OMO's agent-model-requirements.ts per contracts/model-fallback.md (sisyphus, hephaestus, oracle, librarian, explore, multimodal-looker, prometheus, metis, momus, atlas, sisyphus-junior)
- [x] T013 Implement all 11 category `ModelRequirement` constant definitions in `crates/joey-omo/src/categories.rs`: port the exact chains from OMO's category-model-requirements.ts per contracts/model-fallback.md (visual-engineering, ultrabrain, deep, artistry, quick, unspecified-low, unspecified-high, writing)
- [x] T014 Implement `AgentRegistry::build()` in `crates/joey-omo/src/agents/registry.rs`: constructs all 11 OmoAgent definitions, resolves each model via `resolve_model()`, marks skipped agents (resolved_model=None); enforces requiresProvider (Hephaestus) and requiresAnyModel (Sisyphus) constraints per contracts/agent-registry.md BC-001 through BC-005
- [x] T015 Implement `AgentRegistry` accessors in `crates/joey-omo/src/agents/registry.rs`: `all()` (11 agents), `available_primary()` (primary + resolved), `get(name)`, `categories()`, `tab_order()` (Sisyphus → Hephaestus → Prometheus → Atlas — Default prepended by caller)

**Tests for Foundational Phase**

- [x] T016 [P] Write unit test in `crates/joey-omo/src/models.rs`: ModelFamily::detect() correctly classifies claude-opus-4-8→Anthropic, gpt-5.6-sol→Gpt, kimi-k3→Kimi, glm-5→Glm, gemini-3.1-pro→Gemini, minimax-m3→Minimax, unknown-model→Unknown
- [x] T017 [P] Write unit test in `crates/joey-omo/src/models.rs`: resolve_model() with only Anthropic available resolves sisyphus chain entry 1 (claude-opus-4-8 exact match); with only Glm available resolves entry 4 (glm-5 family match); with no providers returns None
- [x] T018 [P] Write unit test in `crates/joey-omo/src/models.rs`: resolve_model() respects chain order — if entries 1-3 unavailable but entry 4 available, entry 4 is selected (not entry 1)
- [x] T019 [P] Write contract test in `crates/joey-omo/src/agents/registry.rs`: AgentRegistry::build() produces exactly 11 agents with canonical names; available_primary() returns only primary agents with resolved models
- [x] T020 [P] Write contract test in `crates/joey-omo/src/agents/registry.rs`: Hephaestus skipped when no OpenAI-class provider connected; Sisyphus skipped when no chain entry resolves

**Checkpoint**: Foundation ready — agent registry, model resolution, and categories all functional. `cargo test -p joey-omo` passes. User story implementation can now begin.

---

## Phase 3: User Story 1 - Agent Mode Switching via Tab (Priority: P1) TARGET MVP

**Goal**: User presses Tab to cycle through Default → Sisyphus → Hephaestus → Prometheus → Atlas, switching the active agent's prompt/model/permissions.

**Independent Test**: Launch the TUI, press Tab, select Prometheus from the picker overlay, status bar shows "Prometheus". Type a planning request — agent behaves as a read-only planner. Press Tab to cycle to Atlas.

### Implementation for User Story 1

- [x] T021 [P] [US1] Port Sisyphus default prompt to `crates/joey-omo/src/agents/prompts/sisyphus/default.rs` as a &str constant: port the full Sisyphus system prompt (Role, Behavior_Instructions with IntentGate Phase 0, delegation tables, todo discipline, anti-patterns, tone) from OMO's `sisyphus/default.ts`
- [x] T022 [P] [US1] Port Sisyphus model-family prompt variants to `crates/joey-omo/src/agents/prompts/sisyphus/`: glm_5_2.rs, gpt.rs, kimi_k3.rs, gemini.rs (port from OMO's sisyphus/*.ts); implement `sisyphus_prompt(model) -> &str` selecting variant by ModelFamily
- [x] T023 [P] [US1] Port Prometheus prompt to `crates/joey-omo/src/agents/prompts/prometheus/default.rs`: read-only planner identity, loads ulw-plan skill, interview protocol, never implements
- [x] T024 [P] [US1] Port Atlas default prompt to `crates/joey-omo/src/agents/prompts/atlas/default.rs`: conductor identity, delegates all implementation, wisdom accumulation, verification per OMO's `atlas/agent.ts` prompt builder
- [x] T025 [P] [US1] Port Atlas model-family variants to `crates/joey-omo/src/agents/prompts/atlas/`: gpt.rs, gemini.rs, kimi.rs, kimi_k3.rs, kimi_k2_7.rs, opus_4_7.rs, glm.rs; implement `atlas_prompt(model) -> &str`
- [x] T026 [P] [US1] Port Hephaestus prompts to `crates/joey-omo/src/agents/prompts/hephaestus/`: gpt.rs, gpt_5_4.rs, gpt_5_5.rs, gpt_5_6.rs (GPT-only); implement `hephaestus_prompt(model) -> &str` per OMO's hephaestus/agent.ts variant resolution
- [x] T027 [US1] Implement `OmoAgent::system_prompt(model)` dispatching to the correct prompt variant function in `crates/joey-omo/src/agents/mod.rs`: match on self.name → call the agent's prompt function with the resolved model
- [x] T028 [US1] Add agent-switching state to `crates/joey-tui/src/state.rs`: `active_agent_index: usize` (0=Default, 1=Sisyphus, ...), `agent_roster: Vec<DisplayAgent>`, `agent_picker_open: bool`, `agent_picker_cursor: usize`; add DisplayAgent and ActiveSubagentEntry structs per data-model.md
- [x] T029 [US1] Modify Tab key handler in `crates/joey-tui/src/app.rs handle_key()`: Tab opens agent picker overlay (if in Input mode and picker closed) or cycles cursor forward (if open); Shift+Tab cycles backward; Enter selects; Esc cancels per contracts/tab-picker.md BC-013 through BC-017
- [x] T030 [US1] Update `crates/joey-tui/src/app.rs handle_key()` for deferred switching: if Busy mode, Tab queues the switch for next turn (does not apply mid-execution) per BC-016
- [ ] T031 [US1] Move transcript focus to Up-arrow key in `crates/joey-tui/src/app.rs handle_key()`: when focus=Input and Up pressed, set focus=Transcript (replaces lost Tab-focus behavior per plan.md Complexity Tracking)
- [x] T032 [P] [US1] Implement agent picker overlay renderer in `crates/joey-tui/src/widgets.rs draw_agent_picker()`: centered popup listing Default + available primary agents in canonical order, cursor highlight, resolved model per row per contracts/tab-picker.md overlay layout
- [x] T033 [US1] Wire agent switch action to host in `crates/joey-tui/src/app.rs`: add `TuiAction::SwitchAgent(agent_name)` variant; host (repl.rs) rebuilds AgentConfig with the selected agent's model, prompt, permissions and emits AgentModeChanged event
- [ ] T034 [US1] Add agent mode switching to CLI in `crates/joey-cli/src/repl.rs`: handle Tab key in reedline (or `/agent` command) to print numbered agent list and accept numeric selection per contracts/tab-picker.md BC-018, BC-019
- [x] T035 [US1] Add `AgentModeChanged { agent_name, model }` variant to AgentEvent in `crates/joey-agent-core/src/events.rs` (additive enum extension per research.md Decision 9)
- [x] T036 [US1] Update TUI status bar in `crates/joey-tui/src/widgets.rs draw_status()` to show active agent display_name + color indicator
- [x] T037 [US1] Update help overlay in `crates/joey-tui/src/widgets.rs draw_help_overlay()` to document Tab = agent switching, Up = transcript focus

**Tests for User Story 1**

- [x] T038 [P] [US1] Write unit test in `crates/joey-omo/src/agents/mod.rs`: OmoAgent::system_prompt("claude-opus-4-8") for Sisyphus returns the default variant; system_prompt("glm-5") returns the GLM variant
- [x] T039 [P] [US1] Write contract test in `crates/joey-tui/tests/smoke.rs`: Tab key opens agent picker; Enter selects; agent_roster updated; status bar reflects selection
- [ ] T040 [P] [US1] Write unit test in `crates/joey-tui/src/state.rs`: active_agent_index cycles correctly (0→1→2→3→4→0); Shift+Tab cycles backward; picker cursor wraps

**Checkpoint**: Tab cycles through 5 agents in TUI; CLI has numbered menu parity. Each agent produces a distinct system prompt. US1 acceptance scenarios 1-5 pass.

---

## Phase 4: User Story 2 - The 11 Built-In Agents (Priority: P1)

**Goal**: All 11 agents defined with correct names, colors, modes, prompts, and tool permissions.

**Independent Test**: Query available agents — exactly 11 listed with correct names, descriptions, modes, colors. Sisyphus-Junior blocks `task` tool, allows `call_omo_agent`.

### Implementation for User Story 2

- [x] T041 [P] [US2] Port Oracle prompt to `crates/joey-omo/src/agents/prompts/oracle.rs`: read-only architecture consultant identity
- [x] T042 [P] [US2] Port Librarian prompt to `crates/joey-omo/src/agents/prompts/librarian.rs`: documentation/OSS search agent
- [x] T043 [P] [US2] Port Explore prompt to `crates/joey-omo/src/agents/prompts/explore.rs`: fast codebase grep agent
- [x] T044 [P] [US2] Port Multimodal-Looker prompt to `crates/joey-omo/src/agents/prompts/multimodal.rs`: vision/screenshot analysis agent
- [x] T045 [P] [US2] Port Metis prompt to `crates/joey-omo/src/agents/prompts/metis.rs`: gap analyzer identity
- [x] T046 [P] [US2] Port Momus prompt to `crates/joey-omo/src/agents/prompts/momus.rs`: plan reviewer identity
- [x] T047 [P] [US2] Port Sisyphus-Junior default prompt to `crates/joey-omo/src/agents/prompts/junior/default.rs`: focused executor, no delegation, todo obsession, verification gate
- [x] T048 [P] [US2] Port Sisyphus-Junior model-family variants to `crates/joey-omo/src/agents/prompts/junior/`: kimi_k3.rs, kimi_k2_7.rs, kimi_k2_6.rs, gpt.rs, gpt_5_5.rs, gpt_5_4.rs, gemini.rs, glm_5_2.rs; implement `junior_prompt(model, use_task_system) -> &str` per OMO's sisyphus-junior/agent.ts
- [x] T049 [US2] Implement Sisyphus-Junior tool permissions in `crates/joey-omo/src/agents/mod.rs`: deny `task`/delegate_task, allow `call_omo_agent`, default-deny all other delegation per contracts/agent-registry.md BC-005
- [x] T050 [US2] Register all 7 subagent definitions in `crates/joey-omo/src/agents/registry.rs`: oracle, librarian, explore, multimodal-looker, metis, momus, sisyphus-junior with correct names, display_names, colors (#20B2AA for Junior), modes (Subagent), model requirements, tool permissions
- [x] T051 [US2] Define agent colors and display names in `crates/joey-omo/src/agents/registry.rs`: Atlas=#10B981, Hephaestus=#D97706, Sisyphus-Junior=#20B2AA, others per OMO source

**Tests for User Story 2**

- [x] T052 [P] [US2] Write contract test in `crates/joey-omo/src/agents/registry.rs`: AgentRegistry::all() returns exactly 11 agents with canonical names in {sisyphus, hephaestus, prometheus, atlas, oracle, librarian, explore, multimodal-looker, metis, momus, sisyphus-junior}
- [x] T053 [P] [US2] Write unit test in `crates/joey-omo/src/agents/mod.rs`: Sisyphus-Junior tool_permissions denies `task` and allows `call_omo_agent`
- [x] T054 [P] [US2] Write unit test in `crates/joey-omo/src/agents/mod.rs`: each primary agent's system_prompt() returns non-empty string containing its identity marker (e.g., "Sisyphus" for sisyphus, "Prometheus" for prometheus)

**Checkpoint**: All 11 agents defined and queryable. US2 acceptance scenarios 1-5 pass. INCREMENT 1 PARTIALLY COMPLETE (registry + agents done).

---

## Phase 5: User Story 3 - Category-Based Delegation System (Priority: P1)

**Goal**: Agents delegate by category name, not model name. Categories route to the right model automatically via Sisyphus-Junior.

**Independent Test**: Sisyphus delegates with `category="quick"` — Sisyphus-Junior spawns with the quick category's model chain and prompt append.

### Implementation for User Story 3

- [x] T055 [US3] Implement category delegation routing in `crates/joey-omo/src/categories.rs`: `resolve_category(name, registry) -> Option<(model, CategoryConfig)>` that resolves the category's model chain and returns config
- [x] T056 [US3] Implement `validate_delegation(category, subagent_type)` in `crates/joey-omo/src/categories.rs`: returns error if both specified (mutual exclusivity per contracts/category-delegation.md BC-011, BC-012)
- [ ] T057 [US3] Wire category delegation into `crates/joey-orchestration/src/delegation_tool.rs`: accept optional `category` and `subagent_type` fields in tool args; when category present, resolve category model+prompt_append, construct DelegationRequest with resolved model and Junior's toolsets; when subagent_type present, resolve that agent's model
- [ ] T058 [US3] Add category support to the delegate_task tool schema in `crates/joey-orchestration/src/delegation_tool.rs`: add `category` (string, optional), `subagent_type` (string, optional), `load_skills` (array, optional) properties to the JSON schema; validate mutual exclusivity
- [x] T059 [US3] Add `CategoryDelegation { category, model }` event variant to `crates/joey-agent-core/src/events.rs`
- [ ] T060 [US3] Update prompt append injection in `crates/joey-orchestration/src/subagent.rs`: when category has a prompt_append, prepend it to the subagent's system prompt alongside any loaded skills

**Tests for User Story 3**

- [x] T061 [P] [US3] Write unit test in `crates/joey-omo/src/categories.rs`: resolve_category("quick") with only Gpt-mini available returns the quick model; resolve_category("visual-engineering") with Gemini available returns Gemini
- [x] T062 [P] [US3] Write unit test in `crates/joey-omo/src/categories.rs`: validate_delegation(Some("quick"), Some("oracle")) returns Err; validate_delegation(Some("quick"), None) returns Ok
- [ ] T063 [P] [US3] Write contract test in `crates/joey-orchestration/tests/`: delegation with category="quick" spawns Sisyphus-Junior with quick model and prompt_append included in system prompt

**Checkpoint**: Category delegation routes correctly. US3 acceptance scenarios 1-4 pass. INCREMENT 1 COMPLETE.

---

## Phase 6: User Story 6 - Agent Activity Panel (Priority: P1)

**Goal**: Bottom-right TUI panel shows live agent/sub-agent activity in an elegant split layout.

**Independent Test**: Sisyphus fires 3 parallel explore agents. Panel immediately shows 3 running entries with spinners, types, elapsed times, phases. As each completes, status flips to "done".

### Implementation for User Story 6

- [ ] T064 [P] [US6] Implement `ActiveSubagentEntry` tracking in `crates/joey-tui/src/state.rs`: Vec<ActiveSubagentEntry> with id, agent_type, category, status (Running/Done/Failed), phase, model, iterations, started; add `subagent_entries` field to App
- [ ] T065 [US6] Update `App::apply()` in `crates/joey-tui/src/state.rs` to handle orchestration events: SubagentSpawn→add entry; SubagentComplete→entry.Done; SubagentFailed→entry.Failed; IterationStart→update iterations; ApiCallStart→phase="querying model"; ToolStart→phase="running tool: X"; CategoryDelegation→add entry with category label; AgentModeChanged→update active_agent_index
- [ ] T066 [US6] Implement `draw_omo_panel()` in `crates/joey-tui/src/widgets.rs` (or `crates/joey-tui/src/omo_panel.rs`): split layout — top: pinned active agent (display_name, color, model) + concurrency indicator (X/Y slots); below: full roster (idle) or live subagent entries (active) per contracts/activity-panel.md
- [ ] T067 [US6] Implement roster rendering (idle state) in `crates/joey-tui/src/widgets.rs`: all 11 agents listed with name, mode, resolved model; skipped agents dimmed with "(unavailable)" per contracts/activity-panel.md idle state layout
- [ ] T068 [US6] Implement subagent entry rendering (active state) in `crates/joey-tui/src/widgets.rs`: spinner glyph (◷/✓/✗), agent_type, status, elapsed time, phase, model, category label if present; roster condenses/collapses when active per contracts/activity-panel.md active state layout
- [ ] T069 [US6] Implement job board rendering in `crates/joey-tui/src/widgets.rs`: during Atlas execution, show task titles, status (pending/running/done/failed), tool-call count, last tool per contracts/activity-panel.md job board
- [ ] T070 [US6] Implement wisdom counter + concurrency indicator in `crates/joey-omo/src/widgets.rs`: "X/Y slots" with idle/active/queued status icon; learnings count during plan execution
- [ ] T071 [US6] Replace `draw_activity` call in `crates/joey-tui/src/app.rs draw()` with `draw_omo_panel`; keep the existing sidebar region (Length 34 when width >= 72) but render the richer split panel into it
- [ ] T072 [US6] Implement graceful degradation in `crates/joey-tui/src/widgets.rs draw_omo_panel()`: narrow terminal (<72 cols) hides panel entirely (existing); short height (<15 rows) truncates roster to 5, subagent entries to 3; very short (<9 rows) collapses to single line per contracts/activity-panel.md
- [x] T073 [P] [US6] Implement CLI inline agent activity summaries in `crates/joey-cli/src/omo_render.rs`: print one-line summaries as events arrive (e.g., `[explore] spawned → running (model: glm-5)`, `[explore] done (4.2s)`) using ANSI colors matching TUI agent colors per contracts/activity-panel.md CLI parity
- [x] T074 [US6] Wire CLI event rendering in `crates/joey-cli/src/repl.rs`: route orchestration AgentEvents to omo_render functions for inline printing during turns

**Tests for User Story 6**

- [ ] T075 [P] [US6] Write unit test in `crates/joey-tui/src/state.rs`: SubagentSpawn event adds an entry with Running status; SubagentComplete sets it to Done; 3 parallel spawns create 3 entries
- [ ] T076 [P] [US6] Write unit test in `crates/joey-tui/src/state.rs`: CategoryDelegation event adds entry with category label populated
- [ ] T077 [P] [US6] Write contract test in `crates/joey-tui/tests/smoke.rs`: panel renders without panic at 80×24, 120×40, and 70×20 (degraded); roster shows all 11 agents when idle

**Checkpoint**: Bottom-right panel shows live subagent activity. US6 acceptance scenarios 1-6 pass. INCREMENT 2 COMPLETE.

---

## Phase 7: User Story 7 - Model Fallback Chains (Priority: P2)

**Goal**: Exact OMO fallback chains with family-level fuzzy matching against joey-agent's providers.

**Independent Test**: Configure only Z.ai/GLM. Sisyphus resolves to glm-5 (4th in chain) with GLM prompt variant. Hephaestus skipped (requires OpenAI).

### Implementation for User Story 7

- [ ] T078 [US7] Implement `AvailableModelSet::build()` in `crates/joey-omo/src/models.rs`: query joey-providers for connected provider profiles + copilot model catalog; build HashSet of exact model IDs; build family→model-id lookup for fuzzy matching per research.md Decision 2
- [ ] T079 [US7] Wire `AvailableModelSet` construction into `crates/joey-cli/src/repl.rs` (and oneshot.rs): at agent startup, build the set from config + provider catalog, pass to AgentRegistry::build()
- [ ] T080 [US7] Implement user model override bypass in `crates/joey-omo/src/agents/registry.rs`: if config has `omo.agents.<name>.model` set, use it directly (skip chain) per contracts/model-fallback.md BC-009
- [ ] T081 [US7] Implement prompt variant selection by model family in each agent's prompt module: `sisyphus_prompt(model)` detects family and returns matching variant; default variant when no match per research.md Decision 10 resolution order

**Tests for User Story 7**

- [ ] T082 [P] [US7] Write integration test in `crates/joey-omo/src/models.rs`: with AvailableModelSet containing only glm-5, resolve_model(sisyphus_requirement) returns (glm-5, None) via family match on entry 4; Hephaestus requiresProvider check fails → skipped
- [ ] T083 [P] [US7] Write unit test in `crates/joey-omo/src/agents/registry.rs`: user override config `omo.agents.sisyphus.model: "custom-model"` → sisyphus.resolved_model == "custom-model" (chain bypassed)

**Checkpoint**: Model fallback chains resolve with fuzzy family matching. US7 acceptance scenarios 1-5 pass.

---

## Phase 8: User Story 5 - Orchestration Pipeline (Priority: P2)

**Goal**: Full plan→execute pipeline: Prometheus plans, /start-work activates Atlas, Atlas delegates, wisdom accumulates.

**Independent Test**: Prometheus produces a plan in `.omo/plans/`. `/start-work` activates Atlas, delegates tasks, learnings accumulate in `.omo/notepads/`, tasks complete with verification.

### Implementation for User Story 5

- [x] T084 [P] [US5] Implement `BoulderState` struct and JSON persistence in `crates/joey-omo/src/boulder.rs`: BoulderState { works, version }, BoulderWork { id, plan_path, plan_name, session_id, agent, worktree_path, status, started_at }, BoulderWorkStatus enum; `read(dir)`, `write(dir, state)`, `create_work()`, `complete_work()`, `select_active()` per data-model.md
- [x] T085 [P] [US5] Implement `GoalState` struct and JSON persistence in `crates/joey-omo/src/goal.rs`: GoalState { session_id, objective, status, set_at }, GoalStatus (Active/Paused); `read(dir)`, `write(dir, state)`, parse_goal_command() for set/pause/resume/clear/show per contracts/slash-commands.md
- [x] T086 [P] [US5] Implement `NotepadStore` in `crates/joey-omo/src/notepad.rs`: append-only markdown files under `.omo/notepads/{plan}/` (learnings.md, decisions.md, issues.md, verification.md, problems.md); `append(plan, file, content)`, `read_all(plan) -> String` for passing wisdom forward
- [x] T087 [P] [US5] Implement plan parser in `crates/joey-omo/src/plan_parser.rs`: parse `.omo/plans/{name}.md`; extract task rows matching `- [ ] N. <title>` (implementation) and `- [ ] F<num>. <title>` (final verification); extract dependency graph if present; return ParsedPlan { tasks, dependencies } per contracts/orchestration-pipeline.md BC-031
- [x] T088 [US5] Implement wisdom extraction in `crates/joey-omo/src/notepad.rs`: after a task completes, extract learnings from the delegation result summary (conventions, successes, failures, gotchas); append to appropriate notepad files
- [x] T089 [US5] Implement Atlas delegation flow in `crates/joey-omo/src/agents/atlas.rs`: read plan, for each task in dependency order delegate to worker (Sisyphus-Junior via category or Oracle/Explore/Librarian via subagent_type), pass accumulated wisdom in delegation context, verify result, extract learnings per contracts/orchestration-pipeline.md BC-026 through BC-032
- [x] T090 [US5] Wire session continuation IDs into the delegate_task tool in `crates/joey-orchestration/src/delegation_tool.rs`: add `task_id` (string, optional) parameter to the tool JSON schema; when provided, load the referenced session's message history from SessionDb (existing spec 002 persist infrastructure) and pass it as the subagent's initial context instead of starting fresh; enables Atlas to follow up with previously-spawned subagents (fix failures, ask questions, retry verification) per FR-030 and contracts/orchestration-pipeline.md
- [x] T091 [US5] Wire OMO concurrency configuration into SubagentManager in `crates/joey-orchestration/src/manager.rs`: add `omo.background_task.defaultConcurrency` (default 5), `omo.background_task.providerConcurrency` (optional map), and `omo.background_task.modelConcurrency` (optional map) to ManagerConfig::from_config(); the existing Arc<Semaphore> from spec 002 enforces the global limit; provider/model-level overrides create additional per-key semaphores keyed by model/provider routing key per FR-031
- [x] T092 [US5] Add `/start-work` command handler in `crates/joey-cli/src/repl.rs`: parse plan name arg; read plan file or auto-resume from boulder state; activate Atlas agent; create BoulderWork; emit BoulderWorkStarted/Resumed event
- [x] T093 [US5] Add `/start-work` to slash command registry in `crates/joey-cli/src/slash.rs`: Category "Session", args_hint "[plan-name]", implemented true
- [x] T094 [US5] Add `/goal` command handler in `crates/joey-cli/src/repl.rs`: parse subcommand (set/pause/resume/clear/show); manage GoalState; inject continuation prompt on idle turns when goal is Active per contracts/slash-commands.md BC-022, BC-023
- [x] T095 [US5] Flip `/goal` to implemented in `crates/joey-cli/src/slash.rs`: change implemented field from false to true
- [x] T096 [US5] Add goal continuation injection in `crates/joey-cli/src/repl.rs`: when goal is Active and agent is idle, inject the goal objective as a continuation prompt per contracts/orchestration-pipeline.md
- [x] T097 [US5] Add boulder/goal/wisdom event variants to `crates/joey-agent-core/src/events.rs`: BoulderWorkStarted, BoulderWorkResumed, BoulderWorkCompleted, GoalSet, GoalCleared, WisdomAccumulated (additive per research.md Decision 9)
- [x] T098 [US5] Wire TUI to render boulder/goal events in `crates/joey-tui/src/state.rs`: BoulderWorkStarted→show job board; WisdomAccumulated→update learnings counter

**Tests for User Story 5**

- [x] T099 [P] [US5] Write contract test in `crates/joey-omo/src/boulder.rs`: BoulderState round-trip — write then read produces identical state; missing file returns empty state (not error)
- [x] T100 [P] [US5] Write contract test in `crates/joey-omo/src/goal.rs`: parse_goal_command("set Ship feature") → setObjective; parse_goal_command("pause") → Paused; parse_goal_command("") → show
- [x] T101 [P] [US5] Write unit test in `crates/joey-omo/src/notepad.rs`: append() adds content without overwriting; read_all() returns concatenated content from all 5 files
- [x] T102 [P] [US5] Write unit test in `crates/joey-omo/src/plan_parser.rs`: parse a sample plan markdown and extract correct task list with dependencies

**Checkpoint**: Full plan→execute pipeline works end-to-end. US5 acceptance scenarios 1-5 pass. INCREMENT 3 COMPLETE.

---

## Phase 9: User Story 4 - IntentGate and Working Modes (Priority: P2)

**Goal**: IntentGate classifies intent; ultrawork mode activates via keyword; Prometheus interview mode works.

**Independent Test**: User types `ulw implement a CLI` — agent responds "ULTRAWORK MODE ENABLED!", fires explore, delegates, verifies.

### Implementation for User Story 4

- [x] T103 [P] [US4] Implement IntentGate keyword detection in `crates/joey-omo/src/intent_gate.rs`: detect `ultrawork`/`ulw`, `hyperplan`, `hyperplan ultrawork` combo, `team` keywords in user messages; return detected keyword type + message per OMO's keyword-detector/ constants.ts
- [x] T104 [P] [US4] Port ultrawork default prompt to `crates/joey-omo/src/agents/prompts/ultrawork/default.rs`: port the full ultrawork instruction set from OMO's prompts/ultrawork/default.md (absolute certainty, mandatory plan agent, TDD, manual QA, zero tolerance)
- [x] T105 [P] [US4] Port ultrawork model-family variants to `crates/joey-omo/src/agents/prompts/ultrawork/`: gpt.rs, gemini.rs, glm.rs, planner.rs; implement `ultrawork_prompt(model_family) -> &str`
- [x] T106 [US4] Implement ultrawork activation guard in `crates/joey-omo/src/intent_gate.rs`: valid on Default, Sisyphus, Hephaestus, Atlas; IGNORED on Prometheus (read-only planner incompatible) per FR-022 and spec clarification Q2
- [ ] T107 [US4] Wire keyword detection into prompt building in `crates/joey-cli/src/repl.rs`: before each turn, scan user message for keywords; if ultrawork detected and active agent is not Prometheus, inject ultrawork instruction set into system prompt; emit AgentEvent::Notice for "ULTRAWORK MODE ENABLED!" first-response requirement
- [ ] T108 [US4] Implement `@plan` prefix detection in `crates/joey-cli/src/repl.rs`: when user input starts with `@plan`, delegate to Prometheus to create a plan (equivalent to switching to Prometheus + describing work) per contracts/slash-commands.md
- [ ] T109 [US4] Port the `ulw-plan` skill to `skills/ulw-plan/SKILL.md` and references: port from OMO's `omo-codex/plugin/skills/ulw-plan/` (SKILL.md, references/full-workflow.md, intent-clear.md, intent-unclear.md) per research.md Decision 6 and spec clarification Q5
- [ ] T110 [US4] Wire Prometheus to load ulw-plan skill: Prometheus system prompt references the skill; ensure skill-loading mechanism resolves it

**Tests for User Story 4**

- [x] T111 [P] [US4] Write unit test in `crates/joey-omo/src/intent_gate.rs`: detect "ultrawork" and "ulw" in messages; detect "hyperplan"; detect combo "hyperplan ultrawork"
- [x] T112 [P] [US4] Write unit test in `crates/joey-omo/src/intent_gate.rs`: ultrawork activation on Sisyphus returns Some(message); on Prometheus returns None (ignored)
- [ ] T113 [P] [US4] Write unit test verifying ulw-plan skill exists at `skills/ulw-plan/SKILL.md` with frontmatter and the mandatory opening announcement text

**Checkpoint**: Ultrawork activates on valid agents, ignored on Prometheus. IntentGate keyword detection works. US4 acceptance scenarios 1-4 pass.

---

## Phase 10: User Story 8 - Slash Commands (Priority: P3)

**Goal**: /start-work, @plan, /goal, ultrawork, hyperplan all functional in CLI and TUI.

**Independent Test**: `/start-work my-feature` activates Atlas on the plan. `/goal set Ship X` persists and injects on idle.

### Implementation for User Story 8

- [ ] T114 [US8] Integrate `/start-work`, `/goal`, `@plan`, ultrawork, hyperplan into the TUI (not just CLI): ensure slash commands work in TUI input box; TuiAction::Submit detects slash prefixes and routes to handlers in `crates/joey-tui/src/app.rs` and `crates/joey-cli/src/repl.rs`
- [x] T115 [US8] Add `/agents` command (or enhance existing) in `crates/joey-cli/src/slash.rs`: lists all 11 agents with resolved models, modes, availability status; flip implemented to true for `/agents`
- [ ] T116 [US8] Verify CLI/TUI parity for all new commands: `/start-work`, `/goal`, `@plan`, `/agents`, Tab agent switching, keyword detection all work identically in both surfaces per Constitution Principle II

**Tests for User Story 8**

- [ ] T117 [P] [US8] Write integration test: `/start-work` with non-existent plan returns error, no state change; with 1 active work auto-resumes; with multiple asks user
- [ ] T118 [P] [US8] Write integration test: `/goal set` → continuation injected on next idle; `/goal pause` → no injection; `/goal resume` → injection resumes; `/goal clear` → removed

**Checkpoint**: All slash commands work in CLI and TUI with parity. US8 acceptance scenarios 1-5 pass.

---

## Phase 11: User Story 9 - Team Mode (Priority: P3, Optional)

**Goal**: Parallel multi-agent orchestration via shared mailbox + shared task list, OFF by default.

**Independent Test**: Enable team mode in config, define a team with 3 sisyphus-junior members, members coordinate via shared mailbox.

### Implementation for User Story 9

- [x] T119 [P] [US9] Implement team mode config types in `crates/joey-omo/src/team.rs`: TeamModeConfig (enabled, max_parallel_members=4, max_members=8, message limits, polling intervals, tmux_visualization), TeamSpec, TeamMember (kind: category/subagent_type, prompt)
- [x] T120 [US9] Implement team member eligibility validation in `crates/joey-omo/src/team.rs`: eligible (sisyphus, atlas, sisyphus-junior), conditional (hephaestus with permission), hard-reject (oracle, librarian, explore, multimodal-looker, metis, momus, prometheus) per contracts and spec FR-042
- [x] T121 [US9] Implement shared mailbox in `crates/joey-omo/src/team.rs`: message passing between team members via a shared in-memory + file-backed mailbox; TeamMailbox with send/receive/poll
- [x] T122 [US9] Implement shared task list in `crates/joey-omo/src/team.rs`: shared task coordination across members; TeamTaskList with claim/complete/status
- [x] T123 [US9] Wire team mode activation: when config `omo.team.enabled` is true, team infrastructure activates; otherwise invisible (default off) per FR-041

**Tests for User Story 9**

- [x] T124 [P] [US9] Write unit test in `crates/joey-omo/src/team.rs`: eligibility validation — sisyphus accepted, oracle rejected with clear error, hephaestus conditional
- [x] T125 [P] [US9] Write unit test in `crates/joey-omo/src/team.rs`: team mode disabled by default; no team infrastructure activates when enabled=false

**Checkpoint**: Team mode available but OFF by default. US9 acceptance scenarios 1-4 pass. INCREMENT 4 COMPLETE.

---

## Phase 12: Polish & Cross-Cutting Concerns

**Purpose**: Regression coverage, documentation, performance verification.

- [ ] T126 Run `cargo build --workspace` and fix any compilation errors across all modified crates
- [ ] T127 Run `cargo test --workspace` and ensure ALL existing tests pass (zero regressions per Constitution Principle VII and FR-047)
- [ ] T128 [P] Add regression test in `crates/joey-tui/tests/smoke.rs`: existing Tab-free keybindings (Ctrl+C, Ctrl+D, Esc, Enter, arrow keys) still work unchanged
- [ ] T129 [P] Add regression test: existing CLI flags and config keys still work — `joey --model`, `joey --tui`, `config.yaml` keys all honored
- [ ] T130 Run quickstart.md validation scenarios 1-7 end-to-end and verify all pass
- [ ] T131 [P] Performance check: time AgentRegistry::build() — verify <50ms; time Tab picker render — verify <16ms per frame; time model fallback resolution — verify <5ms per agent
- [ ] T132 [P] Verify narrow-terminal degradation: TUI at 70×20, 80×12, 50×10 renders without panic or layout breakage
- [ ] T133 Update help overlay text in `crates/joey-tui/src/widgets.rs` to document all new keybindings (Tab=agent switch, Shift+Tab=reverse, Up=transcript focus)
- [x] T134 Update `crates/joey-cli/src/slash.rs` help text for `/agents`, `/start-work`, `/goal` to show accurate descriptions

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately
- **Foundational (Phase 2)**: Depends on Setup — BLOCKS all user stories
- **US1 (Phase 3)**: Depends on Foundational — agent prompts + Tab switching
- **US2 (Phase 4)**: Depends on Foundational — subagent prompts + registry. Can run in PARALLEL with US1 (different files: prompts/ vs TUI)
- **US3 (Phase 5)**: Depends on Foundational + US2 (needs Sisyphus-Junior defined). Can run in PARALLEL with US1
- **US6 (Phase 6)**: Depends on Foundational + US2 (needs agent roster data). INCREMENT 2
- **US7 (Phase 7)**: Depends on Foundational (model resolution). Can run in PARALLEL with US6
- **US5 (Phase 8)**: Depends on US2 + US3 (needs agents + categories for delegation). INCREMENT 3
- **US4 (Phase 9)**: Depends on Foundational (prompts). Can run in PARALLEL with US5
- **US8 (Phase 10)**: Depends on US5 (slash commands reference pipeline). Can run in PARALLEL with US4
- **US9 (Phase 11)**: Depends on Foundational. INCREMENT 4 (optional)
- **Polish (Phase 12)**: Depends on all desired user stories complete

### User Story Completion Order (Incremental Delivery)

| Increment | Phases | User Stories | Deliverable |
|-----------|--------|-------------|-------------|
| Inc 1 | 1-5 | US1, US2, US3 | Agent registry + Tab switching + categories |
| Inc 2 | 6 | US6 | Agent activity panel |
| Inc 3 | 7-8 | US7, US5 | Model fallback + orchestration pipeline |
| Inc 4 | 9-11 | US4, US8, US9 | Ultrawork + slash commands + team mode |

### Within Each User Story

- Prompt ports ([P]) before prompt builder functions
- Registry/state before UI rendering
- Core implementation before integration glue
- Tests alongside implementation (Constitution Principle IV)

### Parallel Opportunities

- T003-T004: Setup module stubs (different files)
- T005-T013: Foundational types and chains (independent modules)
- T016-T020: Foundational tests (independent test functions)
- T021-T026: Prompt ports across agents (different files per agent)
- T041-T048: Subagent prompt ports (different files)
- US1 (TUI) and US2 (prompts) can proceed in parallel after Foundational
- US3 (categories) and US1 (Tab) can proceed in parallel after Foundational
- US6 (panel) and US7 (models) can proceed in parallel
- US4 (ultrawork) and US5 (pipeline) can proceed in parallel after their deps

---

## Parallel Example: Foundational Phase

```bash
# Launch all model/type tasks together (independent files):
Task: "Implement ModelFamily + detect() in crates/joey-omo/src/models.rs"
Task: "Implement AgentMode + ToolPermissions in crates/joey-omo/src/mode.rs"
Task: "Create OmoAgent struct in crates/joey-omo/src/agents/mod.rs"
Task: "Implement CategoryConfig in crates/joey-omo/src/categories.rs"

# Then chain (registry depends on all above):
Task: "Implement AgentRegistry::build() in crates/joey-omo/src/agents/registry.rs"
```

## Parallel Example: User Story 1 + User Story 2

```bash
# After Foundational, these can proceed in parallel:
# Developer A (TUI/CLI — US1):
Task: "Port Sisyphus prompt to crates/joey-omo/src/agents/prompts/sisyphus/default.rs"
Task: "Modify Tab handler in crates/joey-tui/src/app.rs"
Task: "Implement agent picker overlay in crates/joey-tui/src/widgets.rs"

# Developer B (subagent prompts — US2):
Task: "Port Oracle prompt to crates/joey-omo/src/agents/prompts/oracle.rs"
Task: "Port Librarian prompt to crates/joey-omo/src/agents/prompts/librarian.rs"
Task: "Port Sisyphus-Junior prompts to crates/joey-omo/src/agents/prompts/junior/"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup (T001-T004)
2. Complete Phase 2: Foundational (T005-T020) — CRITICAL, blocks all stories
3. Complete Phase 3: User Story 1 (T021-T040) — Tab agent switching
4. **STOP and VALIDATE**: Press Tab in TUI, cycle through 5 agents, verify distinct prompts
5. Deploy/demo if ready — user can switch agents interactively

### Incremental Delivery

1. Setup + Foundational → Foundation ready (registry, models, categories)
2. + US1 + US2 + US3 → **Increment 1**: Agents defined, Tab works, categories route (MVP!)
3. + US6 → **Increment 2**: Activity panel shows live subagent activity
4. + US7 + US5 → **Increment 3**: Model fallback + plan→execute pipeline
5. + US4 + US8 + US9 → **Increment 4**: Ultrawork + slash commands + team mode
6. Each increment builds green (`cargo build --workspace`) and tests green

---

## Notes

- [P] tasks = different files, no dependencies on incomplete tasks
- [Story] label maps task to specific user story for traceability
- Each user story is independently completable and testable
- Tests are written alongside implementation (Constitution Principle IV)
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence
- All new code in `crates/joey-omo/`; modifications to existing crates are strictly additive (Constitution Principle VII)

---

## Phase 13: Convergence

**Purpose**: Close gaps between the implemented code and the spec/plan/tasks. These tasks were identified by `/speckit-converge` assessing the current codebase against FR-001 through FR-048 and the acceptance scenarios.


- [ ] T135 Wire `category` and `subagent_type` fields into the delegate_task tool schema and dispatch path in `crates/joey-orchestration/src/delegation_tool.rs`: when `category` present, resolve via `joey_omo::resolve_category()` to get model + prompt_append, construct DelegationRequest with resolved model and Sisyphus-Junior's toolsets; when `subagent_type` present, resolve that agent's model; validate mutual exclusivity (BC-011) at dispatch time per FR-013, FR-014, T057-T058 (missing)
- [ ] T136 Implement Atlas delegation flow in a new `crates/joey-omo/src/atlas.rs`: read parsed plan, delegate tasks in dependency order via category or subagent_type, pass accumulated wisdom in delegation context, verify results, extract learnings to notepad; add session continuation `task_id` parameter to delegate_task tool schema and load referenced session history from SessionDb as initial context per FR-028, FR-029, FR-030, T089-T090 (missing)
- [ ] T137 Port the `ulw-plan` skill to `skills/ulw-plan/SKILL.md` with references (full-workflow.md, intent-clear.md, intent-unclear.md) from OMO source; wire Prometheus system prompt to load and reference this skill via the existing skill-loading mechanism per FR-025, T109-T110 (missing)
- [ ] T138 Implement `draw_omo_panel()` in `crates/joey-tui/src/widgets.rs` as a split layout replacing `draw_activity`: top section pins active agent (display_name, color, model) + concurrency indicator (X/Y slots); below section shows full 11-agent roster (idle) or live subagent entries (active) with spinner glyphs; add job board for Atlas execution; implement graceful degradation for narrow/short terminals; replace the `draw_activity` call in `app.rs draw()` with `draw_omo_panel` per FR-033-FR-039, T066-T072 (partial)
- [ ] T139 Wire orchestration events into `subagent_entries` in `crates/joey-tui/src/state.rs App::apply()`: SubagentSpawn→add ActiveSubagentEntry with Running status; SubagentComplete→entry.Done; SubagentFailed→entry.Failed; IterationStart→update iterations; ApiCallStart→phase="querying model"; ToolStart→phase="running tool: X"; CategoryDelegation→add entry with category label; AgentModeChanged→update active_agent_index per FR-035, T064-T065 (missing)
- [x] T140 Implement `AvailableModelSet::build()` in `crates/joey-omo/src/models.rs` that queries joey-providers for connected provider profiles and builds the HashSet of exact model IDs + family→model lookup; wire this into `crates/joey-cli/src/repl.rs` and `oneshot.rs` at startup to build the set from config, pass to AgentRegistry::build(), and populate `app.agent_roster` via `build_agent_roster_from_registry()` so the Tab picker shows agents per FR-005, FR-007, T078-T079 (done: `from_connected()` + `populate_agent_roster()` in `crates/joey-cli/src/tui.rs`; repl/oneshot don't render a Tab picker so they were not changed)
- [ ] T141 Wire keyword detection into prompt building in `crates/joey-cli/src/repl.rs`: before each turn, scan user message via `joey_omo::detect_keyword()`; if ultrawork detected and active agent is not Prometheus, inject `joey_omo::agents::prompts::ultrawork_prompt()` into the system prompt; emit `AgentEvent::Notice("ULTRAWORK MODE ENABLED!")` as first-response requirement (BC-024) per FR-022, FR-024, T107 (missing)
- [ ] T142 Implement `@plan` prefix detection in `crates/joey-cli/src/repl.rs`: when user input starts with `@plan`, delegate to Prometheus to create a plan (equivalent to switching to Prometheus + describing work); do not start execution per T108 (missing)
- [ ] T143 Wire goal continuation injection on idle turns in `crates/joey-cli/src/repl.rs`: when GoalState is Active and agent is idle, inject the goal objective as a continuation prompt per BC-022 per FR-032, T096 (missing)
- [ ] T144 Create `crates/joey-cli/src/omo_render.rs` for CLI inline agent activity summaries: print one-line summaries as events arrive (e.g. `[explore] spawned → running (model: glm-5)`, `[explore] done (4.2s)`) using ANSI colors matching TUI agent colors; route orchestration AgentEvents to omo_render in repl.rs per FR-040, T073-T074 (missing)
- [x] T145 Update `draw_status()` in `crates/joey-tui/src/widgets.rs` to show the active agent display_name + color indicator from `app.agent_roster[app.active_agent_index]` per FR-017, T036 (done: ◆ display_name shown after the mode badge)
- [ ] T146 Move transcript focus to Up-arrow key in `crates/joey-tui/src/app.rs handle_key()`: when focus=Input and Up pressed (on single-line input), set focus=Transcript (replaces lost Tab-focus behavior) per T031 (missing)
- [ ] T147 Update help overlay text in `crates/joey-tui/src/widgets.rs draw_help_overlay()` to document Tab=agent switch, Shift+Tab=reverse cycle, Up=transcript focus per T133 (missing)
- [ ] T148 Wire OMO concurrency configuration into `ManagerConfig::from_config()` in `crates/joey-orchestration/src/manager.rs`: add `omo.background_task.defaultConcurrency` (default 5), `providerConcurrency`, and `modelConcurrency` config keys; create per-key semaphores for provider/model-level overrides per FR-031, T091 (missing)
- [ ] T149 Update prompt append injection in `crates/joey-orchestration/src/subagent.rs`: when a category delegation has a `prompt_append`, prepend it to the subagent's system prompt alongside any loaded skills per FR-012, T060 (missing)
- [ ] T150 Add CLI agent switching via `/agent` command or numbered menu in `crates/joey-cli/src/repl.rs`: print numbered agent list and accept numeric selection per BC-018, BC-019 per FR-019, T034 (missing)
- [ ] T151 Write contract and integration tests: Tab picker smoke test (T039), agent cycling unit test (T040), category delegation contract test (T063), subagent_entries event tests (T075-T076), panel rendering at multiple sizes (T077), glm-5 family match integration test (T082), user override bypass test (T083), `/start-work` error/resume tests (T117), `/goal` continuation tests (T118), regression tests for Tab-free keybindings (T128) and CLI flags (T129) per SC-008, T039-T040, T063, T075-T077, T082-T083, T117-T118, T128-T129 (missing)
- [ ] T152 Write performance checks: time AgentRegistry::build() (<50ms), Tab picker render (<16ms), model fallback resolution (<5ms); verify narrow-terminal degradation at 70×20, 80×12, 50×10 renders without panic per SC-003, T131-T132 (missing)
