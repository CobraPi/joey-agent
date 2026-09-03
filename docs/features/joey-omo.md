# joey-omo — oh-my-openagent: agents, goals, plans, teams

`joey-omo` is a 1-to-1 Rust port of oh-my-openagent's multi-agent
orchestration system, layered on the `joey-orchestration` delegation engine.
It defines the 11 built-in agents (4 primary Tab-selectable + 7 delegation
subagents), 11 delegate-task categories with model fallback chains,
family-level fuzzy model resolution, the IntentGate ultrawork/hyperplan/team
keyword gate, the plan→execute→worker pipeline (plan parser, boulder state,
wisdom notepads), goals/subgoals, team mode with optional tmux
visualization, and per-agent model-family system-prompt variants. The crate
is strictly additive over joey-agent: the existing default agent is prepended
to the Tab cycle, and the public API is deliberately narrow.

> See also: [../orchestration.md](../orchestration.md)

## Overview

Runtime pieces and their files:

- **`agents/`** — the 11 `OmoAgent` definitions, model requirements, and the
  prompt dispatch system (`agents/prompts/`, one module per agent).
- **`models.rs`** — `ModelFamily::detect`, `FallbackEntry`,
  `ModelRequirement`, `AvailableModelSet`, `resolve_model`.
- **`categories.rs`** — 11 built-in categories + custom-category loading and
  resolution.
- **`intent_gate.rs`** — ultrawork/hyperplan/team keyword detection.
- **`goal.rs` / `boulder.rs` / `plan_parser.rs` / `notepad.rs`** — the
  plan-execution state model under `.omo/`.
- **`orchestrator.rs`** — delegation routing, wisdom extraction,
  `start_work`, task delegation prompts, Prometheus write restriction.
- **`team.rs`** — team config, mailbox, task list, eligibility, tmux
  visualizer. (Feature-022 team *tools* live in joey-orchestration;
  `crates/joey-omo/src/team.rs` stays the OMO-side port.)
- **`mode.rs`** — `AgentMode`, `ToolPermissions`.

## Module map (31 files)

`src/lib.rs` + 10 top-level modules + `agents/` (`mod.rs` + `registry.rs`)
+ 14 prompt modules, 2 test files, 1 example, `Cargo.toml` (31 total):

| File | Role |
|---|---|
| `src/lib.rs` | Re-exports, `ultrawork_prompt`, `dispatch_system_prompt` |
| `src/agents/mod.rs` | `OmoAgent` + the 11 model-requirement functions |
| `src/agents/registry.rs` | `AgentRegistry` construction/resolution |
| `src/agents/prompts/mod.rs` | `dispatch_system_prompt`, `ultrawork_prompt`, `conductor_prompt` |
| `src/agents/prompts/{sisyphus,atlas,hephaestus,prometheus,oracle,librarian,explore,multimodal,metis,momus,junior,ultrawork,conductor}.rs` | Per-agent identity prompts with model-family variants |
| `src/models.rs` | Families, fallback chains, availability, resolution |
| `src/categories.rs` | Category configs + resolution + custom categories |
| `src/intent_gate.rs` | `detect_keyword`, `check_ultrawork_activation` |
| `src/goal.rs` | `GoalState`, `Subgoal`, command parsers |
| `src/plan_parser.rs` | `parse_plan`, `ParsedTask`/`ParsedPlan` |
| `src/boulder.rs` | `BoulderState`/`BoulderWork` persistence |
| `src/notepad.rs` | `NotepadStore`, `NotepadFile`, learning extraction |
| `src/orchestrator.rs` | Routing, wisdom, `start_work`, delegation prompts |
| `src/team.rs` | Team mode types, mailbox/task list, tmux visualizer |
| `src/mode.rs` | `AgentMode`, `ToolPermissions` |
| `tests/verify_prompts.rs` | 11 agents × 6 families prompt verification |
| `tests/slash_commands.rs` | `/start-work` + `/goal` backing logic (T117/T118) |
| `examples/test_omo_registry.rs` | Registry smoke example |

## Agent roster

All 11 agents (`OmoAgent` fields: `name`, `display_name`, `mode`, `color`,
`description`, `model_requirement`, `resolved_model`/`resolved_variant`,
`temperature`, `max_tokens`, `tool_permissions`; methods `system_prompt(model)`
→ `dispatch_system_prompt`, `is_available()` (model resolved, BC-002),
`effective_model()` → resolved or `"unavailable"`):

| Agent | Mode | Color | Role | Permissions / notes |
|---|---|---|---|---|
| `sisyphus` | Primary | `#6C5CE7` | The default OMO orchestrator | `allow_all`; temp 0.1; `requires_any_model=true` |
| `hephaestus` | Primary | `#D97706` | High-precision coding specialist | `allow_all`; **GPT-only** historically — now requires one of openai/github-copilot/opencode/vercel/**zai** (GLM 5.2 fallback); `max_tokens` 32000 |
| `prometheus` | Primary | `#8B5CF6` | Read-only planning consultant | Denies `terminal`, `delegate_task`; writes only `.omo/*.md` (`is_prometheus_write_allowed`) |
| `atlas` | Primary | `#10B981` | Master orchestrator/conductor | Denies `write_file`, `patch` (delegates all implementation); temp 0.1 |
| `oracle` | Subagent | `#3B82F6` | Architecture consultant | Denies `write_file`, `patch`, `delegate_task` (read-only) |
| `librarian` | Subagent | `#F59E0B` | Documentation/OSS search | Read-only (same denies as oracle) |
| `explore` | Subagent | `#06B6D4` | Fast codebase grep/search | Read-only; same chain as librarian |
| `multimodal-looker` | Subagent | `#EC4899` | Vision/screenshot analysis | `allow_all` |
| `metis` | Subagent | `#A855F7` | Gap analyzer | Denies `write_file`, `patch` |
| `momus` | Subagent | `#EF4444` | Plan reviewer/critic | Denies `write_file`, `patch` |
| `sisyphus-junior` | Subagent | `#20B2AA` | Focused task executor | Denies `task`, `delegate_task`; allows `call_omo_agent`, `read_file`, `write_file`, `patch`, `terminal`, `search_files`, `web_search`, `web_extract`, `todo`, `skills` (research via `call_omo_agent`); `max_tokens` 64000 |

`AgentRegistry`:

| Method | Behavior |
|---|---|
| `build(available, overrides)` | Constructs all 11 agents, resolves each model via fallback chains; unresolved agents marked skipped (`resolved_model=None`), never dropped (BC-001) |
| `build_with_categories(available, overrides, custom)` | Merges custom categories over/onto the builtins (same name replaces; FR-012/T154) |
| `all()` | All 11 agents including skipped |
| `available_primary()` | Only Primary agents with a resolved model (BC-002) |
| `tab_order()` | Canonical Tab order `["sisyphus","hephaestus","prometheus","atlas"]`, available only; the "Default" joey agent is prepended by the caller |
| `get(name)` | Lookup by canonical name |
| `categories()` | All categories |
| `available_models()` | The `AvailableModelSet` used to build |

`ModelOverrides` (agent name → model) honors `omo.agents.<name>.model`,
bypassing the chain (BC-009). `requires_provider` is checked before chain
resolution (BC-010).

## Categories

11 built-in `CategoryConfig`s (each: `name`, `description`,
`model_requirement` fallback chain, optional `temperature`, optional
`prompt_append`). Category delegation routes to **sisyphus-junior** with the
category's resolved model and prompt append:

| Category | Temp | Description |
|---|---|---|
| `visual-engineering` | 0.5 | Frontend/UI work, design, visual implementation |
| `ultrabrain` | 0.3 | Hard logic, strategic thinking, complex reasoning |
| `deep` | 0.4 | Autonomous research and execution, deep work |
| `artistry` | 0.7 | Creative and design work, aesthetics |
| `quick` | 0.2 | Fast, cheap tasks — minimal tokens, quick turnaround |
| `unspecified-low` | 0.3 | Low-effort fallback (no prompt_append) |
| `unspecified-high` | 0.4 | High-effort fallback (no prompt_append) |
| `writing` | 0.5 | Prose and documentation, content creation |
| `quick-rust` | 0.2 | Quick Rust-specific tasks (uses the `quick` chain) |
| `quick-zig` | 0.2 | Quick Zig-specific tasks (uses the `quick` chain) |
| `git` | 0.1 | Git operations and version control (uses the `quick` chain) |

`resolve_category(name, registry)` walks the category's chain against the
registry's available models; `validate_delegation(category, subagent_type)`
enforces XOR — both set → error (BC-011), neither → error (BC-012).
`route_delegation` (orchestrator.rs) implements the same routing:
`category` → Junior with the category model (default temp 0.5), else
`subagent_type` → the named agent; `DelegationRoute` carries `agent_name`,
`model`, `denied_tools`, `prompt_append`, `temperature`, `max_tokens`.
Custom categories load from `omo.categories` config entries
(`name`/`description`/`model`/`temperature`/`prompt_append`) as single-model
chains.

## Model resolution

- `ModelFamily::detect(model_id)`: prefix classification — `claude-*` →
  `Anthropic`, `gpt-*` → `Gpt`, `kimi-*` → `Kimi`, `glm-*` → `Glm`,
  `gemini-*` → `Gemini`, `minimax-*`/`MiniMax_*` → `Minimax`, else
  `Unknown`. Case-insensitive.
- `FallbackEntry { providers, model, variant }` — one chain candidate with an
  optional effort variant ("max"/"xhigh"/"high"/"medium"/"low").
- `ModelRequirement { fallback_chain, requires_any_model, requires_provider }`
  — the ordered chain; `requires_provider` gates the whole agent (BC-010).
- `AvailableModelSet`:
  - `from_connected(profile, active_model)` — canonical name + aliases +
    billing-plan aliases (e.g. `zai` → `zai-coding-plan`,
    `bailian-coding-plan`, `moonshotai-cn`, `opencode-go`; `openrouter` →
    `opencode`, `vercel`, `kimi-for-coding`, …) plus the active model,
    default aux model, and fallback models.
  - `from_connected_with_catalog(…)` — additionally seeds every chat-capable
    model id from a Copilot-compatible proxy endpoint's `/models` catalog.
  - `from_models(iter)` — explicit id list (used by tests).
  - Queries: `contains_exact`, `contains_family`, `first_in_family`,
    `has_provider`, `has_any_provider`.
- `resolve_model(requirement, available)`: walk entries in order; per entry
  try **exact** id match first, then **family fuzzy** match (first available
  model of the entry's family) — BC-006→BC-010. Returns
  `(model, variant)` or `None` (agent skipped, BC-008).

## Intent gating

`KeywordType`: `Ultrawork` (`ultrawork` or `ulw`), `Hyperplan` (`hyperplan`),
`HyperplanUltraworkCombo` (both in one message; combo wins over individual),
`Team` (`team`). `detect_keyword(message)` scans lowercased text with
**word-boundary** matching (`contains_word` keeps alphanumeric + `-`
characters, so "ultraworking" does not match). Each type's mandatory first
response (BC-024): "ULTRAWORK MODE ENABLED!", "HYPERPLAN MODE ENABLED!", or
"TEAM MODE ENABLED!".

`ultrawork_valid_for_agent(agent_name)` — valid on `""` (default agent),
`"default"`, `"sisyphus"`, `"hephaestus"`, `"atlas"`; **silently ignored on
`prometheus`** (read-only planner incompatible, FR-022/BC-025).
`check_ultrawork_activation(keyword, agent_name)` returns the first-response
message or `None` (ignored).

## Goals & subgoals

- `GoalState { session_id, objective, status, subgoals, set_at }` persisted
  at `.omo/goals.json`; `read` (None if missing), `write`, `clear` (remove
  file), `new(session_id, objective)` (status `Active`, RFC3339 timestamp).
- `GoalStatus`: `Active` (default) | `Paused`.
- `GoalAction` (parsed by `parse_goal_command`): `Set { objective }`
  (`/goal set <text>`), `Pause`, `Resume`, `Clear`, `Show` (also for empty
  or unknown subcommands).
- `Subgoal { number, text, done, added_at }` — extra success criteria via
  `/subgoal`; `SubgoalAction` (parsed by `parse_subgoal_command`): `Add`,
  `Remove(n)`, `SetDone { number, done }` (`done`/`undone`), `Clear`,
  `Show`.

## Plans & start-work

- `plan_parser`: `parse_plan(markdown)` recognizes `- [ ] N. <title>`
  (implementation), `- [ ] F<num>. <title>` (final verification),
  `- [x]`/`- [X]` (completed), and optional `> Depends on: N, M` dependency
  lines. `ParsedTask { number, title, is_final_verification, dependencies,
  completed }`; `ParsedPlan` helpers: `implementation_tasks()`,
  `final_verification_tasks()`, `ready_tasks(completed)` (not completed, not
  in the completed set, all dependencies completed — BC-032).
  `F_TASK_NUMBER_OFFSET = 1 << 30` places F-tasks in a numeric range
  implementation tasks can never reach, so `F1` and task `1` stay distinct in
  number-keyed collections.
- `prepare_plan_execution(plan_content)` → `(implementation, verification)`
  task vectors.
- `build_task_delegation_prompt(task, wisdom_context, plan_context)` — the
  Atlas→Junior prompt: task header, `<plan-context>`, accumulated
  `<accumulated-wisdom>`, `<constraints>` (MUST DO: complete fully, todo
  discipline, fix errors; MUST NOT DO: delegate, modify `.omo/plans/`,
  exceed scope), and a `<verification>` block.
- `start_work(omo_dir, session_id, explicit_plan_name)` → `StartWorkResult {
  is_resume, agent ("atlas"), plan_path, boulder, context_injection }`.
  Resume: an existing `.omo/boulder.json` with works selects this session's
  active work or the **most recent** active work (multiple active → most
  recent) and injects a resuming `<session-context>` with progress (checked
  vs total `- [` lines). Init: uses the explicit plan name (slugified) or the
  most recently modified `.omo/plans/*.md`; errors otherwise ("No plans
  found… Create one with Prometheus (@plan) first.").
- `boulder`: `BoulderState { works, version }` at `.omo/boulder.json`
  (atomic temp-file + fsync + rename writes); `BoulderWork { id ("work_*"),
  plan_path, plan_name, session_id, agent, worktree_path, status, started_at }`;
  `BoulderWorkStatus`: `Active` | `Completed` | `Abandoned`. Methods:
  `read`/`write`, `create_work(plan_path, plan_name, session_id)`,
  `complete_work(work_id)`, `select_active()` (Some iff exactly one Active).
  `boulder_push_reminder(incomplete_todos)` renders the "[SYSTEM REMINDER -
  TODO CONTINUATION]" block Junior receives until all todos are complete.

## Wisdom & notepad

- `extract_wisdom(subagent_response, task_description)` → `ExtractedWisdom`
  with five buckets — `learnings` (convention/pattern/standard/discovered),
  `decisions` (decided/chose/architecture/rationale), `issues`
  (issue/problem/gotcha/blocker/warning/error:), `verification` (test+pass or
  test+fail), `problems` (unresolved/technical debt/todo:/fixme). Always
  records at least one learning ("Task '<desc>': completed").
- `accumulate_wisdom(store, wisdom)` appends each non-empty bucket to its
  notepad file; `wisdom_context_block(store)` renders all five files as
  `<accumulated-wisdom>…</accumulated-wisdom>` (section labels: "Patterns &
  Conventions", "Decisions", "Issues & Gotchas", "Verification Results",
  "Unresolved Problems").
- `NotepadStore::new(omo_dir, plan_name)` roots at
  `.omo/notepads/{plan-name}/`; `NotepadFile` maps to `learnings.md`,
  `decisions.md`, `issues.md`, `verification.md`, `problems.md`. `append`
  (append-only, never rewritten — VR-005), `read_all` (concatenated with
  file-name headers), `read(file)`.
  `extract_and_append_learnings(store, summary)` is the lighter heuristic
  extractor for delegation summaries (conventions/issues/decisions markers).

## Teams

`TeamModeConfig` (all defaults per serde + `Default`): `enabled=false`
(FR-041), `max_parallel_members=4`, `max_members=8`, `message_limit=10`,
`poll_interval_ms=500`, `tmux_visualization=false`.

- `TeamMember { name, kind, prompt }`; `TeamMemberKind` (serde-tagged
  `type`): `Category { category }` or `SubagentType { subagent_type }`.
- `TeamSpec { name, members }`.
- `TeamMailbox`: `send(from, to, content)` (RFC3339 timestamp),
  `receive(member)` (destructive — drains the member's messages),
  `poll(member)` (non-destructive peek).
- `TeamTaskList`: `add(title) -> id ("task_<uuid>")`, `claim(task_id,
  member)` (atomic — only a `Pending` task can be claimed),
  `complete(task_id, success)` (→ `Done`/`Failed`), `list()`.
  `TeamTaskStatus`: `Pending`/`Running`/`Done`/`Failed`.
- `validate_team_eligibility(agent_name)` (FR-042/T120): **Eligible** —
  `sisyphus`, `atlas`, `sisyphus-junior`; **Conditional** — `hephaestus`;
  **Rejected** — `oracle`, `librarian`, `explore`, `multimodal-looker`,
  `metis`, `momus`, `prometheus` (and unknown names).
- `activate_team(config, spec)` → `Ok(Some(TmuxVisualizer))` when enabled +
  visualization on, `Ok(None)` when enabled but visualization off/tmux
  unavailable (or disabled — team infrastructure invisible), or
  `Err(TeamActivationError::IneligibleMember)` for a rejected member
  (category members count as `sisyphus-junior`).
- `TmuxVisualizer`: detached tmux session `joey-omo-team`
  (`DEFAULT_TMUX_SESSION`) with one tiled pane per member;
  `render_member(name, &MemberActivity)` updates a pane;
  `MemberActivity { name, status, current_task, completed, failed,
  last_message }` renders the per-member block. All tmux subprocess work runs
  on tokio's blocking pool (T009) and every method degrades to a no-op when
  tmux is missing — visualization is purely additive. `stop()`/`Drop` tears
  the session down via a detached thread.

The feature-022 team *tools* (`team_status`/`team_message`/`team_tasks`) and
the file-backed team registry live in
[joey-orchestration.md](joey-orchestration.md) § Teams.

## Modes & permissions

- `AgentMode`: `Primary` (Tab-selectable: sisyphus, hephaestus, prometheus,
  atlas) | `Subagent` (delegation-invoked). Helpers `is_primary`,
  `is_subagent`, `label` ("Primary"/"Sub").
- `ToolPermissions` — per-tool allow/deny with **deny precedence**:
  - `allow_all()` — no restrictions.
  - `new(allow, deny)` — explicit lists; `allow(tool)`/`deny(tool)` add.
  - `is_allowed(tool)`: denied → always false; empty allow list → all
    non-denied permitted; otherwise must be explicitly allowed.
  - `is_denied(tool)`; accessors `allowed()`/`denied()`.

## Prompt system

Each agent identity is captured as compile-time `&str` prompt constants with
model-family variants (runtime layers — tool tables, skills — are injected by
the harness, not baked in). Each prompt module exposes `for_model(model)`
which selects the Anthropic/GPT/Kimi/Glm/Gemini variant via
`ModelFamily::detect`. `dispatch_system_prompt(agent_name, model)` routes by
canonical name (`"multimodal"` and `"junior"` are accepted aliases); unknown
names fall back to the **sisyphus** default (safest orchestrator identity).
`ultrawork_prompt(model)` is the keyword-gated ultrawork overlay — a mode on
the active primary agent, not its own identity; every variant carries the
mandatory "ULTRAWORK MODE ENABLED!" announcement. `conductor_prompt(model)`
(feature 025) is the delegation-first Conductor persona — not a registered
agent, no registry entry/Tab/chain. Tab switching injects an agent's identity
via `dispatch_system_prompt` (BC-004).

## Testing

- `tests/verify_prompts.rs` — exercises `dispatch_system_prompt` across all
  11 agents × 6 model families (Anthropic/GPT/Kimi/Glm/Gemini/Minimax;
  prompts >500 chars), ultrawork variants carry the mandatory announcement,
  and GLM variants mention GLM.
- `tests/slash_commands.rs` — T117/T118: `/start-work` (non-existent plan
  errors with no state change; single active work auto-resumes; multiple
  active works resolve to the most recent) and `/goal` (set persists an
  Active goal; pause stops continuation; resume restarts it; clear removes).
- Inline `#[cfg(test)]` suites pin: exactly 11 agents and 11 categories,
  hephaestus provider gating, sisyphus `requires_any_model`, family
  detection, chain order + exact/fuzzy resolution, override precedence,
  eligibility, mailbox/task-list semantics, tmux no-op degradation, goal and
  subgoal parsers, plan parsing (F-offset, dependencies), routing errors,
  wisdom extraction, boulder atomic writes.

## See also

- [../orchestration.md](../orchestration.md) — orchestration subsystem doc
- [joey-orchestration.md](joey-orchestration.md) — the delegation engine this
  crate rides on (`delegate_task`, `call_omo_agent`, team tools)
- [joey-agent-core.md](joey-agent-core.md) — the agent turn loop
- [joey-tools.md](joey-tools.md) — tool registry and toolsets
- [joey-cli.md](joey-cli.md) — CLI/TUI wiring: Tab switching, slash commands
- [joey-providers.md](joey-providers.md) — provider profiles feeding
  `AvailableModelSet::from_connected`
