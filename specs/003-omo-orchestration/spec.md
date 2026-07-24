# Feature Specification: Oh My OpenAgent Orchestration

**Feature Branch**: `003-omo-orchestration`

**Created**: 2026-07-23

**Status**: Draft

**Input**: User description: "please implement all the features of oh-my-openagent (keep all agent names and orchestration routines identical) and add them to this rust project joey-agent. I want you to implement it such that I can switch between the different agent mode (default, sysyphus, prometheus, etc,) by pressing tab. Be extremely through and make this a 1-to-1 re-implementation. Please modify both the CLI and TUI to show all the agent activity in an intuitive format on the bottom right of the UI-I want you to convey as much information about the different agents/sub-agents in the most elegant way possible."

## Clarifications

### Session 2026-07-23

- Q: Should the existing joey-agent default agent also appear in the Tab picker alongside the OMO agents (user mentioned "default, sisyphus, prometheus")? → A: Yes — the existing default agent appears first; the Tab cycle is Default → Sisyphus → Hephaestus → Prometheus → Atlas (5 entries). The 11 OMO agents remain unchanged; the default is an additional 12th top-level agent entry preserving the existing joey-agent experience.
- Q: Should ultrawork mode activate on any agent or only when Sisyphus is active? → A: Any agent except Prometheus. Ultrawork is valid on Default, Sisyphus, Hephaestus, and Atlas. Prometheus is a read-only planner and ultrawork's mandatory-implementation semantics are incompatible with it, so ultrawork is ignored (not injected) when Prometheus is the active agent.
- Q: Should model fallback chains hardcode exact OMO model IDs (claude-opus-4-8, kimi-k3, etc.) or fuzzy-match to joey-agent's configured providers? → A: Preserve chain order and model families (1-to-1 structure fidelity), but resolve each entry via family-level fuzzy matching against joey-agent's actually-configured providers. E.g., "claude-opus-4-8" matches the Anthropic family — if joey-agent has any Anthropic provider with an Opus-class model available, that model is used. This preserves OMO's routing semantics (which family is tried first, variant, fallback order) without forcing users to replicate OMO's exact provider/model configuration. Exact model IDs are tried first (direct match); family-level aliasing is the fallback within each chain entry.
- Q: When subagents are running, should the activity panel fully replace the idle roster, or split to show both the active agent and live activity? → A: Split. The active primary agent is always pinned at the top of the panel (name, color, mode, resolved model). When idle, the full agent roster expands below it. When active, the roster condenses/collapses and live subagent activity fills the remaining space. This preserves context about which agent mode is active while maximizing subagent information density.
- Q: Should the ulw-plan workflow (Prometheus's adversarial planning protocol) be ported as a joey-agent skill file or inlined into the Prometheus prompt? → A: Port as a joey-agent skill file (e.g. under the skills system), preserving OMO's skill-based architecture 1-to-1. The Prometheus system prompt loads and references the skill rather than inlining its content. This is more maintainable (the skill can be iterated independently of the prompt builder) and matches the user's "1-to-1 re-implementation" directive most faithfully.

## User Scenarios & Testing

### User Story 1 - Agent Mode Switching via Tab (Priority: P1)

The user is in the TUI (or CLI REPL) and wants to switch the active
primary agent mode. They press **Tab** (or **Shift+Tab** to cycle
backwards). A mode picker appears — either an inline overlay in the TUI
or a numbered menu in the CLI — listing all available primary agents
in their canonical assembly order:

```
Default → Sisyphus → Hephaestus → Prometheus → Atlas
```

The existing joey-agent default agent (the Hermes-style orchestrator)
appears first as the 0th entry, preserving backward compatibility —
users who never switch agents see no behavioral change. The 4 OMO
primary agents follow. The default agent is an additional top-level
entry; the 11 OMO built-in agents are unchanged.

Selecting an agent immediately swaps the system prompt, model, tool
permissions, and behavioral instructions for the next turn. The TUI
status bar / CLI prompt indicator updates to show the active agent name
and color. The user can press Tab again to cycle to the next agent or
pick from the list.

This is a 1-to-1 re-implementation of oh-my-openagent's primary-agent
selection, with the existing joey-agent default agent prepended:
the four OMO primary agents (`sisyphus`, `hephaestus`,
`prometheus`, `atlas`) are selectable top-level session agents, plus
the existing default. Pressing Tab cycles through them, and the selected
agent's prompt, model fallback chain, color, and mode (`primary`) become
active for the session.

**Why this priority**: Tab-based agent switching is the single most
visible user-facing capability and the explicit trigger mechanism the
user requested. Without it, none of the orchestration agents are
reachable interactively. It is the front door to the entire feature.

**Independent Test**: The user launches the TUI, presses Tab, sees the
four primary agents listed, selects Prometheus, and the status bar shows
"Prometheus" with its color. They type a planning request and the agent
behaves as a read-only planner (interviews, does not implement). Pressing
Tab again cycles to Atlas, and the next turn uses Atlas's conductor
prompt.

**Acceptance Scenarios**:

1. **Given** the TUI is in Input mode with the default agent active,
   **When** the user presses Tab, **Then** a mode picker overlay appears
   listing all five primary agents in canonical order (Default,
   Sisyphus, Hephaestus, Prometheus, Atlas) with their colors,
   descriptions, and current model assignments.
2. **Given** the mode picker is visible, **When** the user selects
   Prometheus (via Enter or arrow keys), **Then** the active agent
   switches, the system prompt is rebuilt for Prometheus, and the status
   bar reflects the new agent name and color.
3. **Given** the active agent is Atlas, **When** the user presses Tab
   again, **Then** the picker reappears with Atlas highlighted and
   cycling continues (wraps back to Default after Atlas).
4. **Given** a turn is in progress (Busy mode), **When** the user
   presses Tab, **Then** the switch is deferred — the new agent takes
   effect on the next user turn, not mid-execution.
5. **Given** the CLI REPL (non-TUI), **When** the user presses Tab,
   **Then** a numbered list of agents is printed and the user types a
   number to select, achieving CLI/TUI parity (Constitution Principle II).

---

### User Story 2 - The 11 Built-In Agents (Priority: P1)

The system MUST provide all 11 built-in agents from oh-my-openagent,
with identical names, roles, colors, and behavioral semantics:

**Primary agents** (selectable via Tab, `mode: "primary"`):
- **Sisyphus** — "The Discipline Agent." Main orchestrator. Plans,
  delegates to specialists, drives tasks to completion with aggressive
  parallel execution. Named after the Greek myth — he rolls the boulder
  every day, never stops. Color: its configured color.
- **Hephaestus** — "The Legitimate Craftsman." Autonomous GPT-native
  deep worker. Goal-oriented execution without hand-holding. Explores
  thoroughly, researches patterns, executes end-to-end.
- **Prometheus** — "The Strategic Planner." Interview-based planner.
  READ-ONLY — can only create/modify markdown files within `.omo/`.
  Asks clarifying questions, identifies scope, builds a plan before code
  is touched.
- **Atlas** — "The Conductor." Executes Prometheus plans via task
  delegation. Distributes tasks to specialized subagents, accumulates
  learnings across tasks, verifies completion independently. Does not
  write code directly — delegates all implementation.

**Subagent agents** (invoked via delegation, `mode: "subagent"`):
- **Oracle** — Read-only high-IQ consultant for architecture decisions
  and complex debugging.
- **Librarian** — Documentation and OSS code search. Stays current on
  library APIs and best practices.
- **Explore** — Fast codebase grep. Uses speed-focused models for
  pattern discovery.
- **Multimodal-Looker** — Vision and screenshot analysis.
- **Metis** — Gap analyzer. Catches what Prometheus missed before plans
  are finalized.
- **Momus** — Ruthless reviewer. Validates plans against clarity,
  verification, and context criteria.
- **Sisyphus-Junior** — Focused task executor. The workhorse that
  actually writes code. Cannot delegate (blocked from the task tool).
  Disciplined: obsessive todo tracking, must pass diagnostics before
  completion.

Each agent has: a unique display name, a system prompt (potentially with
model-family-specific variants), a color, a mode (primary or subagent),
a model fallback chain, temperature/variant settings, and tool permission
restrictions.

**Why this priority**: The 11 agents ARE oh-my-openagent. Without all
11, this is not a 1-to-1 re-implementation. They are the actors that
make the orchestration system work.

**Independent Test**: A user can enumerate all available agents via a
CLI command or TUI overlay and see exactly 11 agents with the correct
names, descriptions, modes, and colors. Each primary agent, when
selected, produces a distinct system prompt and behavioral profile.

**Acceptance Scenarios**:

1. **Given** the system is initialized, **When** the user queries
   available agents, **Then** exactly 11 agents are listed: sisyphus,
   hephaestus, prometheus, atlas (primary) and oracle, librarian,
   explore, multimodal-looker, metis, momus, sisyphus-junior
   (subagent).
2. **Given** the user selects Sisyphus, **When** the system prompt is
   built, **Then** it contains the Sisyphus identity, IntentGate phase,
  delegation tables, todo discipline, and anti-patterns sections.
3. **Given** the user selects Prometheus, **When** the system prompt is
   built, **Then** it identifies as a read-only planner that never
   implements and writes only to `.omo/`.
4. **Given** Sisyphus-Junior is spawned as a subagent, **When** its tool
   registry is assembled, **Then** the `task`/delegate tool is blocked
   (deny) while `call_omo_agent` is allowed, enforcing its
   no-delegation constraint.
5. **Given** any agent, **When** its model is resolved, **Then** the
   fallback chain from the agent's model requirements is tried in order
   until an available model is found.

---

### User Story 3 - Category-Based Delegation System (Priority: P1)

When the primary agent delegates work, it does NOT pick a model name —
it picks a **category** that describes the task's intent. Categories
automatically route to the right model. This is the core innovation of
oh-my-openagent: semantic categories replace raw model names.

The system MUST support these delegate-task categories (1-to-1 with the
source):

| Category | Intent | Example Model Routing |
|---|---|---|
| `visual-engineering` | Frontend/UI work, design | Gemini-class models |
| `ultrabrain` | Hard logic, strategic thinking | Top-tier reasoning models |
| `deep` | Autonomous research and execution | Deep reasoning models |
| `artistry` | Creative and design work | Vision-capable models |
| `quick` | Fast, cheap tasks | Mini/flash models |
| `unspecified-low` | Low-effort fallback | Cheapest available |
| `unspecified-high` | High-effort fallback | Best available |
| `writing` | Prose and documentation | Prose-optimized models |

Additionally, specialized categories `quick-rust`, `quick-zig`, and
`git` are recognized. Regardless of category name, category dispatch
goes through Sisyphus-Junior.

Each category has a model fallback chain (tried in order until an
available model is found), a temperature, a description, and an optional
prompt append. Categories are user-configurable and extendable via
config.

**Why this priority**: The category system is what makes orchestration
"just work" — the agent describes intent, not implementation. Without
it, delegation requires manual model juggling, defeating the purpose.

**Independent Test**: Sisyphus delegates a frontend task with
`category="visual-engineering"`. The system spawns Sisyphus-Junior
configured with the visual-engineering model chain and skill set. The
subagent receives the category-optimized model and prompt append.

**Acceptance Scenarios**:

1. **Given** Sisyphus delegates with `category="quick"`, **When** the
   subagent is spawned, **Then** Sisyphus-Junior receives the quick
   category's model chain (fast/cheap models tried first) and its prompt
   append is included.
2. **Given** the configured model for a category is unavailable, **When**
   the fallback chain is resolved, **Then** the next model in the chain
   is tried until one is available.
3. **Given** a user defines a custom category in config, **When**
   Sisyphus delegates with that category name, **Then** the custom
   category's model and settings are used.
4. **Given** a category and a direct subagent_type are both specified in
   one delegation, **When** the system validates, **Then** it rejects
   the call — category and subagent_type are mutually exclusive.

---

### User Story 4 - IntentGate and Working Modes (Priority: P2)

Every user message passes through an **IntentGate** before the agent
acts. The IntentGate classifies the true intent (research,
implementation, investigation, evaluation, fix, open-ended) and routes
accordingly. This is embedded in the Sisyphus/Hephaestus system prompts
as Phase 0.

The system MUST support three working modes, identical to
oh-my-openagent:

1. **Normal mode** — The active primary agent processes the request with
   its standard prompt. Sisyphus plans and delegates; Hephaestus does
   deep autonomous work; Prometheus plans read-only; Atlas executes
   plans.

2. **Ultrawork mode** — Triggered when the user types `ultrawork` or
   `ulw`. The ultrawork instruction set is injected into the active
   agent's prompt. This is the "just do it" mode: maximum precision,
   mandatory plan-agent invocation, TDD enforcement, manual QA mandate,
   zero-tolerance for partial completion. The agent MUST say
   "ULTRAWORK MODE ENABLED!" as its first response. Ultrawork has
   model-family-specific prompt variants (default/Claude, GPT, Gemini,
   GLM, planner). Ultrawork activates on any primary agent except
   Prometheus — Prometheus is a read-only planner and cannot implement,
   so ultrawork is incompatible with it. On Default, Sisyphus,
   Hephaestus, and Atlas, ultrawork is valid and injects its instruction
   set.

3. **Prometheus interview mode** — When Prometheus is the active agent,
   it interviews the user like a real engineer: asks clarifying
   questions, identifies scope and ambiguities, builds a detailed plan.
   It can launch explore/librarian agents for context. After gathering
   requirements, it consults Metis (gap analysis) and optionally Momus +
   Oracle (high-accuracy dual review) before finalizing the plan. The
   plan is written to `.omo/plans/{name}.md`.

**Why this priority**: IntentGate and working modes are what make the
agents behave intelligently rather than blindly executing. Ultrawork is
oh-my-openagent's signature feature.

**Independent Test**: The user types `ulw implement a hello-world CLI`.
The agent responds "ULTRAWORK MODE ENABLED!", then fires explore agents,
invokes the plan flow, delegates implementation to Sisyphus-Junior, and
verifies the result — all autonomously.

**Acceptance Scenarios**:

1. **Given** Sisyphus is active and the user types a message containing
   `ultrawork` or `ulw`, **When** the message is processed, **Then** the
   ultrawork instruction set is injected into the system prompt and the
   agent announces "ULTRAWORK MODE ENABLED!".
2. **Given** Prometheus is active, **When** the user describes work,
   **Then** Prometheus asks clarifying questions before producing a plan
   and never implements code directly.
3. **Given** the user message is classified as research/exploration,
   **When** the IntentGate routes it, **Then** explore/librarian agents
   are fired in parallel (background) and results are synthesized.
4. **Given** ultrawork is active with a GPT-family model, **When** the
   ultrawork prompt is selected, **Then** the GPT-specific ultrawork
   variant is used (not the default/Claude variant).

---

### User Story 5 - The Orchestration Pipeline (Priority: P2)

The three-layer orchestration pipeline MUST be faithfully reproduced:

**Planning Layer**: Prometheus (planner) + Metis (gap analyzer) + Momus
(reviewer) + Oracle (architecture review). The user describes work to
Prometheus. Prometheus interviews, gathers codebase context via
explore/librarian, consults Metis for gap analysis, and optionally runs
high-accuracy dual review (Momus + Oracle both must approve). The plan
is written to `.omo/plans/{name}.md`. This layer is READ-ONLY.

**Execution Layer**: Atlas (conductor). When the user runs `/start-work`,
Atlas reads the plan, analyzes tasks, accumulates wisdom across tasks,
delegates to workers, verifies results, and produces a final report.
Atlas MUST delegate all implementation — it does not write code.

**Worker Layer**: Sisyphus-Junior (task executor via categories),
Oracle (architecture consultation), Explore (codebase grep), Librarian
(docs search), Multimodal-Looker (vision). Each worker returns results
+ learnings to Atlas.

**Wisdom Accumulation**: After each task, Atlas extracts learnings
(conventions, successes, failures, gotchas, commands) and passes them
forward to ALL subsequent subagents. A notepad system persists this:

```
.omo/notepads/{plan-name}/
├── learnings.md
├── decisions.md
├── issues.md
├── verification.md
└── problems.md
```

**Boulder State**: The system tracks active work (plans being executed)
in `.omo/boulder.json` — which plan, which session, which agent,
worktree path. `/start-work` auto-resumes a single active work or asks
the user to choose among multiple.

**Why this priority**: The pipeline is the orchestration "brain" that
makes multi-agent work coherent. Without it, agents are just independent
actors with no shared plan or accumulated knowledge.

**Independent Test**: The user runs Prometheus to plan a feature,
produces a plan in `.omo/plans/`. Then runs `/start-work`. Atlas reads
the plan, delegates tasks to Sisyphus-Junior workers, each worker's
results and learnings accumulate in `.omo/notepads/`, and Atlas verifies
completion.

**Acceptance Scenarios**:

1. **Given** Prometheus has produced a plan, **When** the user runs
   `/start-work`, **Then** Atlas reads the plan file and begins
   delegating tasks.
2. **Given** Atlas delegates task 1 to a worker, **When** the worker
   completes, **Then** the worker's learnings are extracted and passed
   to the worker for task 2.
3. **Given** a plan with a dependency graph, **When** Atlas executes,
   **Then** tasks are run in dependency order, with independent tasks
   parallelized.
4. **Given** Atlas is active, **When** it needs to write code, **Then**
   it MUST delegate to a worker — Atlas itself never edits code files.
5. **Given** a boulder state file exists with one active work, **When**
   the user runs `/start-work` without specifying a plan, **Then** the
   system auto-resumes that work.

---

### User Story 6 - Agent Activity Panel (Bottom-Right TUI) (Priority: P1)

The TUI MUST display all agent and sub-agent activity in an elegant,
information-dense panel in the **bottom-right** of the screen. This
panel conveys as much information about active agents/sub-agents as
possible in an intuitive format. This is a re-implementation of
oh-my-openagent's TUI sidebar concept, adapted to joey-agent's ratatui
TUI.

The panel shows, in real time:
- **Active primary agent** — name, color, mode (e.g. "Sisyphus -
  ultraworker" in its theme color).
- **Running sub-agents** — each background or foreground subagent: its
  agent type (explore, librarian, oracle, sisyphus-junior, etc.),
  status (running/done/failed), current phase (querying model, running
  tool X, reasoning), iteration count, elapsed time.
- **Category assignments** — when a subagent is category-spawned, show
  the category (e.g. "quick", "visual-engineering") and resolved model.
- **Task/job board** — active delegated tasks: title, status, tool-call
  count, last tool used.
- **Wisdom/learnings counter** — accumulated learnings count for the
  current plan.
- **Concurrency indicator** — X of Y parallel slots in use.

The panel uses the existing synthwave-aurora theme (gradient borders,
spinners, particle field) and updates live as AgentEvents stream in.
The panel uses a split layout: the active primary agent is always
pinned at the top (name, color, mode, resolved model). When idle (no
subagents running), the full agent roster expands below the pinned
active agent — all configured agents with their names, modes, and
resolved models. When active (subagents running), the roster
condenses/collapses and live subagent activity fills the remaining
space — animated spinners per running agent, each with type, status,
phase, iteration count, and elapsed time.

The CLI (non-TUI) MUST have parity: a compact text summary of active
agents/sub-agents printed inline as events arrive, so CLI users see the
same information (Constitution Principle II).

**Why this priority**: The user explicitly requested this — "show all
the agent activity in an intuitive format on the bottom right of the UI."
It is the primary visibility mechanism for the orchestration system.

**Independent Test**: The user triggers a delegation (e.g., Sisyphus
fires 3 parallel explore agents). The bottom-right panel immediately
shows 3 running sub-agents with spinners, their types, elapsed times,
and phases. As each completes, its spinner stops and status flips to
"done." The CLI prints equivalent inline summaries.

**Acceptance Scenarios**:

1. **Given** the TUI is idle, **When** no agents are running, **Then**
   the bottom-right panel shows the active primary agent pinned at the
   top, with the full agent roster expanding below it — all 11 agents
   listed with their names, modes, and resolved models.
2. **Given** Sisyphus fires 3 background explore agents, **When** the
   agents start, **Then** the panel shows 3 entries with animated
   spinners, each labeled "explore", with elapsed timers and phase
   indicators.
3. **Given** a subagent completes, **When** its result returns, **Then**
   its panel entry transitions to "done" (green check or equivalent),
   the spinner stops, and the final elapsed time is shown briefly before
   fading.
4. **Given** Atlas is executing a plan with 5 tasks, **When** tasks are
   delegated, **Then** the panel shows a job board: each task title,
   its status (pending/running/done/failed), tool-call count, and last
   tool used.
5. **Given** the CLI is used (non-TUI), **When** a subagent spawns,
   **Then** a compact one-line summary is printed (e.g.
   `[explore] spawned → running (model: glm-5)`), achieving CLI/TUI
   parity.
6. **Given** the terminal is narrow (e.g. 80 columns), **When** the
   panel renders, **Then** it gracefully degrades — truncating labels,
   hiding secondary info — without overflowing or breaking the layout.

---

### User Story 7 - Model Fallback Chains and Agent-Model Matching (Priority: P2)

Each agent and category has a **model fallback chain** — an ordered list
of model candidates, each with a set of acceptable providers. At
runtime, the system tries candidates in order until it finds a model
that is available (connected provider + known model). This ensures work
continues even if a preferred provider is down.

The fallback chains MUST match oh-my-openagent exactly (1-to-1). For
example:
- **Sisyphus**: claude-opus-4-8 (max) → kimi-k3 → gpt-5.6-sol (medium)
  → glm-5 → big-pickle
- **Hephaestus**: gpt-5.6-sol (medium) [requires an OpenAI-class
  provider]
- **Oracle**: gpt-5.6-sol (xhigh) → gpt-5.6-sol (high, copilot) →
  gemini-3.1-pro (high) → claude-opus-4-8 (max) → glm-5.2
- **Sisyphus-Junior**: claude-sonnet-4-6 → kimi-k3 → gpt-5.6-sol →
  minimax-m3 → minimax-m2.7 → big-pickle
- **Categories**: each has its own chain (visual-engineering → Gemini,
  ultrabrain → GPT-5.6-sol xhigh, quick → gpt-5.4-mini, etc.)

Model-family-specific prompt variants MUST be supported: when a model
is resolved, the system selects the matching prompt variant for that
agent (e.g., Sisyphus has Claude-default, GLM-5.2, GPT, Kimi-K3, Gemini
variants; Atlas has default, gpt, gemini, kimi, kimi-k3, kimi-k2-7,
opus-4-7, glm variants; Sisyphus-Junior has 9 variants).

**Why this priority**: Fallback chains are the reliability mechanism
that makes multi-model orchestration practical. Without exact chain
reproduction, the system is fragile and not a true 1-to-1 port.

**Independent Test**: Configure only a Z.ai/GLM provider. Start the
system. Sisyphus resolves to glm-5 (4th in its chain) and the GLM-5.2
prompt variant is used. All agents that can use GLM activate; agents
requiring OpenAI (Hephaestus) are gracefully skipped.

**Acceptance Scenarios**:

1. **Given** only an Anthropic provider is configured, **When** Sisyphus
   resolves its model, **Then** claude-opus-4-8 is selected (first in
   chain) and the Claude-default prompt variant is used.
2. **Given** only a Z.ai provider is configured, **When** Sisyphus
   resolves, **Then** glm-5 is selected (first available in chain) and
   the GLM-5.2 prompt variant is used.
3. **Given** Hephaestus requires an OpenAI-class provider and none is
   connected, **When** agents are registered, **Then** Hephaestus is
   gracefully skipped (not offered in the Tab picker).
4. **Given** a category's primary model is unavailable, **When** the
   fallback chain is walked, **Then** the next available model is used
   silently.
5. **Given** the user overrides an agent's model in config, **When**
   the agent is registered, **Then** the user's model is honored
   directly, bypassing the fallback chain.

---

### User Story 8 - Slash Commands: /start-work, @plan, /goal (Priority: P3)

The orchestration system exposes slash commands that drive the pipeline:

- **`/start-work [plan-name]`** — Activates Atlas on a plan. Reads the
  plan from `.omo/plans/{name}.md` (or auto-selects/resumes). Atlas
  becomes the active agent and begins executing the plan's tasks via
  delegation. Manages boulder state (`.omo/boulder.json`).
- **`@plan "description"`** — From Sisyphus, delegates to Prometheus to
  create a plan. Equivalent to switching to Prometheus and describing
  work.
- **`/goal [set <text> | pause | resume | clear | show]`** — Manages a
  persistent per-session objective. When set and active, the goal is
  re-injected as a continuation prompt on every idle until a completion
  audit confirms the work is done. This is the "boulder pushing"
  discipline mechanism.
- **`ultrawork` / `ulw`** — (Covered in User Story 4) Activates
  ultrawork mode.
- **`hyperplan`** — Activates the adversarial hyperplan workflow
  (optional, loads the hyperplan skill).

These commands integrate with the existing joey-cli slash command
infrastructure (`crates/joey-cli/src/slash.rs`) and are available in
both CLI and TUI.

**Why this priority**: Slash commands are the control surface for the
orchestration pipeline. They are lower priority than the agents
themselves but essential for driving the plan→execute workflow.

**Independent Test**: The user types `/start-work my-feature`. Atlas
reads `.omo/plans/my-feature.md`, begins delegating tasks, and the
activity panel shows the execution. The user types `/goal set Ship the
dashboard` and the goal persists across turns.

**Acceptance Scenarios**:

1. **Given** a plan exists at `.omo/plans/my-feature.md`, **When** the
   user types `/start-work my-feature`, **Then** Atlas activates, reads
   the plan, and begins executing.
2. **Given** no plan name is specified and one active work exists,
   **When** the user types `/start-work`, **Then** the system
   auto-resumes that work.
3. **Given** the user types `/goal set Ship feature X`, **When** the
   goal is set, **Then** it persists and is re-injected on subsequent
   idle turns until cleared or completed.
4. **Given** the user types `/goal pause`, **When** the goal is paused,
   **Then** continuation prompts stop until `/goal resume`.
5. **Given** the user types `@plan "refactor auth"`, **When** in
   Sisyphus mode, **Then** Prometheus is invoked to create the plan.

---

### User Story 9 - Team Mode (Optional, Off by Default) (Priority: P3)

Team mode is parallel multi-agent orchestration, OFF by default. When
enabled, multiple agents run as team members with a shared mailbox and
shared task list. This is a faithful but optionally-enabled reproduction
of oh-my-openagent's team mode.

Team member eligibility:
- **Eligible**: sisyphus, atlas, sisyphus-junior
- **Conditional**: hephaestus (requires teammate permission enablement)
- **Hard-reject**: oracle, librarian, explore, multimodal-looker,
  metis, momus, prometheus (these are read-only/consultant agents
  unsuitable as parallel team members)

Team members communicate via a shared mailbox (message passing) and
coordinate via a shared task list. Configuration includes max parallel
members (default 4), max total members (default 8), message size limits,
and polling intervals.

**Why this priority**: Team mode is an advanced feature that extends
the orchestration system but is not core to the 1-to-1 agent
re-implementation. It is the lowest priority.

**Independent Test**: The user enables team mode in config, defines a
team spec with 3 sisyphus-junior members, and starts a team session.
The members coordinate via shared mailbox, divide tasks, and report
results.

**Acceptance Scenarios**:

1. **Given** team mode is disabled (default), **When** the system runs,
   **Then** team features are invisible and no team infrastructure
   activates.
2. **Given** team mode is enabled, **When** the user defines a team with
   eligible members, **Then** members spawn and communicate via the
   shared mailbox.
3. **Given** a team spec includes a hard-rejected agent (e.g., oracle),
   **When** the team is validated, **Then** the member is rejected with
   a clear error message listing eligible agents.
4. **Given** team mode is active with tmux visualization enabled, **When**
   members run, **Then** an optional tmux layout visualizes each member's
   activity.

---

### Edge Cases

- What happens when ALL models in an agent's fallback chain are
  unavailable? → The agent is gracefully skipped during registration
  (not offered in Tab picker, not available for delegation). A
  diagnostic message is logged. The system continues with available
  agents.
- What happens when a subagent fails mid-delegation (provider error,
  timeout, crash)? → The subagent's error is captured, reported as a
  failed result to the parent, and does NOT abort sibling subagents in
  a parallel batch. The parent can retry via session continuation.
- What happens when the user presses Tab during agent registration
  (before agents are loaded)? → Tab is queued or shows a "loading"
  indicator; the picker appears once agents are registered.
- What happens when two primary agents try to be active simultaneously
  (race condition)? → Only one primary agent is active per session.
  Switching is atomic — the previous agent's turn completes (or is
  interrupted) before the new one takes effect.
- What happens when the `.omo/` directory does not exist? → It is
  created lazily on first write (plans, notepads, boulder state).
- What happens when a plan file is malformed or missing required
  sections? → Atlas reports the parsing error and asks the user to fix
  or regenerate the plan via Prometheus.
- What happens when the terminal is too small for the activity panel?
  → The panel degrades gracefully: hides secondary info, truncates
  labels, or collapses to a minimal summary line.
- What happens when model-family detection is ambiguous (model ID
  matches multiple families)? → The resolution order is deterministic
  (most specific match first: exact version → family → default).
- What happens when background task concurrency exceeds the configured
  limit (default 5)? → Tasks are queued and dispatched as slots free up,
  keyed by model/provider routing to avoid provider rate-limit
  collisions.
- What happens when the user switches agents mid-ultrawork? → The
  ultrawork injection is cleared; the new agent starts fresh with its
  standard prompt. The goal/notepad state persists if set.

## Requirements

### Functional Requirements

#### Agent Registry and Identity

- **FR-001**: System MUST define exactly 11 built-in agents with these
  names: `sisyphus`, `hephaestus`, `prometheus`, `atlas` (primary) and
  `oracle`, `librarian`, `explore`, `multimodal-looker`, `metis`,
  `momus`, `sisyphus-junior` (subagent).
- **FR-002**: Each agent MUST have: a display name, a system prompt
  builder, a color, a mode (primary or subagent), a model fallback chain,
  temperature, max tokens, and tool permission configuration.
- **FR-003**: The canonical assembly order for primary agents in the Tab
  picker MUST be: Default → Sisyphus → Hephaestus → Prometheus → Atlas.
  The existing joey-agent default agent is the 0th entry (backward
  compatibility); the 4 OMO primary agents follow. This order is used in
  the Tab picker and agent listing.
- **FR-004**: Each agent MUST support model-family-specific system prompt
  variants. When a model is resolved, the matching variant is selected.
  Agents without a specific variant fall back to a default variant.
- **FR-005**: System MUST register agents at startup, resolving each
  agent's model via its fallback chain against available providers, and
  gracefully skip agents whose models are all unavailable or whose
  required providers are not connected.

#### Model Fallback Chains

- **FR-006**: System MUST store the exact model fallback chains for all
  11 agents and all categories as defined in oh-my-openagent's
  `agent-model-requirements` and `category-model-requirements`.
- **FR-007**: Model resolution MUST try fallback chain entries in order,
  selecting the first entry whose model is available via a connected
  provider. Within each chain entry, exact model ID is tried first
  (direct match); if unavailable, family-level fuzzy matching resolves
  the entry against joey-agent's actually-configured providers (e.g.,
  "claude-opus-4-8" matches the Anthropic family — any available
  Opus-class model from an Anthropic provider satisfies the entry). If
  no entry resolves, the agent is skipped.
- **FR-008**: Each fallback chain entry specifies: a list of acceptable
  providers, a model ID, and an optional variant (e.g., "max", "high",
  "medium", "xhigh", "low"). The variant is applied to the resolved
  agent config.
- **FR-009**: User-configured agent model overrides MUST take precedence
  over fallback chain resolution. If a user pins a model, it is used
  directly.
- **FR-010**: System MUST support a `requiresProvider` constraint
  (Hephaestus requires an OpenAI-class provider) and a `requiresAnyModel`
  constraint (Sisyphus requires at least one model in its chain).

#### Categories

- **FR-011**: System MUST support these delegate-task categories:
  `visual-engineering`, `ultrabrain`, `deep`, `artistry`, `quick`,
  `unspecified-low`, `unspecified-high`, `writing`, `quick-rust`,
  `quick-zig`, `git`.
- **FR-012**: Each category MUST have: a model fallback chain, a
  temperature, a description, and an optional prompt append. Categories
  are user-configurable and extendable.
- **FR-013**: Category delegation MUST dispatch through Sisyphus-Junior
  (the task executor), regardless of category name.
- **FR-014**: Category and subagent_type MUST be mutually exclusive in a
  single delegation call — providing both is an error.

#### Agent Mode Switching (Tab)

- **FR-015**: TUI MUST respond to Tab keypress by presenting a mode
  picker listing all available primary agents in canonical order (Default
  → Sisyphus → Hephaestus → Prometheus → Atlas), with their colors,
  descriptions, and resolved models.
- **FR-016**: Shift+Tab (or reverse cycling) MUST cycle backwards
  through the agent list.
- **FR-017**: Selecting an agent MUST immediately rebuild the system
  prompt, swap the model, apply tool permissions, and update the TUI
  status bar / CLI prompt indicator.
- **FR-018**: Agent switching MUST be deferred if a turn is in progress
  (Busy mode) — the switch takes effect on the next user turn.
- **FR-019**: CLI (non-TUI) MUST provide equivalent agent switching via
  a numbered menu or cycling command, achieving CLI/TUI parity.
- **FR-020**: The Tab picker MUST only show primary agents whose models
  resolved successfully; skipped agents (unavailable models) are hidden.

#### IntentGate and Working Modes

- **FR-021**: The Sisyphus and Hephaestus system prompts MUST include a
  Phase 0 IntentGate that classifies user intent (trivial, explicit,
  exploratory, open-ended, ambiguous) and routes accordingly.
- **FR-022**: System MUST detect the keyword `ultrawork` or `ulw` in
  user messages and inject the ultrawork instruction set into the active
  agent's prompt. Ultrawork is valid on Default, Sisyphus, Hephaestus,
  and Atlas. When Prometheus is the active agent, ultrawork is ignored
  (not injected) because Prometheus is read-only and ultrawork's
  mandatory-implementation semantics are incompatible with it.
- **FR-023**: Ultrawork MUST have model-family-specific prompt variants
  (default/Claude, GPT, Gemini, GLM, planner). The correct variant is
  selected based on the active model family.
- **FR-024**: When ultrawork activates, the agent MUST announce
  "ULTRAWORK MODE ENABLED!" as its first response.
- **FR-025**: Prometheus MUST be a read-only planner: it can only
  create/modify files within `.omo/`. It MUST NOT implement code. Its
  prompt enforces this. Prometheus's planning workflow (interview
  protocol, clearance checks, Metis consultation, plan template,
  high-accuracy review loop) MUST be ported as a joey-agent skill file
  (`ulw-plan`), preserving OMO's skill-based architecture. The
  Prometheus system prompt loads and references this skill rather than
  inlining its content.
- **FR-026**: System MUST support `hyperplan` keyword detection and
  combo detection (`hyperplan ultrawork` / `ultrawork hyperplan`).

#### Orchestration Pipeline

- **FR-027**: System MUST implement the `/start-work` command that
  activates Atlas on a plan file, managing boulder state in
  `.omo/boulder.json`.
- **FR-028**: Atlas MUST read the plan, delegate tasks to workers
  (Sisyphus-Junior via categories, Oracle, Explore, Librarian), verify
  results, and accumulate wisdom.
- **FR-029**: Wisdom accumulation MUST extract learnings after each task
  and pass them to subsequent subagents. Learnings persist in
  `.omo/notepads/{plan-name}/`.
- **FR-030**: System MUST support session continuation IDs for
  subagents, allowing follow-up interactions with the same subagent
  context via `task_id`.
- **FR-031**: Background tasks MUST run concurrently with a configurable
  limit (default 5), keyed by model/provider routing key.
- **FR-032**: System MUST support the `/goal` command family: set,
  pause, resume, clear, show. An active goal is re-injected as a
  continuation prompt on idle turns.

#### Agent Activity Panel (TUI)

- **FR-033**: TUI MUST render an agent activity panel in the bottom-right
  region of the screen, using a split layout: the active primary agent
  is always pinned at the top (name, color, mode, resolved model). The
  remaining space shows either the full roster (idle) or live subagent
  activity (active).
- **FR-034**: When idle, the panel MUST show the agent roster below the
  pinned active agent: all registered agents with names, modes, and
  resolved models. When subagents start, the roster condenses/collapses.
- **FR-035**: When active, the panel MUST show each running subagent
  below the pinned active agent: type, status (running/done/failed),
  phase, iteration count, elapsed time, resolved model, and category
  (if applicable).
- **FR-036**: The panel MUST show a job board for delegated tasks: title,
  status, tool-call count, last tool used.
- **FR-037**: The panel MUST show a concurrency indicator (X of Y
  parallel slots in use) and accumulated learnings count.
- **FR-038**: The panel MUST use the existing synthwave-aurora theme
  (gradient borders, spinners) and update live via the AgentEvent stream.
- **FR-039**: The panel MUST degrade gracefully on narrow terminals
  (truncate, hide secondary info) without breaking layout.
- **FR-040**: CLI MUST print compact inline summaries of agent/sub-agent
  activity as events arrive, achieving CLI/TUI information parity.

#### Team Mode

- **FR-041**: System MUST support team mode (parallel multi-agent
  orchestration) as an opt-in feature, OFF by default.
- **FR-042**: Team member eligibility MUST enforce: eligible (sisyphus,
  atlas, sisyphus-junior), conditional (hephaestus with permission),
  hard-reject (oracle, librarian, explore, multimodal-looker, metis,
  momus, prometheus).
- **FR-043**: Team members MUST communicate via a shared mailbox and
  coordinate via a shared task list.
- **FR-044**: Team mode configuration MUST include: max parallel members
  (default 4), max total members (default 8), message size limits,
  polling intervals, and optional tmux visualization.

#### Integration and Non-Regression

- **FR-045**: The orchestration agents MUST integrate with the existing
  `joey-orchestration` crate (SubagentManager, DelegationRequest) —
  extending, not replacing, the existing delegation infrastructure.
- **FR-046**: All new code MUST live in dedicated crate(s) under
  `crates/` per Constitution Principle I (Workspace-First Rust).
- **FR-047**: `cargo build --workspace` and `cargo test --workspace`
  MUST remain green — no regressions to existing functionality
  (Constitution Principle VII).
- **FR-048**: Existing CLI flags, TUI keybindings (except the new Tab
  behavior), and config keys MUST remain backward-compatible.

### Key Entities

- **Agent**: A named orchestration role with a system prompt, model
  chain, color, mode (primary/subagent), and tool permissions. 11
  built-in agents. Identity: name, display_name, mode, color.
- **AgentPromptVariant**: A model-family-specific version of an agent's
  system prompt (e.g., "default", "gpt", "gemini", "kimi-k3", "glm").
  Selected at runtime based on the resolved model.
- **ModelRequirement**: A fallback chain — an ordered list of
  {providers, model, variant?} entries. Shared structure for both agents
  and categories.
- **Category**: A semantic delegation target (visual-engineering,
  ultrabrain, etc.) with its own model chain and prompt append.
  Dispatches through Sisyphus-Junior.
- **AgentMode**: Either "primary" (selectable via Tab) or "subagent"
  (invoked via delegation).
- **BoulderState**: Persistent work-tracking state in
  `.omo/boulder.json` — which plan is active, which session, which agent,
  worktree path. Enables `/start-work` resume.
- **NotepadStore**: Wisdom accumulation files under
  `.omo/notepads/{plan-name}/` — learnings, decisions, issues,
  verification, problems.
- **GoalState**: A persistent per-session objective that drives
  continuation prompts until completion audit passes.
- **TeamSpec**: A definition of team members (each with kind: category
  or subagent_type, prompt) and team-level configuration.

## Success Criteria

### Measurable Outcomes

- **SC-001**: All 11 oh-my-openagent agents are present and selectable,
  each producing a distinct system prompt and behavioral profile
  matching the source project's semantics.
- **SC-002**: A user can switch between all 4 primary agents (Sisyphus,
  Hephaestus, Prometheus, Atlas) by pressing Tab within 1 second, with
  the active agent and its color immediately visible in the TUI status
  bar.
- **SC-003**: The agent activity panel in the bottom-right of the TUI
  shows real-time status of all running sub-agents (type, phase,
  elapsed time, model) updating at least once per second, with zero
  perceptible lag.
- **SC-004**: Category-based delegation routes to the correct model
  family 100% of the time — a `visual-engineering` task uses a
  Gemini-class model, a `quick` task uses a mini/flash model — when
  those providers are configured.
- **SC-005**: Model fallback chains resolve correctly: when a preferred
  provider is down, the system transparently falls back to the next
  available model in the chain without user intervention.
- **SC-006**: The full plan→execute pipeline works end-to-end: Prometheus
  produces a plan in `.omo/plans/`, `/start-work` activates Atlas, Atlas
  delegates to workers, wisdom accumulates in `.omo/notepads/`, and
  tasks complete with verification.
- **SC-007**: CLI users see equivalent agent activity information
  (compact inline summaries) as TUI users — no information asymmetry
  between interfaces.
- **SC-008**: The system adds zero regressions: all existing tests pass,
  existing CLI flags and TUI keybindings work unchanged (except the
  enhanced Tab behavior), and existing config keys are honored.
- **SC-009**: A parallel delegation of 5 independent subagents completes
  in wall-clock time close to the slowest single subagent (not the sum),
  demonstrating effective parallel execution.
- **SC-010**: The 1-to-1 fidelity is verifiable: agent names, model
  chains, category lists, and behavioral rules can be diffed against the
  oh-my-openagent source and match exactly.

## Assumptions

- The oh-my-openagent source code at `/Users/joey/Development/oh-my-openagent`
  (dev branch) is the authoritative reference for all agent names, model
  chains, prompt semantics, and orchestration routines.
- The existing `joey-orchestration` crate (SubagentManager,
  DelegationRequest, SubagentRole, parallel batch dispatch, concurrency
  limiter, interrupt handling) provides the delegation infrastructure
  that the OMO agents build on top of.
- The existing `joey-tui` crate (App state, widgets, theme, animation)
  provides the rendering infrastructure; the activity panel is an
  additional widget/region, not a rewrite.
- The existing `joey-cli` slash command infrastructure (`slash.rs`)
  provides the command dispatch mechanism; `/start-work`, `/goal`, etc.
  are new commands registered into it.
- Provider and model availability is determined by the existing
  `joey-providers` infrastructure; "available models" means models
  resolvable through connected providers.
- Model fallback chains preserve OMO's exact chain order, families, and
  variants (1-to-1 structure fidelity), but each chain entry resolves
  via family-level fuzzy matching against joey-agent's actually-
  configured providers. Exact OMO model IDs (claude-opus-4-8, kimi-k3,
  gpt-5.6-sol, glm-5, etc.) are tried as direct matches first; family-
  level aliasing (Anthropic family, GPT family, Kimi family, GLM family,
  Gemini family, MiniMax family) is the fallback within each entry.
  This adapts OMO's routing semantics to joey-agent's provider model
  without forcing users to replicate OMO's OpenCode-specific provider
  namespaces.
- The `.omo/` directory (plans, notepads, boulder state) is the
  orchestration artifact directory, matching oh-my-openagent's
  convention. This is distinct from `.specify/` (spec-kit artifacts).
- Color values, display names, and behavioral descriptions are preserved
  exactly from oh-my-openagent (e.g., Atlas color #10B981, Hephaestus
  color #D97706, Sisyphus-Junior color #20B2AA).
- Team mode is implemented but OFF by default; it is not required for
  the core 1-to-1 agent experience and may be delivered as a later
  increment.
- The `ulw-plan` skill referenced in Prometheus's prompt is ported as a
  joey-agent skill file, preserving OMO's skill-based architecture
  1-to-1. The Prometheus system prompt loads and references the skill
  (containing the interview protocol, clearance checks, Metis
  consultation workflow, plan template, and high-accuracy review loop)
  rather than inlining its content. This allows the planning workflow
  to be iterated independently of the prompt builder.
