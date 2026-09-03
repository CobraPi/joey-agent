# Data Model: OMO-HyperCode Orchestration Integration

**Date**: 2026-09-03 | **Spec**: [spec.md](spec.md)

No new persisted state is introduced; all entities below are runtime constructs derived from existing configuration and registries. Validation rules trace to spec functional requirements (FR-xxx).

## Entities

### Orchestrator Persona Overlay

The single authoritative governing-instruction text applied to the orchestrator while integration is active.

- **Fields**: selected agent (optional name, default none), resolved model id, persona text (model-family-selected variant), embedded hard-rules core (invariant text).
- **Validation**: persona MUST be the delegation-first variant when agent is none (FR-001); MUST be the named agent's variant when set (FR-007); hard-rules core MUST be present in every persona form (FR-002); GPT-5.6-family models MUST select a GPT-5.6 variant where one exists (FR-008, FR-009).
- **Lifecycle**: session start (default persona) → user agent switch (persona swap, no restart) → model switch (variant reselection for new family, persona retained) → orchestration disabled (overlay removed).

### Role-to-Agent Model Mapping

Default model derivation for HyperCode roles from OMO agent chains.

- **Fields**: role (explorer | implementor | orchestrator), ordered agent chain (explorer: explore→librarian; implementor: momus; orchestrator: sisyphus→hephaestus→metis), resolved model (first chain member resolving against available providers).
- **Validation**: explicit user-configured model for the role always wins (FR-005); unresolvable chain → inherit parent/role default + user-visible warning, never failure (FR-006).
- **Relationships**: consumed by role configuration resolution; chains reference OMO Agent Registry Entries by name.

### OMO Agent Registry Entry (extended usage)

Existing registry entry, now also a delegation target and default-model source.

- **Fields (existing)**: name, display name, mode, identity prompt + model-optimized variants, model fallback chain, tool permissions.
- **New validation**: every registered agent — primary and delegation-only — MUST be name-addressable from delegation tooling (FR-003, FR-010); role enrichment and skill loading MUST be accepted on such delegations (FR-004).

### Delegation Target

A name-addressed dispatch of work to an agent.

- **Fields**: target agent name, goal, context, optional role enrichment (explorer/implementor), optional skills list.
- **Validation**: unknown names produce an error listing valid agent names (User Story 2); category routing remains mutually exclusive with named routing (spec edge case).

### Spec-Kit Step Context

The active spec-kit lifecycle step influencing dispatch patterns.

- **Fields**: feature directory (from `.specify/feature.json`), detected step (specify | clarify | plan | implement | complete) inferred from artifact presence (spec.md/plan.md/tasks.md).
- **Validation**: orchestrator guidance MUST map step → dispatch pattern: research/review read-only for specify/clarify/plan; parallel implementation for implement; exactly one final acceptance run (FR-011).
- **Note**: detection is procedural doctrine in the persona (research D7); not a persisted structure.

## State transitions

Persona overlay is the only stateful entity (see its lifecycle above). All other entities are derived per resolution and stateless.

## Traceability

| Entity | Spec source |
|---|---|
| Orchestrator Persona Overlay | FR-001, FR-002, FR-007, FR-008, FR-009, SC-002, SC-003, SC-004 |
| Role-to-Agent Model Mapping | FR-005, FR-006, SC-005 |
| OMO Agent Registry Entry | FR-003, FR-004, FR-010, SC-001 |
| Delegation Target | FR-003, FR-004, User Story 2 |
| Spec-Kit Step Context | FR-011, SC-007 |
