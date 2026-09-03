# Contract: Role Model Defaults

**Date**: 2026-09-03 | **Consumers**: HyperCode role resolution (CLI + orchestration mirror) | **Keys**: `hypercode.<explorer|implementor>.<provider>.*` (unchanged); the orchestrator chain applies at session model resolution when no model is pinned or configured — no hypercode key

## Default derivation (when role model is unset)

Mode is selected by `hypercode.omo_specialists.enabled` (bool, default **true**).

### Specialists ON (default)

Direct 1:1 mapping to a single OMO agent — strict, no chain fallback.
An unresolved agent warns (FR-006 style) and inherits the parent/role
default.

| Role | OMO agent | Fallback on unresolved agent |
|---|---|---|
| explorer | explore | inherit parent/role default + user-visible warning |
| implementor | hephaestus | inherit parent/role default + user-visible warning |
| orchestrator | atlas | inherit parent/role default + user-visible warning |

### Specialists OFF (`hypercode.omo_specialists.enabled: false`)

Legacy feature-025 chains, unchanged:

| Role | Ordered OMO chain | Fallback on unresolvable chain |
|---|---|---|
| explorer | explore → librarian | inherit parent/role default + user-visible warning |
| implementor | momus | inherit parent/role default + user-visible warning |
| orchestrator | sisyphus → hephaestus → metis | inherit parent/role default + user-visible warning |

Chains resolve against available providers in order; the first resolvable member's model becomes the role default (FR-005).

## Precedence

1. Explicit per-role user configuration (existing keys) — always wins.
2. Derived OMO-chain default (this contract) — applies when the configured model is empty.
3. Inherit parent/role default — applies when the chain cannot resolve, accompanied by a warning (FR-006).

## Invariants

- Mode switch (supersedes the earlier "no new configuration keys" invariant): `hypercode.omo_specialists.enabled` (bool, default **true**) is THE mode switch — ON maps each tier directly 1:1 to its OMO agent with a strict no-chain-fallback rule (an unresolved agent warns FR-006-style and inherits the parent/role default); OFF preserves the legacy feature-025 chains.
- Mapping is model-level only: role guidance, toolsets, and turn/token limits keep their existing definitions and defaults.
- The mapping is read-only derivation; persisted configuration is never rewritten by it.

## Compatibility

With the integration inactive (orchestration disabled or empty registry) or with explicit models configured, resolution is identical to today (FR-012).
