# Research: OMO-HyperCode Orchestration Integration

**Date**: 2026-09-03 | **Status**: Complete — all design points resolved

Findings verified against the codebase during specify/clarify/plan exploration. Each decision records rationale and rejected alternatives.

## D1 — Persona application architecture

**Decision**: Single authoritative overlay slot. `orchestrator_overlay()` becomes persona-aware: `orchestrator_persona_overlay(agent: Option<&str>, model: &str) -> String`. With integration active (orchestration enabled + OMO registry populated): `agent = None` → delegation-first persona variant; `agent = Some(name)` → that agent's `dispatch_system_prompt(name, model)` persona. Both embed the orchestration hard-rules core (no direct writes; single final gate; full roster briefing). `reapply_orchestrator_overlay` is extended to carry the current agent name and resolved model; `engine_switch_agent` already knows both.

**Rationale**: One slot means a persona switch literally swaps the orchestrator's governing instructions (spec FR-001/FR-007, clarification Q3) with no stacking ambiguity; `switch_model` clearing the overlay and reapply restoring it is existing, tested behavior.

**Alternatives considered**: (a) Persona in `agent_identity` slot stacked with an invariants-only overlay — rejected: ships today and produces two competing personas on the prompt; (b) verbatim atlas prompt reuse — rejected in clarification Q1 (imports OMO tool tables not applicable to orchestration).

## D2 — Persona home

**Decision**: New module `crates/joey-omo/src/agents/prompts/conductor.rs`, exported through the prompts `mod.rs` dispatch surface, NOT registered in `AgentRegistry` (no tab, no model chain, not callable as an agent). joey-cli consumes it via the existing prompt-dispatch function.

**Rationale**: Per-model variant dispatch, prompt-fidelity tests, and all sibling personas already live in joey-omo's prompts module (clarification Q2); registration would alter the pinned OMO roster/tab order and violate backward compatibility.

**Alternatives considered**: (a) Persona text in joey-cli next to `ORCHESTRATOR_PROMPT` — rejected: duplicates variant-selection logic, isolates the persona from the prompt test suite; (b) registered OMO agent — rejected: changes the user-facing roster.

## D3 — GPT-5.6 variant detection

**Decision**: Follow the established two-level convention: `ModelFamily::detect` prefix match, then substring version checks on the lowercased model id accepting both `5.6` and `5-6` (precedent: hephaestus `gpt_5_6()`, junior's Gpt arm). New `gpt_5_6()` variants: conductor (persona), sisyphus, atlas, prometheus. Non-GPT families fall back to each persona's existing/default variant. Delegation-only agents fall back to nearest variant (clarification Q4).

**Rationale**: Byte-consistent with how the codebase already discriminates GPT point releases; no new detection machinery.

**Alternatives considered**: Exact model-id matching — rejected: brittle against provider aliasing; family-only matching without point-release checks — rejected: measurably worse fit for 5.6-specific calibration.

## D4 — Full roster exposure via delegation

**Decision**: Widen `call_omo_agent`'s schema-only `subagent_type` enum from {explore, librarian, oracle} to all 11 registered agent names, and enrich the runtime unknown-agent error to list the valid names. The runtime resolver stays authoritative (schema is advisory and not closed; args forward unfiltered today, so no behavioral break).

**Rationale**: FR-003/FR-010 and User Story 2 (including its "error lists valid names" scenario); additive enum widening is backward compatible.

**Alternatives considered**: Drop the enum entirely and rely on runtime errors — rejected: loses schema-level guidance for models; register a second roster tool — rejected: duplicate surface.

## D5 — Role model defaults from OMO chains

**Decision**: When a role's configured model is empty, derive the default from the mapped OMO chains — explorer: explore → librarian; implementor: momus; orchestrator: sisyphus → hephaestus → metis — resolved against available providers in order. If no chain member resolves, keep today's behavior (inherit parent/role default) and emit a user-visible warning. Config keys (`hypercode.<role>.<provider>.*`) are unchanged; overrides keep winning.

**Rationale**: FR-005/FR-006 and clarification Q5 (no new configuration keys; automatic activation); the joey-orchestration `HyperRoleSettings` mirror documents that the config keys are the contract — both sides derive identically.

**Alternatives considered**: (a) New config keys for the mapping — rejected by clarification Q5; (b) hardcode model names — rejected: ignores provider availability and existing fallback-chain machinery.

## D6 — Activation and degradation

**Decision**: Integration is active iff orchestration mode is enabled AND the OMO agent registry has ≥1 resolved agent. Empty registry → existing HyperCode behavior (plain `ORCHESTRATOR_PROMPT`, parent-model roles) with the empty-bench notice already specified as an edge case.

**Rationale**: Clarification Q5 (automatic, no toggle); reuses `orchestrator_active()` plus a registry-populated check; degradation path already user-visible.

**Alternatives considered**: Opt-in/out config keys — rejected (Q5); tie activation to a single agent's availability — rejected: fragile.

## D7 — Spec-kit lifecycle alignment

**Decision**: Doctrine in the persona, detection delegated to the model: the delegation-first persona (and the hard-rules core) embeds explicit spec-kit lifecycle dispatch patterns — read-only researchers/reviewers during specify/clarify/plan, parallel implementors during implement, exactly one final acceptance verification — plus a concrete procedure for determining the active step from `.specify/feature.json` and artifact presence (spec.md/plan.md/tasks.md). No dynamic prompt injection machinery in v1.

**Rationale**: FR-011/SC-007; keeps the build-once-per-session prompt discipline (constitution performance constraints) — the orchestrator reads artifacts it already can read.

**Alternatives considered**: Dynamic step detection injecting into the system prompt per turn — rejected: breaks prompt-prefix cache warmth and the single-build discipline; a separate spec-kit persona — rejected: fragments the persona surface.

## Open items

None — every NEEDS CLARIFICATION and design unknown is resolved above; implementation sequencing belongs to tasks.md.
