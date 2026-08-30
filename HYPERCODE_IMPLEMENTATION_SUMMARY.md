# HyperCode — Native delegate_task Integration

`/hypercode run <goal>` executes a **plan → explore → build → final
gate** pipeline of parallel subagents through the same
`SubagentManager` machinery that the `delegate_task` tool uses. The
workflow is execution-only: the orchestrator does ALL thinking,
planning, and inference, and dispatches fully-specified briefs (exact
file paths, exact edits, exact commands, expected outcomes) — Explorer
subagents answer factual questions only, and Implementor subagents
execute their brief verbatim. Subagents integrate natively in the TUI:

- Every child (planner / explorer / implementor) appears as a **live pane
  on the right rail** (click / Ctrl+P to focus, full transcript streaming)
- The **Atlas job board** auto-opens during a run (status, iterations,
  last tool per agent)
- The **⚡ badge** shows the live phase (`⚡ PLAN` / `⚡ EXPL` / `⚡ BUILD` /
  `⚡ SYNTH`) while a run executes
- Phase transitions stream into the main transcript as Busy notices

## Pipeline

1. **Plan** — one Planner subagent (read-only `file` toolset) decomposes
   the goal into 1..N disjoint workstreams inside a machine-parsed
   `<workstreams>` block. Skip by passing `--stream "a;b;c"`.
2. **Explore (facts only)** — N parallel Explorer subagents (read-only)
   answer factual questions only (paths, line numbers, quotes, command
   output — no analysis or recommendations). The orchestrator composes
   each workstream's fully-specified brief (exact file paths, exact
   edits, exact commands, expected outcomes) from those facts.
3. **Build (execution-only)** — N parallel Implementor subagents
   execute their brief verbatim; if a brief is ambiguous or incomplete
   they stop and report what's missing rather than guessing. Instructed
   to stay within their stream's files (siblings run concurrently).
   Implementors verify with targeted checks for what they touched only
   (e.g. `cargo build -p <crate>`, `cargo test -p <crate> [filter]`) —
   never the full test suite.
4. **Final gate** — after ALL implementors finish, the orchestrator
   runs the project's full test suite exactly once (e.g. `cargo test
   --workspace`). On failures it dispatches one final fix round
   (implementors verify fixes with targeted checks), optionally
   confirming once more with the full suite.
5. **Synthesize** — per-stream reports merged into a final summary
   (in-memory; no extra LLM call).

## Execution model

- Runs on the **engine actor** (`EngineCommand::Hypercode`) — the GUI
  never blocks; 1st Ctrl-C cooperatively interrupts children, 2nd
  force-kills the engine as usual.
- Children share the engine's provider-request **semaphore** (via the
  manager), so hypercode + delegate_task runs never oversubscribe.
- Every child's events flow through the **process-global orchestration
  tap** (`SubagentSpawn` / `SubagentEvent` / `SubagentComplete`) — the
  exact same stream the TUI already renders for delegate_task batches.

## Configuration

`/hypercode configure <explorer|implementor> <provider> [model]`
`[--reasoning none|low|medium|high] [--tokens N] [--turns N]`

Empty model/reasoning **inherit** the live agent's settings (delegation
defaults don't shadow them). `hypercode.max_workstreams` (config, 0 =
default 5) caps streams per phase; `--max N` overrides per run.

## Line REPL

`/hypercode run` also works in the line REPL (`joey --cli`), printing
phase progress + the final report to stdout.

## Status of older material

The older files `HYPERCODE_INTEGRATION_VERIFICATION.md` and
`test_hypercode_integration.md` describe the pre-integration stub where
`/hypercode` was only a config/badge toggle. The badge-only behavior
(`/hypercode toggle`) still exists as a model hint, but the feature now
actually executes.
