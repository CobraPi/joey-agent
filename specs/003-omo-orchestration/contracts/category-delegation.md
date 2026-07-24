# Contract: Category-Based Delegation

**Feature**: 003-omo-orchestration

## Delegation API

When a primary agent delegates work, it specifies either a **category** or a
**subagent_type** — never both.

### Category Delegation

```text
delegate(category="quick", load_skills=["frontend"], prompt="...")
  → spawns Sisyphus-Junior with:
    - model = resolve_model(category.model_requirement, available)
    - prompt_append = category.prompt_append
    - temperature = category.temperature
    - tool_permissions = Junior defaults (task blocked, call_omo_agent allowed)
    - skills prepended to prompt
```

### Subagent Delegation

```text
delegate(subagent_type="oracle", prompt="...")
  → spawns the named agent (Oracle, Explore, Librarian, etc.) with:
    - model = resolve_model(agent.model_requirement, available)
    - tool_permissions = agent defaults
```

## Mutual Exclusivity

- **BC-011**: A delegation call MUST NOT specify both `category` and
  `subagent_type`. Doing so is an error, rejected at dispatch time.
- **BC-012**: A delegation call MUST specify at least one of `category` or
  `subagent_type` (or a `task_id` for session continuation).

## Category Dispatch Table

| Category | Routes Through | Temperature | Description |
|----------|---------------|-------------|-------------|
| visual-engineering | Sisyphus-Junior | (chain) | Frontend/UI work, design |
| ultrabrain | Sisyphus-Junior | (chain) | Hard logic, strategic thinking |
| deep | Sisyphus-Junior | (chain) | Autonomous research and execution |
| artistry | Sisyphus-Junior | (chain) | Creative and design work |
| quick | Sisyphus-Junior | (chain) | Fast, cheap tasks |
| unspecified-low | Sisyphus-Junior | (chain) | Low-effort fallback |
| unspecified-high | Sisyphus-Junior | (chain) | High-effort fallback |
| writing | Sisyphus-Junior | (chain) | Prose and documentation |
| quick-rust | Sisyphus-Junior | (chain) | Quick Rust-specific tasks |
| quick-zig | Sisyphus-Junior | (chain) | Quick Zig-specific tasks |
| git | Sisyphus-Junior | (chain) | Git operations |

## Integration with joey-orchestration

Category delegation maps onto the existing `DelegationRequest`:
- `goal` ← the prompt
- `model` ← resolved from category chain
- `toolsets` ← Junior's allowed toolsets
- `role` ← `Leaf` (Junior cannot delegate)

The existing `SubagentManager::dispatch_batch` handles parallel execution,
concurrency limiting, and result collection. OMO categories are a routing
layer on top, selecting model + prompt before handing to SubagentManager.

## Skills Integration

Skills are prepended to the subagent's prompt. The `load_skills` parameter
accepts a list of skill names. Available skills are resolved from the
project/user/builtin skill directories (existing joey-agent skill system).

Priority: `project > user > builtin`.
