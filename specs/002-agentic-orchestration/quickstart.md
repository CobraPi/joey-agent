# Quickstart: Agentic Orchestration Engine

**Feature**: 002-agentic-orchestration
**Date**: 2026-07-23

## Prerequisites

- Rust stable 1.80+
- The joey-agent workspace builds clean: `cargo build --workspace`
- All existing tests pass: `cargo test --workspace`

## Validation Scenarios

### Increment 1: Delegation Core (P1-P3)

#### Scenario 1: Single subagent delegation

**Setup**: Ensure a provider key is configured (`joey config set OPENROUTER_API_KEY ...`)

**Run**:

```bash
# Build the workspace with the new crate
cargo build -p joey-orchestration

# Run orchestration crate tests (mocked provider — no API key needed)
cargo test -p joey-orchestration

# Verify the delegate_task tool is registered
cargo run -- joey tools --list | grep delegate_task
```

**Expected**:
- `cargo build -p joey-orchestration` succeeds with zero warnings
- All unit tests pass: subagent isolation, concurrency limiter, summary generation
- `delegate_task` appears in the tool list

#### Scenario 2: Parallel batch delegation

**Run**:

```bash
# Integration test: 3 parallel subagents with mocked Transport
cargo test -p joey-orchestration --test parallel_batch

# Verify batch resilience (one failure doesn't abort others)
cargo test -p joey-orchestration --test batch_resilience
```

**Expected**:
- 3 subagents dispatched concurrently via JoinSet
- Wall-clock time < 1.5x slowest single subtask (asserted via timing)
- When one child fails, the other two results are still returned

#### Scenario 3: Per-subagent model selection

**Run**:

```bash
cargo test -p joey-orchestration --test model_selection
```

**Expected**:
- Mixed-model batch: 1 subagent uses heavy model, 2 use light model
- Each subagent's model assignment is recorded in DelegationResult
- Total token usage is lower than all-heavy baseline

#### Scenario 4: Concurrency limiter

**Run**:

```bash
cargo test -p joey-orchestration --test concurrency_limiter
```

**Expected**:
- Semaphore caps in-flight provider requests at `max_concurrent_requests`
- Excess tasks queue (not reject)
- No retry storms under simulated 429 conditions

#### Scenario 5: Orchestration events

**Run**:

```bash
cargo test -p joey-orchestration --test events
```

**Expected**:
- SubagentSpawn, SubagentComplete/SubagentFailed, DelegationBatchComplete
  events emitted via AgentEvent channel
- Events contain goal, model, token_usage, duration_secs

#### Scenario 6: Full workspace regression

**Run**:

```bash
cargo build --workspace
cargo test --workspace
```

**Expected**: Entire workspace builds and all tests pass — no regressions
from the additive orchestration layer.

---

### Increment 2: Search, Clarify, Processes (P4-P6)

#### Scenario 7: Session history search

**Setup**: Create a session with known content:

```bash
echo "Remember: this project uses cargo test with --workspace flag" | joey
```

**Run**:

```bash
# Start a new session and search for the prior decision
joey
> What test command did we decide on? Search your history.
```

**Expected**:
- Agent calls session_search with relevant query
- Prior session found via FTS5, snippet returned
- Agent reports the decision without re-asking

#### Scenario 8: Background process lifecycle

**Run**:

```bash
cargo test -p joey-tools --test background_process
```

**Expected**:
- `terminal(background=true)` returns a session_id immediately (<100ms)
- `process(action="poll")` returns new stdout output
- `process(action="kill")` terminates the process and cleans up

#### Scenario 9: Clarify in interactive mode

**Run**:

```bash
joey
> Add authentication to the API
```

**Expected**:
- Agent detects ambiguity, calls clarify with choices
- User picks an option, agent incorporates and continues

#### Scenario 10: Full workspace regression (both increments)

**Run**:

```bash
cargo build --workspace
cargo test --workspace
```

**Expected**: Entire workspace green.

## Performance Validation

After both increments are complete, validate the performance criteria:

```bash
# Run the orchestration benchmark (if implemented as a test)
cargo test -p joey-orchestration --test benchmark -- --nocapture
```

Verify against SC targets:
- SC-001: Parallel delegation wall-clock < 1.5x slowest single subtask
- SC-002: Parent context unaffected by subagent intermediate results
- SC-005: Session search < 1s for 100+ sessions
- SC-006: Background process ops < 100ms non-blocking
- SC-009: All lifecycle events carry required fields
