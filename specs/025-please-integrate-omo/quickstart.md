# Quickstart: OMO-HyperCode Orchestration Integration

**Date**: 2026-09-03 | **Spec**: [spec.md](spec.md)

Runnable validation scenarios proving the feature end-to-end. Prerequisites: workspace builds (`cargo build --workspace`); at least one OMO-chain provider configured (e.g. a GPT-5.6-family model for variant checks).

## Setup

```bash
cargo build --workspace
cargo test -p joey-omo
cargo test -p joey-orchestration
cargo test -p joey-cli
```

## Validation scenarios

### V1 — Default persona is delegation-first (FR-001, FR-002, SC-002)

Enable orchestration in a session with the OMO registry populated. Inspect the orchestrator's governing instructions (e.g. via a prompt-dump or the persona unit tests).
**Expected**: instructions carry the atlas-inherited conductor identity, mandate delegation-first with parallel fan-out, embed the hard rules (no direct writes; single final gate), and contain zero hands-on implementation directives.

### V2 — Full roster callable (FR-003, FR-010, SC-001)

From an enabled orchestrator session, delegate one trivial goal to each of the 11 roster names.
**Expected**: every delegation is accepted and runs under that agent's identity and resolved model; an unknown name returns an error listing valid names (see [contracts/delegation-roster.md](contracts/delegation-roster.md)).

### V3 — Role defaults from OMO chains (FR-005, FR-006, SC-005)

Start with fresh config (no role model overrides) and providers available. Resolve each role's model.
**Expected** (default, specialists ON): explorer←explore, implementor←hephaestus, orchestrator←atlas (direct 1:1 mapping; an unresolved agent falls back to parent/role defaults with a warning, never a hard failure). Setting `hypercode.omo_specialists.enabled=false` restores the legacy chains (explorer←explore-or-librarian, implementor←momus, orchestrator←sisyphus/hephaestus/metis, first resolvable) (see [contracts/role-defaults.md](contracts/role-defaults.md)).

### V4 — Persona switch swaps only the persona (FR-007, SC-003)

Mid-session, switch across all four primary agents using the existing agent switch.
**Expected**: each switch replaces the persona text; orchestration toolset, role behavior, and final-gate rule remain active; no restart; conversation context preserved.

### V5 — GPT-5.6 variant selection (FR-008, FR-009, SC-004)

Configure the orchestrator model to a GPT-5.6-family model; repeat for a non-GPT model.
**Expected**: GPT-5.6 selects `gpt_5_6` variants for the default persona and the switchable primaries; non-GPT families select each persona's family/default variant; no errors on personas lacking a variant (see [contracts/orchestrator-persona.md](contracts/orchestrator-persona.md)).

### V6 — Spec-kit synergy (FR-011, SC-007)

Drive one small spec-kit feature end-to-end under orchestration.
**Expected**: read-only researchers/reviewers dispatched during specify/clarify/plan; independent tasks implemented in parallel during implement; exactly one final acceptance verification run.

### V7 — Backward compatibility (FR-012, SC-006)

Disable orchestration (and/or run with an empty OMO registry).
**Expected**: HyperCode and OMO behavior identical to pre-integration; full workspace test suite green.

## Expected full-gate outcome

`cargo test --workspace` passes, including all pre-existing orchestration and OMO regression suites (SC-006).
