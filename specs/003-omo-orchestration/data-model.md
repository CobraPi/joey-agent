# Data Model: Oh My OpenAgent Orchestration

**Feature**: 003-omo-orchestration | **Date**: 2026-07-23

## Core Types

### OmoAgent

The identity and configuration of one of the 11 built-in agents.

| Field | Type | Description |
|-------|------|-------------|
| `name` | `String` | Canonical machine name (e.g., "sisyphus", "oracle") |
| `display_name` | `String` | Human label (e.g., "Sisyphus", "Sisyphus - ultraworker") |
| `mode` | `AgentMode` | `Primary` (Tab-selectable) or `Subagent` (delegation-invoked) |
| `color` | `String` | Hex color (e.g., "#10B981" for Atlas, "#D97706" for Hephaestus) |
| `description` | `String` | One-line description for the Tab picker |
| `model_requirement` | `ModelRequirement` | Fallback chain for model resolution |
| `temperature` | `f64` | Sampling temperature (default 0.1 for most, 0.5 for categories) |
| `max_tokens` | `Option<u32>` | Output token cap (e.g., 64000 for Junior, 32000 for Hephaestus) |
| `tool_permissions` | `ToolPermissions` | Allow/deny per-tool map (Junior blocks `task`, allows `call_omo_agent`) |
| `prompt_builder` | `fn(model) -> String` | Function selecting the model-family-specific prompt variant |

**Validation**: name must be one of the 11 canonical names. Primary agents
must be in {sisyphus, hephaestus, prometheus, atlas}. Subagent agents in
{oracle, librarian, explore, multimodal-looker, metis, momus,
sisyphus-junior}.

### AgentMode

```rust
pub enum AgentMode {
    Primary,   // Tab-selectable: sisyphus, hephaestus, prometheus, atlas
    Subagent,  // Delegation-invoked: oracle, librarian, explore, etc.
}
```

### ModelRequirement

A fallback chain — ordered list of model candidates, each with acceptable
providers, a model ID, and an optional variant.

| Field | Type | Description |
|-------|------|-------------|
| `fallback_chain` | `Vec<FallbackEntry>` | Ordered candidates (tried first → last) |
| `requires_any_model` | `bool` | If true, agent activates if ANY chain entry resolves |
| `requires_provider` | `Option<Vec<String>>` | If set, at least one listed provider must be connected |

### FallbackEntry

One candidate in a fallback chain.

| Field | Type | Description |
|-------|------|-------------|
| `providers` | `Vec<String>` | Acceptable provider namespaces (family-level) |
| `model` | `String` | Model ID (e.g., "claude-opus-4-8") |
| `variant` | `Option<String>` | Effort variant ("max", "high", "medium", "xhigh", "low") |

### ModelFamily

```rust
pub enum ModelFamily {
    Anthropic,   // claude-*
    Gpt,         // gpt-*
    Kimi,        // kimi-*
    Glm,         // glm-*
    Gemini,      // gemini-*
    Minimax,     // minimax-*
    Unknown,
}
```

Detection via regex/model-prefix matching (see research.md Decision 2).
Used for fuzzy chain resolution and prompt variant selection.

### CategoryConfig

A semantic delegation target.

| Field | Type | Description |
|-------|------|-------------|
| `name` | `String` | Category name (e.g., "visual-engineering", "quick") |
| `description` | `String` | What this category is for |
| `model_requirement` | `ModelRequirement` | Fallback chain for this category |
| `temperature` | `Option<f64>` | Override (default 0.5) |
| `prompt_append` | `Option<String>` | Optional text appended to Junior's prompt |

**Built-in categories** (11): visual-engineering, ultrabrain, deep, artistry,
quick, unspecified-low, unspecified-high, writing, quick-rust, quick-zig, git.

## State Entities (`.omo/` files)

### BoulderState (`.omo/boulder.json`)

Tracks active plan-execution work.

| Field | Type | Description |
|-------|------|-------------|
| `works` | `Vec<BoulderWork>` | Active work entries |
| `version` | `u32` | Schema version |

**State transitions**: created (on `/start-work` with a plan) → active (Atlas
executing) → completed (all tasks done). Multiple works can coexist; one is
`active`.

### BoulderWork

| Field | Type | Description |
|-------|------|-------------|
| `id` | `String` | Unique work ID |
| `plan_path` | `String` | Path to `.omo/plans/{name}.md` |
| `plan_name` | `String` | Plan slug (derived from filename) |
| `session_id` | `String` | Agent session executing this work |
| `agent` | `String` | Agent name (usually "atlas") |
| `worktree_path` | `Option<String>` | Optional git worktree path |
| `status` | `BoulderWorkStatus` | Active, Completed, Abandoned |
| `started_at` | `String` | ISO 8601 timestamp |

### BoulderWorkStatus

```rust
pub enum BoulderWorkStatus {
    Active,
    Completed,
    Abandoned,
}
```

### GoalState (`.omo/goals.json`)

Per-session persistent objective.

| Field | Type | Description |
|-------|------|-------------|
| `session_id` | `String` | Owning session |
| `objective` | `String` | The goal text |
| `status` | `GoalStatus` | Active, Paused |
| `set_at` | `String` | ISO 8601 timestamp |

**State transitions**: Active (continuation prompts injected on idle) → Paused
(no injection) → Cleared (removed). `/goal set` → Active; `/goal pause` →
Paused; `/goal resume` → Active; `/goal clear` → removed.

### NotepadStore (`.omo/notepads/{plan-name}/`)

Wisdom accumulation across tasks. Five markdown files per plan:

| File | Content |
|------|---------|
| `learnings.md` | Patterns, conventions, successful approaches |
| `decisions.md` | Architectural choices and rationales |
| `issues.md` | Problems, blockers, gotchas encountered |
| `verification.md` | Test results, validation outcomes |
| `problems.md` | Unresolved issues, technical debt |

**Lifecycle**: created lazily on first write during Atlas execution.
Appended to (never rewritten) as tasks complete.

## Display Entities (TUI/CLI)

### DisplayAgent

For the Tab picker and activity panel roster.

| Field | Type | Description |
|-------|------|-------------|
| `name` | `String` | Canonical name |
| `display_name` | `String` | Human label |
| `color` | `String` | Hex color |
| `mode` | `AgentMode` | Primary or Subagent |
| `resolved_model` | `Option<String>` | Model after fallback resolution (None = skipped) |
| `description` | `String` | Short description |

### ActiveSubagentEntry

For the activity panel when subagents are running.

| Field | Type | Description |
|-------|------|-------------|
| `id` | `usize` | Unique entry ID |
| `agent_type` | `String` | "explore", "librarian", "oracle", "sisyphus-junior", etc. |
| `category` | `Option<String>` | If category-spawned (e.g., "quick") |
| `status` | `SubagentStatus` | Running, Done, Failed |
| `phase` | `String` | "querying model", "running tool: X", "reasoning" |
| `model` | `String` | Resolved model |
| `iterations` | `usize` | API calls made |
| `started` | `Instant` | For elapsed time |

### SubagentStatus

```rust
pub enum SubagentStatus {
    Running,
    Done,
    Failed,
}
```

## Entity Relationships

```
AgentRegistry
 ├── OmoAgent (×11: 4 primary, 7 subagent)
 │    └── ModelRequirement → FallbackEntry (×N per agent)
 └── CategoryConfig (×11)
      └── ModelRequirement → FallbackEntry (×N per category)

BoulderState
 └── BoulderWork (×N active)
      └── NotepadStore (1 per plan)
           ├── learnings.md
           ├── decisions.md
           ├── issues.md
           ├── verification.md
           └── problems.md

GoalState (1 per session)

DisplayAgent ← derived from OmoAgent + model resolution
ActiveSubagentEntry ← derived from AgentEvent stream
```

## Validation Rules

- **VR-001**: Exactly 11 agents registered; names are the canonical set.
- **VR-002**: Primary agents appear in Tab picker only if model resolved
  (resolved_model is Some).
- **VR-003**: Category and subagent_type are mutually exclusive in a single
  delegation (validated at dispatch time).
- **VR-004**: Boulder state file is valid JSON; missing file = empty state
  (not an error).
- **VR-005**: Notepad files are appended-only during plan execution.
- **VR-006**: Goal status transitions: Active ↔ Paused; Cleared = removed.
- **VR-007**: Model family detection is deterministic: exact prefix match
  → family → Unknown.
