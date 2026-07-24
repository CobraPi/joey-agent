# Quickstart: Oh My OpenAgent Orchestration

**Feature**: 003-omo-orchestration | **Date**: 2026-07-23

## Prerequisites

- joey-agent built from source (this repo)
- At least one LLM provider configured (run `joey setup` or edit `config.yaml`)
- Rust 1.80+ stable toolchain

## Build

```bash
# Build the entire workspace (including the new joey-omo crate)
cargo build --workspace

# Verify no regressions
cargo test --workspace
```

## Validation Scenario 1: Agent Registry and Tab Switching

**Proves**: 11 agents registered, Tab cycles through primary agents.

```bash
# Launch the TUI
joey

# Press Tab — an agent picker overlay appears:
#   ► Default
#     Sisyphus      [claude-opus-4.8]
#     Hephaestus    [gpt-5.6-sol]
#     Prometheus    [claude-opus-4.8]
#     Atlas         [claude-sonnet-5]
#
# Use arrow keys to highlight Prometheus, press Enter.
# The status bar updates to show "Prometheus" in its color.
# Type a message — the agent responds as a read-only planner.

# Press Tab again to cycle to the next agent.
# Press Shift+Tab to cycle backwards.
```

**Expected**: Status bar shows the active agent name and color. Each agent
produces a distinct behavioral profile (Sisyphus delegates; Prometheus plans;
Atlas coordinates).

**CLI parity**:
```bash
joey --no-tui
# Type /agent or press Tab — numbered list prints:
#   1. Default
#   2. Sisyphus
#   3. Hephaestus
#   4. Prometheus
#   5. Atlas
# Type a number to select.
```

## Validation Scenario 2: Activity Panel

**Proves**: Bottom-right panel shows live subagent activity.

```bash
joey
# Select Sisyphus (Tab → Sisyphus → Enter)
# The bottom-right panel shows:
#   ► Sisyphus [claude-opus-4.8]
#     0/5 slots ◌ idle
#   (full 11-agent roster below)

# Ask Sisyphus to explore the codebase:
# "explore the crates/ directory structure"
# Sisyphus fires explore subagents in parallel.
# The panel updates live:
#   ► Sisyphus [claude-opus-4.8]
#     3/5 slots ◷ active
#   ◷ explore       running    2s  querying model
#   ◷ explore       running    2s  querying model
#   ◷ librarian     running    1s  querying model
```

**Expected**: Panel shows pinned Sisyphus at top, slot count, and live
subagent entries with spinners. As subagents complete, entries flip to "done".

**CLI parity**: Inline summaries print as events arrive:
```
[explore] spawned → running (model: glm-5)
[explore] done (4.2s)
```

## Validation Scenario 3: Model Fallback Resolution

**Proves**: Fallback chains resolve correctly with family-level fuzzy matching.

```bash
# Configure only a Z.ai/GLM provider in config.yaml:
#   model:
#     provider: zai
#     model: glm-4.6

joey
# The Tab picker shows agents whose chains include GLM:
#   ► Default
#     Sisyphus      [glm-5]        ← resolved via GLM family match
#     Prometheus    [glm-5.2]      ← resolved via GLM family match
#     (Hephaestus hidden — requires OpenAI, which is not configured)
#     Atlas         (unavailable)  ← no chain entry resolves
```

**Expected**: Agents with GLM in their chain activate with the GLM prompt
variant. Hephaestus is hidden (requires OpenAI-class provider). The GLM-
specific prompt variant is used for Sisyphus and Prometheus.

## Validation Scenario 4: Ultrawork Mode

**Proves**: Keyword detection activates ultrawork.

```bash
joey
# Select Sisyphus (Tab → Sisyphus → Enter)
# Type: ulw implement a simple echo CLI in Rust
#
# Expected: Agent responds "ULTRAWORK MODE ENABLED!" first, then:
#   - Fires explore agents to research patterns
#   - Creates a plan via the plan flow
#   - Delegates implementation to Sisyphus-Junior
#   - Verifies the result

# Test on Prometheus (should be ignored):
# Switch to Prometheus (Tab → Prometheus → Enter)
# Type: ulw implement something
# Expected: Prometheus ignores ultrawork (read-only planner), proceeds normally
```

## Validation Scenario 5: Plan → Execute Pipeline

**Proves**: Full orchestration pipeline end-to-end.

```bash
joey
# Step 1: Plan
# Switch to Prometheus (Tab → Prometheus → Enter)
# "I want to add a hello-world subcommand to joey-cli"
# Prometheus interviews, creates plan in .omo/plans/hello-world-subcommand.md

# Step 2: Execute
# Type: /start-work hello-world-subcommand
# Atlas activates, reads the plan, delegates tasks to Sisyphus-Junior.
# The activity panel shows:
#   ► Atlas [claude-sonnet-5]
#     2/5 slots ◷ active
#   ┌─ jobs ──────────────────────┐
#   │ ► Task 1: Implement cmd     │
#   │   running, 3 tool calls     │
#   │   Task 2: Write tests       │
#   │   pending                   │
#   └─────────────────────────────┘
#   ◷ junior [quick] running  5s
#   📝 1 learning accumulated

# Verify artifacts:
ls .omo/plans/         # hello-world-subcommand.md exists
ls .omo/notepads/      # hello-world-subcommand/ with learnings.md etc.
cat .omo/boulder.json  # shows completed work
```

## Validation Scenario 6: Goal Persistence

**Proves**: /goal command and continuation injection.

```bash
joey
# Type: /goal set Ship the dashboard feature
# Expected: "Goal set: Ship the dashboard feature"
# On subsequent idle turns, the goal is re-injected.

# Type: /goal pause
# Expected: continuation injection stops.

# Type: /goal resume
# Expected: injection resumes.

# Type: /goal clear
# Expected: goal removed.
```

## Validation Scenario 7: Category Delegation

**Proves**: Category-based model routing.

```bash
joey
# Select Sisyphus. Ask it to delegate a quick task:
# "delegate a quick task to check the Cargo.toml version"
# Sisyphus delegates with category="quick".
# The activity panel shows:
#   ◷ junior [quick] running  2s  [gpt-5.4-mini]
# (quick category routes to a fast/cheap model)
```

## Narrow Terminal Degradation

```bash
# Resize terminal to 70 columns × 20 rows
joey
# The activity panel hides entirely (width < 72).
# The transcript uses the full width.

# Resize to 80 columns × 12 rows
# Panel shows but roster truncates; only 3 subagent entries visible.
```

## Running Tests

```bash
# All OMO tests
cargo test -p joey-omo

# Specific contract tests
cargo test -p joey-omo -- agent_registry
cargo test -p joey-omo -- model_fallback
cargo test -p joey-omo -- category_delegation
cargo test -p joey-omo -- boulder_state
cargo test -p joey-omo -- tab_picker

# TUI smoke test (includes new panel)
cargo test -p joey-tui

# Full workspace regression
cargo test --workspace
```

## Performance Check

```bash
# Time the agent registry build (should be <50ms)
cargo test -p joey-omo -- registry_build_perf -- --nocapture

# Verify parallel delegation performance
cargo test -p joey-omo -- parallel_delegation_perf -- --nocapture
# Should show wall-clock ≈ slowest single subagent, not the sum
```
