# Contract: System-Prompt Surface Changes

**Kind**: model-facing instruction text (stable tier of `build_system_prompt`, crates/joey-agent-core/src/prompt.rs)

## Additions

- New stable-tier section `GOAL_DIRECTED_GUIDANCE`, inserted immediately after `TASK_COMPLETION_GUIDANCE` (prompt.rs:809-811 position), included when `agent.goal_directed_guidance` is true (default) and tools are loaded — same condition shape as sibling guidance.
- New guidance wording is pinned by a matches-contract test (guidance.rs:329-342 precedent) and a section-order pin in the golden test (prompt.rs:1114-1130).

## Removals (predecessor-brand de-scope)

- `AGENT_HELP_GUIDANCE`: the "You run on Joey Agent (based on Hermes Agent by Nous Research)" attribution and the `https://hermes-agent.nousresearch.com/docs` URL are removed; remaining capability text stays.
- `DEFAULT_SOUL_MD` (joey-core default_soul.rs): persona text de-branded; `identity_matches_seeded_soul` pin (guidance.rs:344-347) updated in lockstep.
- TUI banner (joey-tui render.rs:1968): "· based on Hermes Agent by Nous Research" line removed.
- Explicitly retained (out of contract): `hermes-0day` IOC literal, `HERMES` env-var support, real model names in provider registries, `~/.hermes` home compat, legacy SOUL.md detection literals, upstream doc-comment citations, `UPSTREAM_ATTRIBUTION` (license).

## Invariants

- Prompt remains built once per session; identity (SOUL.md if present) still opens the prompt; tier ordering unchanged (stable → context → volatile); date/Model/Provider footer unchanged.
- Byte-identical rollback: with `agent.goal_directed_guidance=false` and a user-provided SOUL.md unchanged, the only prompt diff vs pre-feature is the two removed attribution strings (the feature's sanctioned divergence).
- Token budget: assembled prompt estimated-token count post-change <= pre-change (SC-007), verified deterministically.

## Verification hooks

- Strengthened no-brand test: zero 'hermes' (case-insensitive) across all model-visible guidance constants — no allowlist (previously two allowlisted attribution strings, guidance.rs:300-325).
- On/off parity + token-neutrality tests per [config-key.md](config-key.md) and plan.md R4.
