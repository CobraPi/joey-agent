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

### Parallel dispatch

(a) Multiple `delegate_task` calls emitted in one assistant message are
dispatched concurrently by the agent turn loop (a joey-native
enhancement; upstream Hermes dispatches tool calls sequentially). There
is deliberately no per-call timeout on this concurrent dispatch —
subagents run long by design.

(b) The child-slot semaphore pool is GLOBAL to the `SubagentManager`
and shared across all dispatch paths — blocking singles, batches, and
background waves all draw admission slots from ONE pool sized by
`delegation.max_concurrent_children`. Concurrent `delegate_task` calls
therefore cannot oversubscribe the documented child cap: every child
still spawns immediately, but at most `max_concurrent_children` may
enter their turn loop at any moment, process-wide per manager.

(c) `task_graph` `action=update` accepts a batched `transitions` array
and is safe under concurrent callers: mutations are Mutex-serialized
and a fresh graph snapshot is re-published via
`AgentEvent::TaskGraphPublished` after each batch of transitions, so
disjoint concurrent updates both land and observers see a consistent
graph after each.

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
    <summary — capped ~2000 chars (~1000 tokens)>

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
| `delegation.subagent_recovery_attempts` | 1 | bounded self-recovery for a child whose turn dies with a fatal provider error: the child's poisoned history is cleared and the turn re-runs from the initial prompt on the same provider/model; N = extra attempts after the initial run (0 disables — pre-feature behavior); each retry surfaces an `AgentEvent::RetryAttempt`; usage/iterations accumulate across attempts |
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

## 3. HyperCode Agent Teams (feature 022, `hypercode.team.*`)

Execution layer for team mode — file-backed shared task list + per-member
mailboxes hosted in `joey-orchestration/src/team.rs` (the joey-omo team
mode above stays untouched, in-memory and unused by this path). OFF by
default (`hypercode.team.enabled: false`); disabled sessions behave
byte-identically to plain subagent delegation.

### Starting a team

`delegate_task` accepts two optional parameters: `team` (team name) and
`name` (member mailbox identity). The first `team` reference lazily creates
the team — that child is the LEAD: an Orchestrator-role child with the
team-lead directive (decompose the objective → `team_tasks add` with
dependencies → spawn teammates with explorer/implementor role profiles →
synthesize) and the `delegation` + `team` toolsets. The lead's model:
an explicit `hypercode.team.lead_model` always wins; otherwise it
defaults to the orchestrator tier's mapping — `atlas` under specialists
ON (`hypercode.omo_specialists.enabled`, default), the legacy chain
head otherwise. The lead directive additionally carries a SPECIALISTS
paragraph permitting teammates to be spawned via `subagent_type`; a
teammate spawned that way uses the agent name as its role label.
Later references spawn
TEAMMATES: Leaf children keeping their role toolset plus `team` — they never
receive `delegate_task` (no nested teams, no background subagents from
mates). At most one team is active per session. Spawning errors
`team mode is disabled` unless enabled.

### Shared task list + mailboxes

Toolset `team`: `team_status`, `team_message` (direct member-to-member
delivery, drop-oldest at `hypercode.team.message_limit`), `team_tasks`
(add/list/claim/complete/release; a claim requires Pending status and all
dependencies Done — exactly one winner under concurrency). State persists
synchronously under `~/.joey/teams/<team>/` (config.json, tasks.json,
inboxes/<member>.json); an in-process registry is the claiming authority.

### Lifecycle + visibility

A stopped or failed team child releases its claimed Running tasks back to
Pending; completion notices identify `team: <team> member: <member>`;
`subagent_control stop` on a teammate likewise frees its tasks. Session end
winds every active team down and removes config.json + inboxes/ while
RETAINING tasks.json for resumption; startup purges team dirs older than
`hypercode.team.cleanup_days` (default 7).

### HyperCode routing

The orchestrator overlay documents when to use teams (independent,
parallelizable work) vs subagents (sequential, same-file, interdependent).
`/hypercode run` routes to team mode when team mode is enabled and the
planner decomposition yields ≥2 workstreams (no explicit workstreams); the
lead runs on `hypercode.team.lead_model` (empty = inherit the
orchestrator's effective model). Each run records its decision in
`HypercodeReport.mode_decisions` as `mode=<subagent|team> task=<summary>
rationale=<text>`.

## 4. OMO Integration (feature 025)

Ties the OMO agent roster into HyperCode orchestrator mode. Everything
here is additive: with the integration inactive, behavior is
byte-identical to pre-feature. Design trail:
`specs/025-please-integrate-omo/`.

### Persona-aware orchestrator overlay

When orchestration is enabled AND the OMO registry has ≥1 resolved agent,
the orchestrator's governing instructions (`extra_instructions`) become
the delegation-first **Conductor persona**
(`crates/joey-omo/src/agents/prompts/conductor.rs` — exported
unregistered, so it has no tab/registry entry), selected by model
family: GPT-5.6 → `gpt_5_6`, other GPT → `gpt`, else `default`
(`conductor::for_model`). Switching OMO agents mid-session swaps only
the persona — the hard-rules core (no direct writes; single final gate)
and the full-roster briefing are appended under any named persona;
`/model` re-selects the variant without losing the persona. Inactive
integration or an empty registry → the fixed `ORCHESTRATOR_PROMPT`,
byte-identical to pre-feature. Entry point:
`hypercode::orchestrator_persona_overlay[_for_profile]`, wired at
session start / `SetOrchestratorMode` / agent- and model-switch reapply
in `engine.rs` and `repl.rs`.

### Full-roster delegation

All 11 registered OMO agents (sisyphus, hephaestus, prometheus, atlas,
oracle, librarian, explore, multimodal-looker, metis, momus,
sisyphus-junior) are valid `subagent_type` targets on `delegate_task`
and valid values on the `call_omo_agent` enum — the schema enum is
advisory; the runtime resolver is authoritative. Unknown names error
with the valid-name list. `load_skills` works on named routing too: the
skill overlay (`prompt_append`) is synthesized from `load_skills`
entries exactly as on the category path. `category` and `subagent_type`
remain mutually exclusive (BC-011). In batch mode (`tasks[]`), each
task may carry its own `subagent_type` (any of the 11 agents): the
resolved model + identity prompt are applied per task, and the resolved
model wins over both the per-task and batch-level `model`; per-task
`role` composes with it — the role still gap-fills toolsets/turns and
appends its directive. The orchestrator prompt/roster advertises
`subagent_type` dispatch of any OMO specialist.

### Role model defaults from OMO agents

HyperCode role models derive from OMO agents — explicit overrides
always win; derivation is read-time and keyed on
`hypercode.omo_specialists.enabled` (bool, default **true**):

- Specialists ON (default): direct 1:1 mapping, strict — no chain
  fallback. explorer ← explore; implementor ← hephaestus; orchestrator
  session model ← atlas, applied ONLY when the model is neither pinned
  (`--model`, `/model`) nor configured (`model.default`). An unresolved
  agent inherits the existing default with a user-visible warning —
  never a failure.
- Specialists OFF (`hypercode.omo_specialists.enabled: false`): legacy
  feature-025 chains — explorer ← explore→librarian; implementor ←
  momus; orchestrator session model ← sisyphus→hephaestus→metis (first
  resolvable), same pinned/configured guard.

Both modes derive identically on both sides: `joey-cli` role resolution
(`hypercode.rs`) and the mirrored gap-fill in `joey-orchestration`
(`HyperRoleSettings` in `delegation_tool.rs`). An unresolvable mapping
inherits the existing default with a user-visible warning (FR-006,
agent-notice channel) — never a failure.

### Workflow inheritance: orchestrator toolset

In orchestrator mode (`/hypercode orchestrator on`) the main agent is a
delegation-first conductor. Its toolset used to be `delegation`,
`terminal`, `file-read`, `web`; it now also inherits the main agent's
workflow surfaces:

- `todo` — the session TODO list tool: the orchestrator decomposes its
  plan into a trackable checklist and maintains it as work progresses.
- `skills` — skills_list / skill_view / skill_manage: the
  `<available_skills>` index is injected into the orchestrator's system
  prompt again, so it can load matching skills (`skill_view`) BEFORE
  planning and pass `load_skills` in delegation requests.
- `task-graph` — the new `task_graph` tool (below): the graph planner.

Every orchestrator prompt — the fixed `ORCHESTRATOR_PROMPT` and every
persona-overlay variant — carries a `## Workflow inheritance` section
instructing exactly that: load matching skills before planning and pass
`load_skills` on delegation; build and maintain the session todo list
from the plan; publish and maintain the task graph and re-plan instead
of drifting. (With the OMO integration inactive the prompt stays
byte-identical to the const plus this section.)

### `task_graph` tool (toolset: task-graph)

New tool in `joey-orchestration`, registered globally but exposed only
through the `task-graph` toolset, so normal sessions never see it —
only the orchestrator does. Actions:

- `plan` — replace the current graph with a validated strict
  `joey-taskgraph/1` document (the same validator the execution-graph
  pipeline uses).
- `update` — apply TaskStatus transitions only: an array of
  `{id, status}` entries, enforcing the legal transition edges.
  Unknown task ids and illegal edges are rejected ("unknown task ..."
  / "illegal transition ...").
- `status` — render the current graph.

Every successful `plan`/`update` emits the additive
`AgentEvent::TaskGraphPublished { graph }` through the SubagentManager
event tap. The TUI feeds that event into the Tasks tab and the
`⚑{done}/{total}` header badge, so live-published orchestrator graphs
render exactly like `/hypercode` execution graphs.

### Strict document schema

The `plan` action's document is validated strictly; violations come
back as `schema_violation` errors. Shape:

- Document: `{"format":"joey-taskgraph/1","tasks":[...]}` —
  `baseline_revision` optional.
- Every task REQUIRES: `id` (lowercase `[a-z0-9-]`), `objective`,
  `dependencies`, `read_set`, `write_set` (relative paths),
  `artifact_ids` (array of integers), `role`
  (`"explorer"|"implementor"|"orchestrator"`), `model_tier`
  (`"economical"|"frontier"`), `risk` (`"low"|"medium"|"high"`),
  `acceptance` (non-empty array of `{criterion, kind}`), and
  `verification`
  (`{steps:[{name, command, parse, timeout_sec, required}], risk_triggered_review}` —
  required key, may be `{"steps":[],"risk_triggered_review":false}`;
  risk `"high"` needs a `required:true` step or
  `risk_triggered_review:true`).
- Optional: `isolation` (auto-injected per `write_set`), `status` /
  `attempts` (defaulted).
- Common rejections: missing `artifact_ids`/`verification` keys,
  string `artifact_ids`, unknown enum variants (e.g. `"light"`,
  `"conductor"`), absolute paths, empty `acceptance`,
  dependency-unrelated tasks sharing a write path.
