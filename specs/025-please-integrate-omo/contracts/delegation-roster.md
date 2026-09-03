# Contract: Delegation Roster Surface

**Date**: 2026-09-03 | **Consumers**: orchestrator and any delegating agent | **Surface**: delegation tooling (`delegate_task` named routing, `call_omo_agent`)

## Named delegation targets (full roster)

Primary agents: sisyphus, hephaestus, prometheus, atlas.
Delegation-only agents: oracle, librarian, explore, multimodal-looker, metis, momus, sisyphus-junior.

All 11 names are valid `subagent_type` values on both surfaces (FR-003, FR-010). Named routing remains mutually exclusive with category routing (existing behavior, preserved).

## `call_omo_agent` schema after widening

| Property | Type | Required | Values |
|---|---|---|---|
| goal | string | yes | task objective |
| context | string | no | self-contained execution brief |
| subagent_type | string | yes | any of the 11 roster names above |

The schema enum is advisory guidance (not a closed schema); the runtime resolver remains authoritative.

## Errors

| Condition | Error text contract |
|---|---|
| unknown agent name | error MUST list the valid agent names (User Story 2) |
| both category and agent name supplied | existing mutual-exclusivity error, unchanged |
| missing goal | existing single/batch goal error, unchanged |

## Role enrichment and skills

Delegations to any roster name accept role enrichment (explorer/implementor) and skill loading; explicit request values always win over role gap-fills (existing behavior, FR-004).

## Compatibility

Widening is additive: previously valid requests behave identically; no argument is removed or renamed; previously invalid names remain invalid but now produce the enriched error (FR-012).
