# Contract: Agent Activity Panel

**Feature**: 003-omo-orchestration

## Layout (Bottom-Right TUI Region)

The panel replaces the existing `draw_activity` sidebar widget. It uses a
split layout within the right-side body region (Length 34, when width >= 72):

```
┌─ agents ────────────────────┐
│ ► Sisyphus [claude-opus-4.8]│  ← Pinned: active primary agent (always visible)
│   (0/5 slots) ◌ idle        │  ← Concurrency indicator + status
├─────────────────────────────┤
│                             │  ← Below: roster (idle) OR subagent activity (active)
│  (idle: full roster)        │
│  (active: subagent entries) │
│                             │
└─────────────────────────────┘
```

## Idle State (no subagents running)

Below the pinned active agent, the full 11-agent roster expands:

```
│   Sisyphus      Primary  claude-opus-4.8 │
│   Hephaestus    Primary  gpt-5.6-sol     │
│   Prometheus    Primary  claude-opus-4.8 │
│   Atlas         Primary  claude-sonnet-5 │
│   Oracle        Sub      gpt-5.6-sol     │
│   Librarian     Sub      gpt-5.4-mini    │
│   Explore       Sub      gpt-5.4-mini    │
│   ... (all 11)                            │
```

Each row: display_name, mode, resolved_model. Skipped agents shown dimmed
with "(unavailable)".

## Active State (subagents running)

The roster condenses/collapses. Live subagent entries fill the space:

```
│  ◷ explore       running    3s   │
│    querying model                │
│  ◷ librarian     running    5s   │
│    running tool: grep            │
│  ✓ sisyphus-jr   done       12s  │  ← completed (green)
│  ✗ oracle        failed     8s   │  ← failed (red)
```

Each entry: spinner glyph (◷ running, ✓ done, ✗ failed), agent_type,
status, elapsed time, current phase.

Category-spawned subagents show their category:

```
│  ◷ junior [quick] running   3s   │
```

## Job Board (during Atlas execution)

When Atlas is executing a plan, a job board section appears:

```
│  ┌─ jobs ──────────────────────┐ │
│  │ ► Task 1: Implement auth    │ │
│  │   running, 3 tool calls     │ │
│  │   Task 2: Write tests       │ │
│  │   pending                   │ │
│  │   Task 3: Update docs       │ │
│  │   pending                   │ │
│  └─────────────────────────────┘ │
```

## Concurrency Indicator

Always visible below the pinned agent:

- `0/5 slots ◌ idle` — no subagents running
- `3/5 slots ◷ active` — 3 of 5 parallel slots in use
- `5/5 slots ⏸ queued` — at limit, tasks queuing

## Wisdom Counter

During plan execution:

```
│  📝 5 learnings accumulated      │
```

## Event Stream Mapping

| AgentEvent | Panel Update |
|------------|-------------|
| SubagentSpawn | Add entry with "running" status |
| SubagentComplete | Entry → "done" (green), stop spinner |
| SubagentFailed | Entry → "failed" (red) |
| AgentModeChanged | Update pinned active agent |
| CategoryDelegation | Add entry with category label |
| IterationStart | Update iteration count on entry |
| ApiCallStart | Entry phase → "querying model" |
| ToolStart | Entry phase → "running tool: X" |
| BoulderWorkStarted | Show job board |
| WisdomAccumulated | Update learnings counter |

## Graceful Degradation

- **Narrow terminal (<72 cols)**: Panel hides entirely (existing behavior).
- **Short height (<15 rows)**: Roster truncates to top 5 agents; subagent
  entries limited to 3 most recent.
- **Very short (<9 rows)**: Panel collapses to single line: active agent +
  slot count.

## CLI Parity

The CLI prints compact inline summaries as events arrive:

```text
[explore] spawned → running (model: glm-5)
[librarian] spawned → running (model: gpt-5.4-mini)
[explore] done (4.2s, 1500 tokens)
[oracle] spawned → running (model: gpt-5.6-sol)
[junior:quick] spawned → running (model: gpt-5.4-mini)
```

These use the existing `render.rs` infrastructure with ANSI colors matching
the TUI agent colors.
