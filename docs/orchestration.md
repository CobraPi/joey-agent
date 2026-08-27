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

### Async delegation — `background=true` (feature 020)

`delegate_task` accepts two additive parameters (default behavior stays
blocking and unchanged):

- `background` (bool, default false): return a work handle immediately
  instead of blocking. One handle line per accepted task, in order —
  `[BACKGROUND] id=<child_id> goal=<goal> started` — within 2s regardless
  of child duration. In batch mode a top-level `background=true` applies
  to every task; any per-task `background: true` in the `tasks` array
  also switches the batch to background dispatch.
- `budgets` (object, optional): per-child caps
  `{max_turns?, max_tokens?, max_wall_clock_secs?}` — every present value
  must be `> 0`; zero/negative values are rejected at call time with
  `budgets.<field> must be > 0` and nothing dispatches. Omitted fields
  keep existing defaults (`delegation.default_max_turns`; otherwise
  unbounded). A top-level `budgets` applies to every task in a batch
  (per-task override is out of scope). On the blocking path `max_turns`
  rides the existing turn-limit machinery; `max_tokens` and
  `max_wall_clock_secs` are enforced by a parent-side watcher on the
  background path — a breach stops the child (reason
  `budget_exceeded`) after at most one more action, and the completion
  notice reports `outcome=budget_exceeded`.

Queueing (FR-013): background work runs under the SAME concurrency
limits as blocking delegation — permits are acquired inside each child
from the child semaphore pool; excess tasks queue, none are rejected,
and the handle does not imply a permit is held.

Completion notices: every finished background child (success, failure,
or stop) yields exactly one distilled notice on the orchestrator's
pending-completions queue (cap 64, drop-oldest; failures are never
silently dropped):

    [SUBAGENT COMPLETE|FAILED|STOPPED] id=<id> goal=<goal> outcome=<...> tokens=<n> duration=<secs>s
    <summary — capped ~2000 chars (~500 tokens)>

`outcome` is `success`, `failure`, or the snake_case stop reason
(`orchestrator_requested`, `operator_requested`, `budget_exceeded`,
`session_end`). Raw transcripts are never pushed into orchestrator
context. Mid-turn, notices deliver at the next turn boundary; when the
orchestrator is idle, the TUI/engine wakes it autonomously. Deviation:
the line REPL cannot be woken mid-read (reedline owns stdin
synchronously) — notices arrive at its next interaction.

Lifecycle: stopping one child never affects siblings; records stay
listed for the session lifetime (in-memory only — no SQLite/on-disk
changes) and are discarded at session end, when running children are
wound down within `delegation.wind_down_timeout_secs` (stop reason
`session_end`). `AgentEvent::SubagentStopped { id, goal, reason,
summary_preview }` is emitted on every non-natural stop.

### `subagent_control` tool (toolset: delegation)

Action-based control/inspection of spawned children (works for
blocking-batch and background children alike). Parameters:

| Parameter | Type | Required for | Notes |
|---|---|---|---|
| `action` | enum: `steer`, `stop`, `list`, `status`, `log`, `wait` | always | unknown actions are rejected with the implemented list |
| `id` | integer | `steer`, `stop`, `status`, `log` | child id from the delegate_task report or handle line (bare numbers or numeric strings accepted) |
| `message` | string | `steer` | non-empty; delivered before the child's next action, at its next action boundary |
| `last` | integer | — (`log`) | default 10, must be positive |
| `ids` | array of integers | `wait` | non-empty, order-preserving dedup |
| `timeout_secs` | integer | — (`wait`) | default 60, must be positive |

Actions and results:

- `list` — one line per child (running + finished this session, oldest
  first): `id=<id> goal=<truncated> state=<state> elapsed=<n>s tokens=<n>`
  where state is `running`, `completed`, `failed`, or
  `stopped:<reason>`. Empty overview: "no delegation children this
  session — start one with delegate_task background=true".
- `status` — single record in detail: `[status] id=… goal=…` header with
  state/elapsed/tokens, plus per-state detail (completed → iterations,
  model, summary; failed → error; stopped → stop reason) and cumulative
  token usage (FR-012).
- `log` — `[log] child <id> goal=… state=… — last N of M recorded
  events`, then numbered activity lines. Bounded ring of 256 lines per
  child — never the full transcript.
- `wait` — blocks until every id is terminal or `timeout_secs` expires:
  `[wait] all N waited-on children finished:` with per-child result
  lines, or `[wait] timed out after Ns — partial statuses (still-running
  children included):`. Holds no semaphore permits.
- `steer` — `steer queued for child <id>: delivered at next action
  boundary`.
- `stop` — `stop requested for child <id> (reason:
  orchestrator-requested)`; the child winds down at its next checkpoint
  and its partial result arrives via the completion notice.

Error semantics: unknown ids → "No subagent with id <id> is running or
has finished in this session"; terminal children → "Subagent <id>
already finished" (steer while a stop is pending adds the reason). All
actions return tool-level errors, never panics. Read-only actions and
steer/stop acquire no provider permits, so control stays fast under full
child saturation (SC-007 — see `delegation.parent_reserved_permits`).

TUI operator controls: with a subagent pane focused, `x` stops that
child (reason `operator_requested`), `s` opens the steer-text overlay;
both target only the focused child.

Examples:

```text
delegate_task
  {"goal": "audit error paths", "background": true,
   "budgets": {"max_turns": 8, "max_tokens": 60000, "max_wall_clock_secs": 600}}
→ [BACKGROUND] id=7 goal=audit error paths started

subagent_control {"action": "list"}
subagent_control {"action": "status", "id": 7}
subagent_control {"action": "log", "id": 7, "last": 5}
subagent_control {"action": "wait", "ids": [7, 8], "timeout_secs": 120}
subagent_control {"action": "steer", "id": 7, "message": "skip benchmarks, tests only"}
subagent_control {"action": "stop", "id": 8}
```

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
| `delegation.parent_reserved_permits` | 1 | orchestrator's guaranteed minimum share of `max_concurrent_requests` provider permits; children draw from a second pool of `max(1, N − reserve)` so the parent never starves under child saturation; 0 disables (child pool == parent pool, pre-feature behavior) |
| `delegation.wind_down_timeout_secs` | 10 | bounded wait when stopping running children at session end (line REPL `end_session` + TUI exit; stop reason `session_end`) |
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
