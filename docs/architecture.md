# Architecture Overview

Joey Agent is a Cargo workspace of thirteen crates. The first eight are a
direct, well-scoped port of the upstream Python project (Hermes Agent), one
module per crate. The remaining five — `joey-tui`, `joey-llm-selector`,
`joey-orchestration`, `joey-omo`, and `joey-speckit-ui` — are joey-native
additions layered on top of the ported core; they have no upstream Python
equivalent. The dependency graph is a strict DAG — lower crates never
depend on higher ones:

```
                      ┌──────────────┐
                      │  joey-cli    │  the `joey` binary: clap + REPL + TUI
                      └──────┬───────┘
                             │ depends on
   ┌───────────┬─────────────┼──────────────┬────────────┬─────────────┐
   ▼           ▼             ▼              ▼            ▼             ▼
┌────────┐┌─────────┐┌──────────────┐┌───────────┐┌─────────────┐┌──────────┐
│joey-   ││joey-cron││joey-mcp      ││joey-      ││joey-tui     ││joey-omo  │
│agent-  ││         ││              ││gateway    ││             ││          │
│core    ││         ││              ││           ││             ││          │
└───┬────┘└────┬────┘└──────┬───────┘└─────┬─────┘└──────┬──────┘└────┬─────┘
    │          │            │              │             │            │
    │          │            │              │             │      ┌─────┴──────┐
    │          │            │              │             │      │joey-       │
    │          │            │              │             │      │orchestration│
    │          │            │              │             │      └─────┬──────┘
    │          │            │              │             │            │
    └──────────┴─────┬──────┴──────────────┴─────────────┴────────────┘
                      ▼
           ┌─────────────────────┐
           │  joey-tools         │  Tool trait, registry, toolsets
           │  joey-providers     │  LLM wire protocols
           │  joey-llm-selector  │  dynamic model allocator (ModelAllocator trait)
           └──────────┬──────────┘
                      ▼
             ┌─────────────────┐
             │   joey-core     │  config, state DB, branding, logging
             └─────────────────┘

joey-speckit-ui is a standalone binary (not a joey-cli dependency); it
serves the SpecKit Visual UI's REST/WebSocket API independently and reads
the same specs/<feature>/*.md files joey-cli's speckit skills operate on.
```

## Crate responsibilities

### Ported from Hermes Agent

| Crate | Ports (upstream Python) | Responsibility |
|---|---|---|
| `joey-core` | `hermes_constants.py`, `hermes_state.py`, `config.py`, logging, redaction | Path/profile resolution (`~/.joey`), layered YAML+env config, the SQLite session store, secret redaction, reasoning-effort parsing, ANSI theme, time helpers |
| `joey-providers` | `providers/`, `agent/transports/` | Provider profile registry, OpenAI Chat Completions / OpenAI Responses / Anthropic Messages wire adapters, SSE streaming, error classification and backoff |
| `joey-tools` | `tools/`, `toolsets.py` | The `Tool` trait and dispatch registry, toolset resolution, JSON-schema sanitizer, fuzzy patch matcher, built-in tools (files, terminal, process, memory, todo, skills, web, session search, clarify, LSP) |
| `joey-agent-core` | `run_agent.py`, `agent/conversation_loop.py`, `agent/prompt_builder.py`, `agent/context_compressor.py` | The turn loop itself: message assembly, system prompt construction, tool-call validation/dispatch, retries/fallback, context compression, threat scanning |
| `joey-cron` | `cron/` | Self-contained scheduler: job store (`~/.joey/cron/jobs.json`), croniter-compatible expression matcher, 60s ticker, job runner |
| `joey-mcp` | `tools/mcp_tool.py` (client half) | Model Context Protocol stdio JSON-RPC client: handshake, tool discovery/naming, pagination, timeouts, safe-env subprocess spawning |
| `joey-gateway` | `gateway/` (core) | Platform-neutral session-key builder, `MessageEvent`/`SendResult` types, send-error classification, the `PlatformAdapter` trait (concrete adapters like Telegram/Discord are added behind this trait) |
| `joey-cli` | `hermes_cli/`, `cli.py` | The `joey` binary: clap argument tree, profile resolution, interactive REPL (reedline-based) or animated TUI (`--tui`), one-shot mode, slash commands, all subcommands (`model`, `config`, `doctor`, `cron`, `mcp`, `skills`, `tools`, `auth`, `discover`, `llm-selector`) |

### Joey-native additions (no upstream equivalent)

| Crate | Responsibility |
|---|---|
| `joey-tui` | The `--tui` animated dashboard: `Tui` runtime, `Theme`/gradient rendering, an `App`/`AppState` model, widgets (including the agent-roster panel populated from `joey-omo`), and an input editor. Rendering/event-pump lifecycle is driven by `joey-cli`. |
| `joey-llm-selector` | Dynamic per-module model allocation used when `model.default = auto`: a `CandidateModelPool`, a persisted `AllocationMap` (`~/.joey/…/allocations.json`), a cold-start `ColdStartScorer`, a diagnoser/learning loop, and the `ModelAllocator` trait consumed by `joey-orchestration` and the parent `Agent` so each call-site can ask "which model for module X". Exposed via the `/llm-selector` slash command and `joey llm-selector` top-level command. |
| `joey-orchestration` | The subagent/multi-agent delegation engine: `SubagentManager`, the `delegate_task` and `call_omo_agent` tools, per-subagent isolated execution contexts, shared concurrency limits, and `register_orchestration*` helpers that install the delegation tool into a `ToolRegistry`. Depends on `joey-llm-selector::ModelAllocator` and a `CategoryResolver` trait (implemented by `joey-cli` to bridge into `joey-omo` without a circular dependency). |
| `joey-omo` | "Oh My OpenAgent": an 11-agent persona registry (`AgentRegistry`, `OmoAgent`) with per-agent model-family fallback chains (`AvailableModelSet`, `resolve_model`), category/subagent delegation routing (`resolve_category`, `route_delegation`), plan parsing and Atlas-style plan execution (`prepare_plan_execution`, `start_work`), intent gating for ultrawork/hyperplan/team triggers (`detect_keyword`, `check_ultrawork_activation`), per-session `GoalState`, an accumulated-wisdom notepad, and team-mode primitives (`TeamSpec`, `TeamMailbox`, `TeamTaskList`, optional tmux visualizer). Wired into both the REPL and TUI paths of `joey-cli` (agent roster, `/agents`, `/start-work`, `/goal`, intent gating). |
| `joey-speckit-ui` | A standalone HTTP + WebSocket backend (own binary, `cargo run -p joey-speckit-ui`) serving the SpecKit Visual UI: parses `specs/<feature>/{spec,plan,tasks}.md` into a typed model, provides conflict-checked (hash-based optimistic-locking) writes, and streams file-watch/clarify/run events over WebSocket to the `web/speckit-ui` frontend. Not linked into the `joey` binary. |

## End-to-end data flow (one turn)

1. **Entry point** (`joey-cli`): the user types a message in the REPL or
   the `--tui` dashboard, or supplies `-z "<prompt>"` for one-shot mode, or
   a cron job fires a scheduled prompt.
2. **Agent construction** (`joey-agent-core::Agent::new`): builds a
   `ProviderClient` from the resolved provider profile, snapshots the
   valid/checked tool names, assembles the session-stable system prompt
   (see [agent-core-reference.md](agent-core-reference.md)), and wires up the context
   compressor. `joey-cli` additionally registers the `delegate_task` tool
   (`joey-orchestration`) — bridged to `joey-omo`'s agent registry via a
   `CategoryResolver` — and, when `model.default = auto`, a
   `joey-llm-selector::ModelAllocator`.
3. **Turn execution** (`Agent::run_turn` in `agent.rs`): the user message
   is appended to history and persisted; the loop then:
   - Assembles the full message list (system prompt + history).
   - Calls the provider (`ProviderClient::complete`/`stream`), applying
     jittered backoff across `api_max_retries` attempts and walking the
     `fallback_providers` chain on hard failures.
   - If the assistant requested tool calls, validates/repairs them,
     dispatches read-only ("parallel-safe") tools concurrently and
     everything else sequentially with `tool_delay` spacing between calls,
     wraps untrusted tool output, and loops. A `delegate_task` call spins
     up a `joey-orchestration::SubagentManager` execution with its own
     isolated context and (optionally) an allocator-selected model.
   - Otherwise, the turn is done.
   - On context overflow (413 / provider length errors) or when usage
     crosses the compression threshold, the context compressor prunes/
     summarizes history and the request is retried.
   - On iteration-budget exhaustion, tools are stripped and the model is
     asked for a final summary.
4. **Events**: throughout, `AgentEvent`s stream out over an mpsc channel
   to whatever is driving the agent (line-REPL renderer, `joey-tui`
   dashboard, or gateway adapter), carrying content/reasoning deltas, tool
   start/end, retries, compression notices, and the final `Done`/`Failed`.
5. **Persistence**: every durable message (user, assistant, tool result)
   is written to `~/.joey/state.db` as it's produced; ephemeral recovery
   scaffolding (e.g. synthetic repair messages) is tracked separately and
   never persisted.

## Design principles visible in the code

- **Byte-for-byte prompt fidelity.** Guidance strings shown to the model
  (`guidance.rs`) are ported verbatim from upstream, because subtle
  wording changes measurably affect model behavior. Comments explicitly
  flag anywhere behavior *had* to diverge (branding, missing OAuth
  impersonation, etc).
- **Explicit registration over reflection.** Where upstream Python
  discovers tools via import side effects, the Rust port registers every
  built-in tool explicitly in `joey_tools::builtins::register_all`,
  trading a little verbosity for compile-time guarantees.
- **The system prompt is built once per session** and never re-rendered,
  specifically to keep provider prompt-prefix caches warm — this is a
  hard constraint baked into `Agent::new`/`build_system_prompt`.
- **Defense in depth on untrusted content.** Context files, tool results
  from `web_search`/`web_extract`/`browser_*`/`mcp_*`, and MCP server
  configs each pass through their own sanitization/threat-scan layer
  before ever reaching the model or the shell.
- **Hermes-compatible on-disk formats.** The SQLite schema
  (`SCHEMA_VERSION = 22`), cron `jobs.json` envelope, and skill format are
  intentionally identical to upstream so a `~/.hermes` home can be renamed
  to `~/.joey` and just work.
