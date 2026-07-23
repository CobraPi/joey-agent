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
subsystem you're working on:

1. [`architecture.md`](architecture.md) — the Cargo workspace, crate
   dependency graph, high-level data flow from user input to final answer.
2. [`agent-turn-loop.md`](agent-turn-loop.md) — the heart of the project:
   `Agent::run_turn`, the iteration loop, tool dispatch, retries, fallback
   providers, interrupts, and persistence.
3. [`system-prompt.md`](system-prompt.md) — how the system prompt is
   assembled: identity (SOUL.md), guidance blocks, skills index, project
   context files (AGENTS.md/.joey.md/CLAUDE.md/.cursorrules), environment
   hints, and the volatile memory/user-profile tail.
4. [`context-compression.md`](context-compression.md) — the automatic
   context-window compaction engine: thresholds, protected zones,
   summarization via an auxiliary model, feedback loop, and manual
   `/compress`.
5. [`tools.md`](tools.md) — the `Tool` trait, the registry, toolsets,
   built-in tools, the fuzzy patch matcher, output persistence/truncation,
   and the parallel-safe / untrusted-tool security model.
6. [`providers.md`](providers.md) — the provider abstraction: profiles,
   wire protocols (OpenAI Chat Completions, OpenAI Responses, Anthropic
   Messages), streaming, retries/backoff, and the fallback chain.
7. [`security.md`](security.md) — threat scanning of context files, tool
   error sanitization, untrusted tool-result wrapping, MCP server
   validation, secret redaction.
8. [`cron.md`](cron.md) — the built-in scheduler: job store, schedule
   grammar, ticker, job execution.
9. [`mcp.md`](mcp.md) — the Model Context Protocol client: stdio
   transport, handshake, tool discovery/naming, pagination, timeouts.
10. [`gateway.md`](gateway.md) — the platform-neutral messaging spine
    (session keys, message events, platform adapter trait).
11. [`cli.md`](cli.md) — the `joey` binary: argument parsing, profiles,
    REPL, one-shot mode, slash commands, subcommands.
12. [`state-and-config.md`](state-and-config.md) — `~/.joey` layout,
    layered configuration, the SQLite session store, logging, redaction.
13. [`events.md`](events.md) — the `AgentEvent` stream consumed by UIs
    (CLI renderer, gateway adapters).

## Project layout

```
joey-agent/
├── Cargo.toml                 workspace manifest (8 member crates)
├── crates/
│   ├── joey-core/             branding, config, SQLite state store, logging, redaction
│   ├── joey-providers/        LLM provider wire protocols + client
│   ├── joey-tools/            Tool trait, registry, toolsets, built-in tools
│   ├── joey-agent-core/       the turn loop, system prompt, context compression
│   ├── joey-cron/             built-in scheduler
│   ├── joey-mcp/              MCP (Model Context Protocol) stdio client
│   ├── joey-gateway/          messaging-platform-neutral spine
│   └── joey-cli/              the `joey` binary (REPL + subcommands)
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
