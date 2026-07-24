# Contract: Slash Commands

**Feature**: 003-omo-orchestration

## New Commands (registered in joey-cli/src/slash.rs)

### /start-work

```
/start-work [plan-name]
```

Activates Atlas on a plan.

- Reads `.omo/plans/{plan-name}.md` (or auto-selects/resumes from boulder state).
- If no plan name given:
  - 0 active works → error: "No plan found. Use Prometheus to create one."
  - 1 active work → auto-resume that work.
  - Multiple active works → ask user to choose (via clarify tool).
- Atlas becomes the active agent and begins executing the plan's tasks.
- Creates/updates `.omo/boulder.json` boulder state.
- Emits `BoulderWorkStarted` or `BoulderWorkResumed` event.

**Registration**: Category "Session", args_hint "[plan-name]", implemented true.

### @plan

```
@plan "description of work"
```

From any primary agent, delegates to Prometheus to create a plan.

- Equivalent to switching to Prometheus and describing work.
- Does NOT start execution — only creates the plan in `.omo/plans/`.
- After the plan is created, the user runs `/start-work` to execute.

**Implementation**: Parsed as a special prefix in the REPL input (not a
slash command per se, but a delegation trigger).

### /goal

```
/goal [set <text> | pause | resume | clear | show]
```

Manages a persistent per-session objective.

- `/goal` or `/goal show` → display current goal and status.
- `/goal set <text>` → set active goal; re-injected as continuation prompt
  on idle turns until cleared or completed.
- `/goal pause` → goal becomes Paused; no continuation injection.
- `/goal resume` → goal becomes Active; injection resumes.
- `/goal clear` → goal removed entirely.

**Registration**: Category "Session". The existing `/goal` entry in slash.rs
has `implemented: false` — this feature flips it to `true`.

### ultrawork / ulw

```
ultrawork [prompt]
ulw [prompt]
```

Not a slash command — a keyword detected in any user message. When detected,
the ultrawork instruction set is injected into the active agent's prompt.
The agent MUST respond "ULTRAWORK MODE ENABLED!" first.

Valid on: Default, Sisyphus, Hephaestus, Atlas. Ignored on Prometheus.

### hyperplan

```
hyperplan [prompt]
```

Keyword detection. Activates the adversarial hyperplan workflow (loads the
hyperplan skill). Combo: `hyperplan ultrawork` activates both.

## Integration Points

- **slash.rs**: New entries for `/start-work`; flip `/goal` to implemented.
- **repl.rs**: Handler for `/start-work` (activates Atlas + boulder state),
  `/goal` subcommands, `@plan` prefix detection, keyword detection for
  ultrawork/hyperplan.
- **joey-omo**: `BoulderState`, `GoalState`, `IntentGate` modules provide
  the backing logic.

## Behavioral Contracts

- **BC-020**: `/start-work` with a non-existent plan name → error message,
  no state change.
- **BC-021**: `/start-work` with no name and 1 active work → auto-resume
  without asking.
- **BC-022**: `/goal set` creates an Active goal; subsequent idle turns
  inject the goal as a continuation prompt.
- **BC-023**: `/goal pause` stops injection; `/goal resume` restarts it.
- **BC-024**: `ultrawork`/`ulw` keyword detection happens before prompt
  building; the instruction set is prepended to the system prompt.
- **BC-025**: Ultrawork on Prometheus is silently ignored (not injected).
