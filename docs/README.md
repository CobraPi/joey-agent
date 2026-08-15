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

Start with the **Feature Overview** for a tour of everything the agent
does today, then the **Architecture Overview**, then dive into whichever
subsystem you're working on:

1. [`features.md`](features.md) — up-to-date feature-first overview of the
   whole project: every capability with links into the per-subsystem
   reference pages, plus an honest "not yet implemented" list.
2. [`architecture.md`](architecture.md) — the Cargo workspace (13 crates),
   crate dependency graph, high-level data flow from user input to final
   answer.
3. [`agent-turn-loop.md`](agent-turn-loop.md) — the heart of the project:
   `Agent::run_turn`, the iteration loop, tool dispatch, retries, fallback
   providers, interrupts, and persistence.
4. [`agent-core-reference.md`](agent-core-reference.md) — the comprehensive
   `joey-agent-core` reference: system-prompt assembly tiers, context
   compression triggers/thresholds/protection, the full `AgentEvent`
   catalog, hooks integration inside the loop, tunables, and subagent
   wiring. Complements `agent-turn-loop.md`.
5. [`tools.md`](tools.md) — the `Tool` trait, registry and dispatch
   semantics, the complete built-in tool inventory (17 always-on + 6
   conditional), the 18-toolset hierarchy with recursive includes,
   parallel-safe vs sequential dispatch, fuzzy patch matching, checkpoint
   VCS, and the sanitizer/guard layers.
6. [`providers.md`](providers.md) — provider wire protocols, the provider
   registry (10 profiles) and model routing
   (`model.provider`/`base_url`/API-key resolution), SSE streaming,
   retries/backoff and error classification, reasoning/thinking support
   per provider, tool-call wire formats, and the dynamic
   `joey-llm-selector` model allocator.
7. [`cli.md`](cli.md) — the `joey` binary's complete user-facing surface:
   every subcommand and flag, exit codes, the REPL slash-command registry,
   setup wizard, and profiles.
8. [`state-and-config.md`](state-and-config.md) — `joey-core`: layered
   config (YAML + `.env` + env), ~80 meaningful config keys, `~/.joey`
   directory layout, the SQLite session store (schema v22, FTS5 search,
   non-destructive compaction), profiles, secret redaction, and logging.
9. [`security.md`](security.md) — the consolidated security model: secret
   redaction, credential storage, threat scanning, untrusted-content
   wrapping, SSRF/URL safety, file guards, dangerous-command approvals,
   and the deliberate Anthropic-OAuth omission.
10. [`orchestration.md`](orchestration.md) — subagent delegation
    (`delegate_task`, SubagentManager) and OMO multi-agent orchestration
    (11 agents, 11 categories, intent gating, goals/boulder/notepads,
    team mode).
11. [`cron.md`](cron.md) — the built-in scheduler: schedule kinds, job
    store format, delivery targets, script jobs, and the `joey cron` CLI.
12. [`mcp.md`](mcp.md) — the MCP stdio client: server configuration,
    `mcp__<server>__<tool>` naming, sanitization layers, and the
    `joey mcp` CLI.
13. [`gateway.md`](gateway.md) — the messaging-platform-neutral spine:
    Platform enum, session keys, `PlatformAdapter` trait (no concrete
    adapters ship yet).
14. [`tui.md`](tui.md) — the animated ratatui dashboard: views/panels,
    activity-scaled animations, run modes.
15. [`speckit-ui.md`](speckit-ui.md) — the SpecKit Visual UI backend:
    artifact parsing, workflow engine, HTTP/WS API, git-backed staging.
16. [`HOOKS.md`](HOOKS.md) — `PreToolUse` hooks: shell-command hooks
    configured in `config.yaml`, the JSON stdin contract, exit-code
    semantics (allow/deny/halt), and argument-rewriting via stdout.
17. [`LSP.md`](LSP.md) — the Language Server Protocol integration: how to
    configure a language server per file type, and the `lsp_diagnostics` /
    `lsp_definition` / `lsp_references` / `lsp_symbols` tools that appear
    only when one is configured.
18. [`speckit-ui-launcher.md`](speckit-ui-launcher.md) — launch recipe for
    the SpecKit visual UI (`joey speckit`).

All pages were verified against workspace source in August 2026. If code
and docs disagree, the code wins — and please fix the doc.

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
