# Contract: Subagent Cascade (FR-021)

**Spec**: [spec.md](../spec.md) FR-021 | **Clarification**: Q5 (Inherit + Share, Option A) | **Plan**: [plan.md](../plan.md)

When a coding task is delegated to a subagent via `joey-orchestration`'s
`DelegateTask`, the subagent inherits the parent's NeuroCode configuration
and uses the shared structural index. The parent's complexity-tier decision
cascades down.

## Cascade mechanism

The cascade flows through **existing** orchestration plumbing — no NeuroCode
logic is threaded into `joey-orchestration` internals (Constitution VI):

1. **Config inheritance** (already exists): `register_orchestration_*` already
   receives `parent_config_tree: joey_core::Config`. The NeuroCode config
   keys (`neurocode.*`) ride along in this tree to the subagent's
   `AgentConfig`. No new parameter is added to the orchestration registration
   functions.

2. **Shared index** (by project-root identity): the subagent's
   `CodingRequest.project_root` points to the same project as the parent's,
   so `NeuroCodeEngine` opens the same `graph.db`. The subagent does NOT
   re-run ingestion (FR-021) — it reads the already-built index. The index
   is a per-project file, not per-agent, so sharing is automatic.

3. **Tier cascade** (via the allocator path): `register_orchestration_with_allocator`
   already threads the `ModelAllocator` into subagent dispatch. When NeuroCode
   is ON and 011 is ON, the parent's tier classification feeds 011's allocator,
   and the subagent's `ModuleId::Subagent` allocation inherits the tier
   constraint (tier-routing-composition.md Mode 1). When 011 is OFF, the
   subagent's `TierModelResolver` reads the same `neurocode.tier.*` config as
   the parent (Mode 2).

## Non-duplication guarantee (FR-021)

- The subagent MUST NOT re-index the project (no `neurocode_index` call on
  delegation). It reads the shared `graph.db`.
- The subagent MUST NOT build a private index. There is exactly one index per
  project, shared by the parent and all its subagents.
- The subagent's `neurocode_query` / `neurocode_status` / `neurocode_ingest`
  tools operate on the same shared index as the parent.

## What the subagent inherits

| Aspect | Inherited from parent | Notes |
|---|---|---|
| `neurocode.enabled` | Yes (via `parent_config_tree`) | Subagent respects the same enable/disable state. |
| `neurocode.tier.*` | Yes (via config) | Same tier models. |
| Structural index (`graph.db`) | Yes (same project root) | Shared, not copied. |
| Complexity tier for the delegated task | Yes (cascades via allocator) | Parent's tier feeds the subagent's model resolution. |
| Learned patterns / anti-patterns | Yes (same `graph.db`) | The subagent sees and contributes to the same learned memory. |
| Domain knowledge | Yes (same `graph.db`) | Shared domain sources. |
| Pega version detection | Yes (same project) | Same detected version. |

## Edge case: subagent targets a different project

If the delegated subagent's goal targets a *different* project root than the
parent (unusual but possible), the subagent's engine detects the un-indexed
project and operates in cold mode (FR-016) for that project, informing the
developer. It does NOT silently use the parent's project index for the wrong
project.
