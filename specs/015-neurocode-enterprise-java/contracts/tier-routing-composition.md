# Contract: Tier Routing Composition with spec 011 (ModelAllocator)

**Spec**: [spec.md](../spec.md) FR-018 | **Clarification**: Q2 (Compose, Option A) | **Research**: [research.md](../research.md) §5

NeuroCode does NOT implement a parallel model router. It classifies
complexity into a `ComplexityTier` and composes with spec 011's
`ModelAllocator` to resolve the actual model id.

## Three operating modes

### Mode 1: NeuroCode ON + spec 011 ON (the composition path)

NeuroCode's `ComplexityClassifier` produces a `ComplexityTier`. This tier is
passed as a **constraint hint** into 011's `ModelAllocator::resolve()`. 011
still owns the allocation map, learning loop, and diagnostics — NeuroCode
does not duplicate any of that machinery (FR-018).

The composition is **strictly additive** to the `ModelAllocator` trait. 011's
`resolve()` already takes `(module, turn_has_images, needs_tools,
token_budget_hint)`. NeuroCode's tier constraint is injected via the
turn-loop intercept: when NeuroCode classifies a coding task as `Frontier`,
the turn loop requests allocation for `ModuleId::MainTurn` with a preference
for the frontier-tier model. The concrete mechanism (a tier-hint field on
the allocation request, or a tier-scoped module id) is finalized in
implementation but MUST NOT change 011's trait signature in a breaking way
(Constitution VII).

### Mode 2: NeuroCode ON + spec 011 OFF

NeuroCode's `TierModelResolver` reads the configured model for the chosen
tier directly from `config.yaml`:

```yaml
neurocode:
  tier:
    economical:
      model: "qwen2.5-coder-7b"      # or any provider model id
    frontier:
      model: "claude-3.5-sonnet"
    ambiguous_default: "economical"  # which tier AmbiguousDefault resolves to
```

If the configured tier model is missing, NeuroCode falls back to the agent's
configured default model and records the fallback in the route reasoning.

### Mode 3: NeuroCode OFF

Byte-identical to today (FR-020, SC-008). The `NeuroCodeEngine` is `None`;
no classification, no tier resolution, no context assembly.

## Non-duplication guarantee (FR-018)

NeuroCode MUST NOT:
- Maintain its own allocation map (011's `AllocationMap` is the single source).
- Run its own learning/diagnoser loop (011's diagnoser is the single learner).
- Send a model id to the API that 011 hasn't resolved (when 011 is ON).

NeuroCode's only routing output is the `ComplexityTier` classification + the
config-based tier→model lookup (Mode 2 only).

## Subagent cascade (FR-021)

When a coding task is delegated to a subagent, the parent's `ComplexityTier`
decision cascades via the existing `joey-orchestration` dispatch path
(`parent_config_tree` carries the NeuroCode config; the allocator path
already threads through `register_orchestration_with_allocator`). The
subagent inherits the tier and the shared index — see
[subagent-cascade.md](./subagent-cascade.md).
