# joey-orchestration & joey-omo — Subagent Delegation and Multi-Agent Orchestration

`joey-orchestration` ports Hermes' `delegate_task` plus Crush coordinator
patterns; `joey-omo` is a 1-to-1 port of oh-my-openagent built on top of it.

---

## 1. joey-orchestration — subagent delegation engine

Single and parallel-batch subagent dispatch in isolated execution contexts
(own history, toolset, budget), shared concurrency limiting, structured
lifecycle events.

### `delegate_task` tool (toolset: delegation)

Args accept a single task or a `tasks` array; each TaskSpec can carry its
own model, toolsets, and turn budget. Traces are ephemeral by default
(`persist=true` keeps them); the parent receives only a concise summary per
child. Model resolution chain: per-TaskSpec model > request model >
`delegation.default_model` > parent model. `model: "auto"` consults the
llm-selector's ModelAllocator.

### `call_omo_agent` tool

Research-only delegation wrapper (explore / librarian / oracle subagent
types) for read-only consultation.

### SubagentManager config (defaults shown)

| Key | Default | Meaning |
|---|---|---|
| `delegation.max_concurrent_children` | 3 | parallel children per batch |
| `delegation.max_concurrent_requests` | 5 | semaphore across parent + children |
| `delegation.max_spawn_depth` | 1 | flat, leaf-only (children can't spawn children) |
| `delegation.default_max_turns` | 50 | per-child turn budget |
| `delegation.default_persist` | false | traces ephemeral by default |
| `delegation.default_model` | — | fallback child model |
| `omo.background_task.defaultConcurrency` | 5 | OMO background tasks |
| `omo.background_task.providerConcurrency` / `modelConcurrency` | — | per-name limit tables |

### `CategoryResolver` trait

Lets the CLI bridge OMO categories into delegation without a circular
dependency (implemented in `joey-cli`'s `omo_resolver.rs` via
`joey_omo::resolve_category`).

---

## 2. joey-omo — Oh My OpenAgent orchestration

### 11 built-in agents

AgentRegistry with model fallback chains + family-level fuzzy matching:
sisyphus, hephaestus, prometheus, atlas, oracle, librarian, explore,
multimodal-looker, metis, momus, sisyphus-junior. Tab switch order in the
UI: sisyphus → hephaestus → prometheus → atlas (the plain joey default
agent is prepended to the cycle for backward compatibility). Each agent has
its own system prompt, tool permissions (`mode.rs`), and model requirement;
`dispatch_system_prompt(agent, model)` resolves prompts on switching.

### 11 built-in delegation categories

Route to Sisyphus-Junior with the category's resolved model + prompt
append; custom categories loadable from config: visual-engineering,
ultrabrain, deep, artistry, quick, unspecified-low, unspecified-high,
writing, quick-rust, quick-zig, git. Each carries a fallback chain of
(model, effort, provider-list) entries — e.g. `quick` prefers small fast
models, `ultrabrain` prefers max-effort reasoning models.

### IntentGate (`intent_gate.rs`)

Keyword detection in user messages: `ultrawork`/`ulw` (→ "ULTRAWORK MODE
ENABLED!"), `hyperplan` (→ "HYPERPLAN MODE ENABLED!"), combo, and `team`
(→ "TEAM MODE ENABLED!"). Ultrawork injects a model-family-specific prompt
overlay.

### Orchestrator runtime (`orchestrator.rs`)

Category/subagent_type routing, `start-work` hook (boulder init/resume,
Atlas activation), Atlas plan execution loop (read → delegate → verify),
boulder-push continuation reminders for Junior, tool restriction
enforcement, and wisdom accumulation.

### Plan and state files (under `.omo/`)

- `goals.json` — GoalState (per-session objective, active/paused; `/goal` command parsing)
- `notepads/` — five append-only markdown wisdom files per plan: learnings,
  decisions, issues, verification, problems
- `boulder.json` — BoulderState tracking active plan-execution work
  (active/completed/abandoned); written ATOMICALLY (unique sibling temp
  file + fsync + rename) so concurrent Atlas sessions can never interleave
  or truncate it
- `plan_parser.rs` — parses plan artifacts into ParsedTasks for execution

Category routing (`route_delegation`) resolves the category's model by
walking the fallback chain against actually-available models (exact then
family-fuzzy match, same as `categories::resolve_category`) — not blindly
taking the first chain entry, so unavailable models fall through.

### Team mode (`team.rs`, OFF by default)

Parallel multi-agent coordination via shared mailbox + shared task list.
Config: `enabled`, `max_parallel_members` (4), `max_members` (8),
`message_limit` (10), `poll_interval_ms` (500), `tmux_visualization`
(optional tmux-based visualizer).

### CLI integration

The REPL builds an `AgentRegistry` from connected models + catalog, applies
custom categories, wires `OmoCategoryResolver` into delegate_task, detects
intent keywords per message, injects active-goal context, and Tab/number
switching between primary agents (`omo_render.rs`). The TUI adds an OMO
agent panel.
