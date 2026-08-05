# Joey Agent — Documentation

Joey Agent is a self-improving, tool-using AI agent, implemented as a Rust
workspace. It is a from-scratch Rust rewrite of **Hermes Agent** (Nous
Research, MIT-licensed): no Python code is shared, but data formats,
defaults, and wire behavior are deliberately kept faithful to the upstream
project so the two remain drop-in compatible (state database schema, cron
job format, skill format, session-key grammar, provider payloads, etc).

This `docs/` folder is the authoritative technical reference for the
project's internals — how the agent actually thinks, loops, calls tools,
manages context, talks to providers, schedules jobs, and exposes a CLI.
It complements the top-level `README.md` (quick start / user-facing) and
`PORTING.md` (upstream parity tracker).

## How to read these docs

Start with the **Architecture Overview**, then dive into whichever
subsystem you're working on. Docs that exist today:

1. [`architecture.md`](architecture.md) — the Cargo workspace (13 crates),
   crate dependency graph, high-level data flow from user input to final
   answer.
2. [`agent-turn-loop.md`](agent-turn-loop.md) — the heart of the project:
   `Agent::run_turn`, the iteration loop, tool dispatch, retries, fallback
   providers, interrupts, and persistence.
3. [`HOOKS.md`](HOOKS.md) — `PreToolUse` hooks: shell-command hooks
   configured in `config.yaml`, the JSON stdin contract, exit-code
   semantics (allow/deny/halt), and argument-rewriting via stdout.
4. [`LSP.md`](LSP.md) — the Language Server Protocol integration: how to
   configure a language server per file type, and the `lsp_diagnostics` /
   `lsp_definition` / `lsp_references` / `lsp_symbols` tools that appear
   only when one is configured.

The following topics are referenced by the codebase's module layout but do
not yet have a dedicated doc page — read the crate itself (or the relevant
section of `architecture.md`) until these are written:

- `system-prompt.md` — system prompt assembly (`joey-agent-core::prompt`)
- `context-compression.md` — automatic context-window compaction
  (`joey-agent-core::agent` compression path)
- `tools.md` — the `Tool` trait, registry, and built-ins (`joey-tools`)
- `providers.md` — provider wire protocols (`joey-providers`)
- `security.md` — threat scanning, redaction, untrusted-content handling
- `cron.md` — the scheduler (`joey-cron`)
- `mcp.md` — the MCP stdio client (`joey-mcp`)
- `gateway.md` — the messaging-platform-neutral spine (`joey-gateway`)
- `cli.md` — the `joey` binary (`joey-cli`)
- `state-and-config.md` — `~/.joey` layout, layered config, session store
- `events.md` — the `AgentEvent` stream
- `orchestration.md` — subagent delegation (`joey-orchestration`, `joey-omo`)
- `tui.md` — the animated dashboard (`joey-tui`)

## Project layout

```
joey-agent/
├── Cargo.toml                 workspace manifest (13 member crates)
├── crates/
│   ├── joey-core/             branding, config, SQLite state store, logging, redaction
│   ├── joey-providers/        LLM provider wire protocols + client
│   ├── joey-tools/            Tool trait, registry, toolsets, built-in tools
│   ├── joey-agent-core/       the turn loop, system prompt, context compression
│   ├── joey-cron/             built-in scheduler
│   ├── joey-mcp/              MCP (Model Context Protocol) stdio client
│   ├── joey-gateway/          messaging-platform-neutral spine
│   ├── joey-cli/              the `joey` binary (REPL + subcommands)
│   ├── joey-tui/              --tui animated ratatui dashboard
│   ├── joey-llm-selector/     dynamic model allocator (model.default = auto)
│   ├── joey-orchestration/    subagent manager + delegate_task tool
│   ├── joey-omo/              multi-agent personas, routing, Atlas plan execution
│   └── joey-speckit-ui/       standalone backend for the SpecKit Visual UI
├── docs/                      you are here
├── skills/                    Agent Skills bundled with the project
├── README.md                  user-facing quick start
└── PORTING.md                 upstream (Hermes Agent) parity tracker
```

## Conventions used throughout the docs

- File/line references like `agent.rs:290` point at
  `crates/joey-agent-core/src/agent.rs` line 290 as of the time of writing;
  treat them as approximate signposts, not permanent anchors.
- "Upstream" always means Hermes Agent (the Python project this is a port
  of). Comments in the Rust source frequently cite the exact upstream
  Python file/line the logic was ported from (e.g. `conversation_loop.py:4309`)
  — these citations are preserved in the docs below because they are the
  most precise description of *why* a given constant or behavior exists.
- `~/.joey` (the "joey home") is the default state directory; it is
  fully overridable via `JOEY_HOME` and is per-profile
  (`~/.joey/profiles/<name>`) when `-p/--profile` is used.
