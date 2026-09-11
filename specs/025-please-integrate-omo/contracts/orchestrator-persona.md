# Contract: Orchestrator Persona Selection

**Date**: 2026-09-03 | **Consumers**: joey-cli engine/hypercode layer | **Provider**: joey-omo persona library

## Selection function

Input: selected agent name (optional; none = default persona), resolved model id.
Output: the orchestrator's governing instruction text for the session/turn context.

| Selected agent | Resolved model family | Persona text |
|---|---|---|
| none | GPT-5.6 (ids containing `5.6`/`5-6` under `gpt-` prefix) | delegation-first persona, `gpt_5_6` variant |
| none | other GPT | delegation-first persona, generic GPT variant |
| none | any other family | delegation-first persona, default variant |
| any registered agent | per that agent's variant dispatch | agent's identity prompt for the resolved model (FR-007) |

## Invariants (embedded in every form)

1. Orchestrator performs no direct file writes, patches, or deletions.
2. Orchestrator runs no builds/tests except the single final verification gate.
3. Full roster briefing: all OMO agents plus HyperCode roles are valid delegation targets (FR-010).
4. Delegation-first doctrine: plan → brief → parallel fan-out → monitor → synthesize.
5. Spec-kit lifecycle dispatch patterns per FR-011 (read-only research/review for specify/clarify/plan; parallel implementation for implement; one final acceptance run).

## Variants required at delivery

- Delegation-first persona: `default`, generic GPT, `gpt_5_6` (FR-009).
- Switchable primaries with `gpt_5_6`: sisyphus, atlas, prometheus; hephaestus already provides one (FR-009).
- Delegation-only agents: nearest-variant fallback, no error (FR-009).

## Activation

Active iff orchestration mode enabled AND OMO registry has ≥1 resolved agent (research D6). Otherwise the existing fixed orchestrator prompt and role behavior apply unchanged (FR-012).

## Compatibility

Persona selection adds no configuration keys and changes no on-disk formats; with integration inactive the surface is byte-identical to today (FR-012).

## Addendum: Roles-Only Delegation (post-FR-010 revision)

The orchestrator persona's delegation surface is restricted to the two
HyperCode roles — role:"explorer" and role:"implementor" — exclusively.
Named-agent delegation (subagent_type), category routing, and any other
model-routed specialist target are no longer advertised to, or accepted
from, the orchestrator. The delegate_task tool enforces the same
restriction at the parameter layer for orchestrator sessions. This
addendum supersedes FR-010's full-roster briefing requirement for the
orchestrator persona.
