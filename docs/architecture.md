# Architecture Overview

Joey Agent is a Cargo workspace of eight crates. Each crate has a single,
well-scoped responsibility and maps to one or more modules of the upstream
Python project (Hermes Agent). The dependency graph is a strict DAG —
lower crates never depend on higher ones:

```
                      ┌──────────────┐
                      │  joey-cli    │  the `joey` binary: clap + REPL
                      └──────┬───────┘
                             │ depends on
        ┌────────────┬───────┼────────────┬─────────────┐
        ▼            ▼       ▼            ▼             ▼
 ┌─────────────┐┌─────────┐┌──────────┐┌─────────┐┌─────────────┐
 │joey-agent-  ││joey-cron││joey-mcp  ││joey-     ││(joey-tools, │
 │core         ││         ││          ││gateway   ││ joey-       │
 │(turn loop)  ││         ││          ││          ││ providers)  │
 └──────┬──────┘└────┬────┘└────┬─────┘└────┬─────┘└──────┬──────┘
        │            │          │           │             │
        └────────────┴─────┬────┴───────────┴─────────────┘
                            ▼
                 ┌─────────────────────┐
                 │  joey-tools         │  Tool trait, registry, toolsets
                 │  joey-providers     │  LLM wire protocols
                 └──────────┬──────────┘
                            ▼
                   ┌─────────────────┐
                   │   joey-core     │  config, state DB, branding, logging
                   └─────────────────┘
```

## Crate responsibilities

| Crate | Ports (upstream Python) | Responsibility |
|---|---|---|
| `joey-core` | `hermes_constants.py`, `hermes_state.py`, `config.py`, logging, redaction | Path/profile resolution (`~/.joey`), layered YAML+env config, the SQLite session store, secret redaction, reasoning-effort parsing, ANSI theme, time helpers |
| `joey-providers` | `providers/`, `agent/transports/` | Provider profile registry, OpenAI Chat Completions / OpenAI Responses / Anthropic Messages wire adapters, SSE streaming, error classification and backoff |
| `joey-tools` | `tools/`, `toolsets.py` | The `Tool` trait and dispatch registry, toolset resolution, JSON-schema sanitizer, fuzzy patch matcher, built-in tools (files, terminal, memory, todo, skills, web) |
| `joey-agent-core` | `run_agent.py`, `agent/conversation_loop.py`, `agent/prompt_builder.py`, `agent/context_compressor.py` | The turn loop itself: message assembly, system prompt construction, tool-call validation/dispatch, retries/fallback, context compression, threat scanning |
| `joey-cron` | `cron/` | Self-contained scheduler: job store (`~/.joey/cron/jobs.json`), croniter-compatible expression matcher, 60s ticker, job runner |
| `joey-mcp` | `tools/mcp_tool.py` (client half) | Model Context Protocol stdio JSON-RPC client: handshake, tool discovery/naming, pagination, timeouts, safe-env subprocess spawning |
| `joey-gateway` | `gateway/` (core) | Platform-neutral session-key builder, `MessageEvent`/`SendResult` types, send-error classification, the `PlatformAdapter` trait (concrete adapters like Telegram/Discord are added behind this trait) |
| `joey-cli` | `hermes_cli/`, `cli.py` | The `joey` binary: clap argument tree, profile resolution, interactive REPL (reedline-based), one-shot mode, slash commands, all subcommands (`model`, `config`, `doctor`, `cron`, `mcp`, `skills`, `tools`, `auth`) |

## End-to-end data flow (one turn)

1. **Entry point** (`joey-cli`): the user types a message in the REPL, or
   supplies `-z "<prompt>"` for one-shot mode, or a cron job fires a
   scheduled prompt.
2. **Agent construction** (`joey-agent-core::Agent::new`): builds a
   `ProviderClient` from the resolved provider profile, snapshots the
   valid/checked tool names, assembles the session-stable system prompt
   (see [system-prompt.md](system-prompt.md)), and wires up the context
   compressor.
3. **Turn execution** (`Agent::run_turn` in `agent.rs`): the user message
   is appended to history and persisted; the loop then:
   - Assembles the full message list (system prompt + history).
   - Calls the provider (`ProviderClient::complete`/`stream`), applying
     jittered backoff across `api_max_retries` attempts and walking the
     `fallback_providers` chain on hard failures.
   - If the assistant requested tool calls, validates/repairs them,
     dispatches read-only ("parallel-safe") tools concurrently and
     everything else sequentially with `tool_delay` spacing between calls,
     wraps untrusted tool output, and loops.
   - Otherwise, the turn is done.
   - On context overflow (413 / provider length errors) or when usage
     crosses the compression threshold, the context compressor prunes/
     summarizes history and the request is retried.
   - On iteration-budget exhaustion, tools are stripped and the model is
     asked for a final summary.
4. **Events**: throughout, `AgentEvent`s stream out over an mpsc channel
   to whatever is driving the agent (CLI renderer, gateway adapter),
   carrying content/reasoning deltas, tool start/end, retries,
   compression notices, and the final `Done`/`Failed`.
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
