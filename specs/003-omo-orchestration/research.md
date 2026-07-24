# Research: Oh My OpenAgent Orchestration

**Feature**: 003-omo-orchestration | **Date**: 2026-07-23

## Source of Truth

The oh-my-openagent source code at `/Users/joey/Development/oh-my-openagent`
(dev branch) is the authoritative reference. Key files studied:

- `packages/model-core/src/agent-model-requirements.ts` — all 11 agent
  fallback chains (providers, model IDs, variants).
- `packages/model-core/src/category-model-requirements.ts` — all 11 category
  fallback chains.
- `packages/omo-opencode/src/agents/sisyphus/default.ts` — Sisyphus prompt
  builder (IntentGate, delegation tables, todo discipline).
- `packages/omo-opencode/src/agents/hephaestus/agent.ts` + `gpt.ts` —
  Hephaestus autonomous deep-worker prompt.
- `packages/omo-opencode/src/agents/prometheus/system-prompt.ts` — Prometheus
  read-only planner prompt.
- `packages/omo-opencode/src/agents/atlas/agent.ts` — Atlas conductor prompt.
- `packages/omo-opencode/src/agents/sisyphus-junior/agent.ts` + `default.ts` —
  Sisyphus-Junior task executor.
- `packages/omo-opencode/src/agents/builtin-agents/general-agents.ts` —
  Oracle, Librarian, Explore, Metis, Momus registration.
- `packages/omo-opencode/src/hooks/keyword-detector/` — ultrawork, hyperplan,
  team keyword detection.
- `packages/prompts-core/src/ultrawork-prompts.ts` — ultrawork prompt variants.
- `packages/omo-codex/plugin/skills/ulw-plan/SKILL.md` — the ulw-plan skill.
- `packages/boulder-state/src/` — boulder state persistence.
- `packages/team-core/src/` — team mode configuration and eligibility.

## Decision 1: New Crate `joey-omo`

**Decision**: Create a single new crate `crates/joey-omo` for the entire OMO
orchestration system.

**Rationale**: Constitution Principle I (workspace-first) requires new features
in dedicated crates. The OMO system is cohesive (agents, models, categories,
pipeline state) and large enough (~12 modules) to warrant its own crate. It
builds on `joey-orchestration` (the existing delegation engine) by composing
its public API, not modifying it.

**Alternatives considered**:
- Add modules to `joey-orchestration` directly. Rejected: violates separation
  of concerns (orchestration = delegation mechanics; OMO = agent definitions
  and pipeline semantics). Would bloat the existing crate.
- Multiple crates (joey-omo-agents, joey-omo-pipeline, joey-omo-ui). Rejected:
  over-modularization for ~3K lines. One crate with clear module boundaries is
  simpler and faster to compile.

## Decision 2: Model Fallback Chain Family-Level Fuzzy Matching

**Decision**: Preserve OMO's exact chain order, families, and variants, but
resolve each chain entry via two-level matching: (1) exact model ID direct
match, (2) family-level aliasing against joey-agent's configured providers.

**Rationale**: joey-agent has its own provider infrastructure
(`joey-providers`) with provider names and model IDs that don't match OMO's
OpenCode-specific namespaces (e.g., "opencode-go", "bailian-coding-plan",
"moonshotai-cn"). A 1-to-1 port of the chain *structure* (which family is
tried first, variant, order) preserves OMO's routing semantics. Family-level
aliasing maps: `claude-*` → Anthropic family, `gpt-*` → OpenAI/GPT family,
`kimi-*` → Kimi/Moonshot family, `glm-*` → GLM/Z.ai family, `gemini-*` →
Google family, `minimax-*` → MiniMax family.

**Implementation**: A `ModelFamily` enum with detection regexes. The resolver
walks the chain; for each entry, it first checks if the exact model ID is in
the available-models set; if not, it checks if any model in the same family is
available. The available-models set is built at startup from
`joey-providers::copilot::fetch_model_catalog` and the configured provider
profiles.

**Alternatives considered**:
- Hardcode exact OMO model IDs. Rejected: forces users to configure
  OpenCode-specific providers they may not have. Brittle.
- User must manually configure each agent's model. Rejected: defeats the
  purpose of fallback chains (automatic model routing).

## Decision 3: Tab Key Repurposing with Mitigation

**Decision**: Tab cycles agent mode. Transcript focus moves to Up-arrow (from
input) and existing scroll keys (j/k, PageUp/Down) work regardless of focus.

**Rationale**: The user explicitly requested Tab ("I can switch between the
different agent mode by pressing tab"). The existing Tab behavior (toggle
focus Input ↔ Transcript) is a convenience, not essential — the transcript is
scrollable via j/k/PageUp/Down from any focus state. The help overlay is
updated.

**Alternatives considered**:
- Use F2 or Ctrl+T for agent switching, preserve Tab. Rejected: contradicts
  explicit user request. User said Tab.
- Tab opens a popup picker (not just cycling). **Chosen**: Tab opens an overlay
  picker listing all 5 agents; arrow keys + Enter select. Shift+Tab cycles
  backwards. Pressing Tab repeatedly cycles forward through the list.

## Decision 4: Prompt Storage — Include Files, Not External Markdown

**Decision**: Store OMO prompts as Rust `&str` constants in
`joey-omo/src/agents/prompts/`, ported from OMO's markdown files. Model-family
variant selection is a function of the resolved model.

**Rationale**: OMO uses external markdown files loaded at runtime via Vite's
`import ... from "....md"`. In Rust, compile-time `&str` constants are zero-
cost, type-safe, and avoid runtime file I/O. The prompts are long (Sisyphus's
default prompt is ~4K) but static. Variant selection is a match on the model
family.

**Alternatives considered**:
- External markdown files loaded via `include_str!`. Rejected: `include_str!`
  produces `&'static str` at compile time, same as a constant — but adds file
  management complexity. Direct constants are simpler.
- Runtime file loading. Rejected: violates Constitution VIII (unnecessary I/O)
  and adds failure modes (missing files).

## Decision 5: Activity Panel Layout — Split (Pinned Active + Roster/Activity)

**Decision**: The bottom-right panel uses a split layout. The active primary
agent is always pinned at the top (name, color, mode, resolved model). When
idle, the full 11-agent roster expands below. When active (subagents running),
the roster condenses and live subagent activity fills the space.

**Rationale**: Clarification Q4 answer (Option B). Preserves context about
which agent mode is active while maximizing subagent information density.

**Implementation**: The existing TUI layout splits the body into
`[transcript (left, Min 40), activity sidebar (right, Length 34)]`. The new
OMO panel replaces the activity sidebar with a richer widget: top 3 rows =
pinned active agent + concurrency indicator; remaining rows = either roster
(idle) or subagent entries (active). Graceful degradation: on narrow terminals
(<72 cols), the panel hides entirely (existing behavior).

**Alternatives considered**:
- Full replace (idle=roster, active=activity only). Rejected: loses active-
  agent context during delegation.
- Two separate panels (roster + activity side by side). Rejected: insufficient
  horizontal space in an 80-col terminal.

## Decision 6: ulw-plan as a joey-agent Skill File

**Decision**: Port the ulw-plan workflow as a skill file under the joey-agent
skills system (e.g., `skills/ulw-plan/SKILL.md`), preserving OMO's architecture.

**Rationale**: Clarification Q5 answer (Option A). Matches the user's "1-to-1
re-implementation" directive. More maintainable (skill can be iterated
independently of the prompt builder). joey-agent already has a skills system.

**Implementation**: The OMO `ulw-plan` SKILL.md and its references
(`full-workflow.md`, `intent-clear.md`, `intent-unclear.md`) are ported to
joey-agent's skills directory. The Prometheus system prompt references it via
the existing skill-loading mechanism.

**Alternatives considered**:
- Inline the workflow into Prometheus's prompt. Rejected: less maintainable,
  diverges from OMO's architecture, makes the prompt enormous.

## Decision 7: No New External Dependencies

**Decision**: The entire feature composes from existing workspace dependencies.

**Rationale**: Constitution Principle VIII. The workspace already has: tokio
(async), serde/serde_json (serialization), ratatui (TUI), crossterm (input),
regex (keyword detection), chrono (timestamps in boulder state), uuid (session
IDs). No new crates needed.

**Dependency weight audit**: Zero new dependencies. Binary size impact is
negligible (prompt constants compile to read-only data; agent registry logic
is small). Compile time impact: one new crate (~3K lines) adds seconds to
incremental builds, acceptable.

## Decision 8: Boulder/Notepad/Goal State via std::fs (No Database)

**Decision**: Use JSON files under `.omo/` for boulder state, goal state, and
markdown files for notepads. No SQLite for OMO state.

**Rationale**: OMO state is infrequent-write, small-volume, and human-readable
(users inspect `.omo/boulder.json`). SQLite is already used for session
history (joey-core SessionDb) but adding OMO tables would couple the
orchestration layer to the session DB unnecessarily. std::fs JSON is simpler,
transparent, and Constitution VIII-friendly (no query overhead).

**Alternatives considered**:
- SQLite tables in SessionDb. Rejected: over-engineering for small,
  infrequent writes. Couples layers unnecessarily.

## Decision 9: AgentEvent Extensions (Additive, Non-Breaking)

**Decision**: Add new event variants to the existing `AgentEvent` enum in
`joey-agent-core/src/events.rs`.

**Rationale**: The existing enum already has orchestration variants
(`SubagentSpawn`, `SubagentComplete`, `SubagentFailed`,
`DelegationBatchComplete`). OMO needs additional variants for: agent mode
switching, category delegation, boulder/goal state changes, wisdom
accumulation. Enum extension in Rust is backward-compatible if existing match
arms use `_` (which they do — see TUI `state.rs::apply`).

**New variants**:
- `AgentModeChanged { agent_name: String, model: String }`
- `CategoryDelegation { category: String, model: String }`
- `BoulderWorkStarted { plan_name: String }`
- `BoulderWorkResumed { plan_name: String }`
- `BoulderWorkCompleted { plan_name: String }`
- `GoalSet { objective: String }`
- `GoalCleared`
- `WisdomAccumulated { plan_name: String, learnings_count: usize }`

## Decision 10: Prompt Variant Resolution Pipeline

**Decision**: A `resolve_prompt_variant(model, &variants) -> &str` function
that matches the model family against available variants in priority order.

**Rationale**: OMO uses a `resolveVariant` function in `prompts-core`. In
Rust, this is a match on `ModelFamily::detect(model)` returning a variant key.
Each agent's prompt module exposes a `variants()` function returning a
`HashMap<&str, &str>` or a match statement.

**Resolution order** (per agent, from OMO source):
- Sisyphus: claude-opus-4-7 → glm-5-2 → gpt → kimi-k3 → gemini → default
- Atlas: opus-4-7 → gpt → gemini → kimi-k3 → kimi-k2-7 → kimi → glm → default
- Sisyphus-Junior: kimi-k3 → kimi-k2-7 → kimi-k2 → gpt-5-5 → gpt-5-4 → gpt → gemini → glm-5-2 → default
- Hephaestus: gpt-5-6 → gpt-5-5 → gpt-5-4 → gpt (GPT-only)
- Ultrawork: planner → gpt → gemini → glm → default

**Alternatives considered**:
- A generic priority-list approach. Rejected: each agent has different variant
  keys; a match statement is clearer and type-safe.
