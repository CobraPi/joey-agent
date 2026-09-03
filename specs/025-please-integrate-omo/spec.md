# Feature Specification: OMO-HyperCode Orchestration Integration

**Feature Branch**: `025-please-integrate-omo`

**Created**: 2026-09-03

**Status**: Draft

**Input**: User description: "please integrate omo functionality into /hypercode - I want the deafault system prompt to inherit aspects of the atlas omo agent system prompt in the sense that I want it to focus on pure delagation and parallelism - I want the /hypercode orchestrator to have access to every omo agent as a callable tool - please map the explorer model, implementor model, and orchestrator models accordingly to their matching omo agent (ex. orchestrator=metis/hephaststus/sysyphus, explore=explore,librarian implementor=momus) - I want the omo function to be so that if /hypercode is enabled, switching to a pre-selected omo agent just swaps the system prompt of the hypercode orchestrator with the selected omo agent - please be sure to use the model-optimized prompts where possible (specifically optimize for GPT 5.6) - every hypercode orchestrator configuration should have access to the full range of omo and hypercode agents - please optimize everything to work optimially with the speckit workflow."

## Clarifications

### Session 2026-09-03

- Q: Default persona prompt strategy — author a new persona, reuse the atlas prompt verbatim, or append a directive to the current instructions? → A: Author a new delegation-first orchestrator persona adopting atlas's conductor identity and delegation doctrine with the existing orchestration hard rules embedded; it replaces the current default orchestrator instructions when OMO integration is active.
- Q: Where does the delegation-first persona live — HyperCode module, OMO registry agent, or OMO prompts module? → A: Maintained in the OMO prompts module with its model-optimized variants, but not registered as an agent; the HyperCode side selects the variant by resolved model and applies it as the orchestrator overlay.
- Q: What is the entry point for switching orchestrator personas — existing agent switch, new dedicated command, or both? → A: The existing OMO agent-switch surface serves as the persona switch when orchestration is enabled; no separate persona-switch command is introduced.
- Q: How wide is the GPT-5.6 prompt-variant coverage — minimum, switchable primaries, or all agents? → A: GPT-5.6-optimized variants are shipped for the delegation-first orchestrator persona and all four switchable primary agents (sisyphus, hephaestus, prometheus, atlas); delegation-only agents use the nearest available variant with graceful fallback.
- Q: How is the integration activated — automatic with orchestration, opt-in key, or automatic with opt-out? → A: Automatic — active whenever orchestration mode is enabled and the OMO agent registry is populated; no new configuration key is introduced; it degrades to plain HyperCode behavior when no OMO agents resolve.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Delegation-First Orchestrator Persona (Priority: P1)

When HyperCode orchestration is enabled, the default orchestrator persona becomes a conductor in the style of the atlas OMO agent: it plans, briefs, and fans out work to subagents in parallel, and never implements anything itself.

**Why this priority**: This is the core behavioral change the entire feature serves; every other story extends or refines this persona.

**Independent Test**: Enable orchestration with default configuration and inspect the orchestrator's governing instructions, then submit one small build request. Passes if the instructions mandate delegation-first and parallel dispatch, and the request is executed entirely through dispatched subagents.

**Acceptance Scenarios**:

1. **Given** HyperCode orchestration enabled with default configuration, **When** an orchestrator session starts, **Then** its governing instructions carry a conductor identity inherited from the atlas OMO agent and mandate pure delegation and parallel fan-out.
2. **Given** an active orchestrator session, **When** a build-type request is processed, **Then** every hands-on action is performed by dispatched subagents while the orchestrator itself only plans, briefs, monitors, and runs the single final verification gate.

---

### User Story 2 - Full OMO Roster Callable by the Orchestrator (Priority: P2)

The orchestrator can dispatch work to every registered OMO agent by name — primary agents and delegation-only subagents alike — as first-class delegation targets.

**Why this priority**: Unlocks the full specialist bench for orchestration; today only a restricted subset is reachable this way.

**Independent Test**: From an enabled orchestrator session, delegate one trivial task to each registered OMO agent by name. Passes if every delegation is accepted and routed to that agent's identity and model.

**Acceptance Scenarios**:

1. **Given** HyperCode orchestration enabled, **When** the orchestrator dispatches a task to any named registered OMO agent, **Then** the work runs under that agent's identity and resolved model.
2. **Given** a delegation request naming an unregistered agent, **When** the request is submitted, **Then** a clear error is returned listing the valid agent names.

---

### User Story 3 - Role-to-Agent Model Mapping (Priority: P3)

The explorer, implementor, and orchestrator role configurations draw their default models from matching OMO agents: explorer from explore then librarian; implementor from momus; orchestrator from sisyphus, then hephaestus, then metis. Explicit user overrides always win.

**Why this priority**: Gives each role a sensible, OMO-consistent model default out of the box without configuration.

**Independent Test**: Start with fresh configuration containing no role overrides and matching providers available; verify each role's resolved model comes from its mapped OMO chain, and that setting an override replaces it.

**Acceptance Scenarios**:

1. **Given** default role configuration and available matching providers, **When** roles resolve their models, **Then** explorer, implementor, and orchestrator resolve from their mapped OMO chains in the specified order.
2. **Given** a user-configured override for a role's model, **When** the role resolves, **Then** the override is used and the OMO default does not apply.
3. **Given** no model in a mapped chain is available, **When** the role resolves, **Then** it falls back gracefully (role default or parent model) with a warning instead of failing.

---

### User Story 4 - Agent Switch Swaps Only the Persona (Priority: P4)

With HyperCode enabled, switching to a pre-selected OMO agent replaces only the orchestrator's instruction overlay with that agent's identity; the orchestration machinery stays fully active.

**Why this priority**: Lets users pick orchestrator personas freely while keeping delegation pipelines, role configs, and safety rails intact.

**Independent Test**: Switch across all primary OMO agents mid-session; verify the persona changes each time while the delegation toolset, role behavior, and final-gate rule remain, with no session restart.

**Acceptance Scenarios**:

1. **Given** HyperCode enabled and an active session, **When** the user switches to a different primary OMO agent, **Then** the orchestrator's instructions become that agent's identity while orchestration tooling, role configurations, and the final-gate rule remain active.
2. **Given** the switch happens mid-session, **When** subsequent turns run, **Then** the session continues without restart and without losing conversation context.

---

### User Story 5 - Model-Optimized Prompt Selection (Priority: P5)

When the resolved model belongs to the GPT-5.6 family, a GPT-5.6-optimized prompt variant is used wherever one exists, including a new variant for the delegation-first orchestrator persona; personas without one fall back to their default variant.

**Why this priority**: Prompt quality measurably varies by model family; the user explicitly targets GPT 5.6.

**Independent Test**: Configure the orchestrator model to a GPT-5.6-family model and verify the active persona prompt is the GPT-5.6 variant where one exists.

**Acceptance Scenarios**:

1. **Given** a persona with a GPT-5.6-optimized prompt variant, **When** the resolved model is in the GPT-5.6 family, **Then** that variant is selected automatically.
2. **Given** a persona with no GPT-5.6 variant, **When** the resolved model is in the GPT-5.6 family, **Then** the persona's default variant is used with no error.

---

### User Story 6 - Spec-Kit Workflow Synergy (Priority: P6)

Orchestrator guidance aligns delegation patterns with the active spec-kit lifecycle step: read-only research and review dispatch during specify, clarify, and plan; parallel task implementation during implement; and exactly one final acceptance verification run.

**Why this priority**: The repo's features are delivered through spec-kit; the integration must reinforce, not fight, that workflow.

**Independent Test**: Drive one small spec-kit feature end-to-end under orchestration and observe stage-appropriate dispatch behavior at each step.

**Acceptance Scenarios**:

1. **Given** a spec-kit feature in the implement step with orchestration enabled, **When** tasks execute, **Then** independent tasks are dispatched to implementors in parallel and verification follows the project's single final acceptance gate.
2. **Given** the specify, clarify, or plan step with orchestration enabled, **When** the step runs, **Then** the orchestrator dispatches read-only researchers and reviewers rather than implementing anything itself.

---

### Edge Cases

- What happens when the user switches OMO agents while orchestration mode is disabled? The switch behaves as a plain OMO agent switch; no orchestration overlay is applied.
- What happens when a mapped model chain cannot resolve against available providers? The role falls back gracefully (role default or parent model) with a warning; orchestration remains usable.
- What happens if a delegation request supplies both a category and an explicit agent name? The existing mutual-exclusivity error is preserved.
- What happens when the OMO agent registry is empty (no providers match)? The orchestrator still functions with plain HyperCode roles and reports the empty bench rather than failing.
- What happens when two OMO agents resolve to the same underlying model? The prompt swap still changes the persona; model sharing is expected and harmless.
- What happens when the delegation-first (conductor) persona tries to edit files directly? Orchestration hard rules and toolset restrictions still block it; personas never relax safety rails.
- What happens when the model changes after a persona switch? The persona's prompt variant is re-selected for the new model family without losing the persona.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: When orchestration mode is enabled, the default orchestrator instructions MUST be a newly-authored delegation-first persona that adopts the conductor identity and delegation doctrine of the atlas OMO agent, embeds the orchestration hard rules, and replaces the prior default orchestrator instructions; it MUST mandate coordinate-don't-execute behavior and parallel fan-out of subagents.
- **FR-002**: The delegation-first persona MUST preserve the orchestration hard rules: no direct file writes, patches, or deletions by the orchestrator, and no build/test execution except the single final verification gate.
- **FR-003**: Every registered OMO agent — primary and delegation-only — MUST be invocable by the orchestrator as a named delegation target without requiring category-based routing.
- **FR-004**: Delegation to any OMO agent MUST accept role enrichment (explorer/implementor guidance) and skill loading.
- **FR-005**: Role model defaults MUST derive from OMO agent chains — explorer: explore then librarian; implementor: momus; orchestrator: sisyphus then hephaestus then metis — and explicit user configuration MUST override these defaults.
- **FR-006**: When a mapped chain cannot resolve, the system MUST fall back to the role default or parent model with a user-visible warning, never a hard failure.
- **FR-007**: With orchestration enabled, switching via the existing OMO agent-switch surface MUST replace only the orchestrator's instruction overlay with the selected agent's persona; the orchestration toolset, role configurations, subagent management, and final-gate behavior MUST remain active, and no separate persona-switch command is introduced.
- **FR-008**: Persona prompt selection MUST be model-family-aware: a GPT-5.6-optimized variant MUST be chosen when the resolved model is in the GPT-5.6 family and a variant exists; otherwise the persona's default variant MUST be used.
- **FR-009**: GPT-5.6-optimized prompt variants MUST exist for the delegation-first orchestrator persona and for all four switchable primary agents (sisyphus, hephaestus, prometheus, atlas); delegation-only agents MUST fall back to the nearest available variant without error.
- **FR-010**: Every orchestrator configuration MUST expose the full combined roster — all OMO agents plus all HyperCode roles — regardless of which persona is active.
- **FR-011**: Orchestrator guidance MUST align delegation patterns with the active spec-kit lifecycle step: read-only research and review dispatch for specify, clarify, and plan; parallel task implementation for implement; and a single final acceptance verification run.
- **FR-012**: The integration MUST be backward compatible and automatic: it is active whenever orchestration mode is enabled and the OMO agent registry is populated, with no new configuration key introduced; when no OMO agents resolve, existing HyperCode behavior MUST be unchanged.
- **FR-013**: The delegation-first persona MUST be maintained alongside the OMO persona prompt library with its model-optimized variants, and MUST NOT be registered as an OMO agent (no tab entry, no registry entry, no independent model fallback chain); the orchestration layer selects the appropriate variant by resolved model family.

### Key Entities *(include if feature involves data)*

- **Orchestrator Persona Overlay**: The active governing instructions for the orchestrator; either the delegation-first default or a selected OMO agent identity; carries model-optimized variants. The delegation-first persona is maintained in the OMO persona prompt library (unregistered) and applied by the orchestration layer, which selects the variant by resolved model family.
- **Role Configuration**: Per-role model and behavior settings (explorer, implementor, orchestrator); defaults now sourced from OMO agent chains.
- **OMO Agent Registry Entry**: A registered agent's name, identity prompt with model-optimized variants, model fallback chain, and tool permissions.
- **Role-to-Agent Model Mapping**: The default derivation of role models from OMO chains (explorer→explore/librarian; implementor→momus; orchestrator→sisyphus/hephaestus/metis).
- **Delegation Target**: A name-addressable dispatch of work to an agent, supporting role enrichment and skill loading.
- **Spec-Kit Step Context**: The active lifecycle step influencing which delegation patterns the orchestrator uses.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: 100% of registered OMO agents are presented as valid delegation targets to every enabled orchestrator configuration.
- **SC-002**: The default orchestrator instructions contain explicit delegation-first and parallel-dispatch directives and zero hands-on implementation directives.
- **SC-003**: All four primary OMO agents are switchable as orchestrator personas mid-session, taking effect without a session restart (4 of 4 switchable).
- **SC-004**: Whenever the resolved model is GPT-5.6-family and the active persona has a GPT-5.6-optimized variant, that variant is in use — including the default orchestrator persona.
- **SC-005**: With zero user configuration, all three roles resolve their models from the mapped OMO chains whenever matching providers are available.
- **SC-006**: Every pre-existing orchestration and OMO regression check continues to pass unchanged after the integration.
- **SC-007**: A spec-kit feature driven end-to-end under orchestration completes with independent tasks implemented in parallel and exactly one final full acceptance verification run.

## Assumptions

- The role-to-agent mapping is model-level: the implementor role inherits momus's model chain only — its write capability and role guidance are unchanged, since momus itself is a read-only reviewer. Likewise, explorer targets keep read-only behavior through role guidance.
- A persona switch swaps the instruction overlay only; toolset restrictions, role configurations, subagent management, and the final verification gate are orchestration invariants.
- GPT-5.6-optimized variants ship for the delegation-first orchestrator persona and all four switchable primaries; delegation-only agents use the nearest available variant with graceful fallback (see FR-009).
- "Full range of agents" includes both primary (user-selectable) and delegation-only OMO agents.
- All existing configuration keys, on-disk formats, and public behavior remain backward compatible per the project constitution; any new configuration is additive.
- Model names and availability depend on the user's configured providers; chains degrade gracefully when providers are missing.
