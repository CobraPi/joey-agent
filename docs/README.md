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

Start with the **End-to-End Walkthrough** to see one request written
through every layer of the system, then the **Feature Overview** for a
tour of everything the agent does today, then the **Architecture Overview**
and **System Diagrams**, then dive into whichever subsystem you're
working on:

1. [`how-it-works.md`](how-it-works.md) — the end-to-end walkthrough:
   startup → config → session → the turn loop → tool dispatch → provider
   wire → context management → events → persistence → subagents / cron /
   browser / MCP → security, one request written through every layer, with
   links into each reference page.
2. [`features.md`](features.md) — up-to-date feature-first overview of the
   whole project: every capability with links into the per-subsystem
   reference pages, plus an honest "not yet implemented" list.
3. [`architecture.md`](architecture.md) — the Cargo workspace (19 crates),
   the crate dependency DAG, system context, high-level data flow from
   user input to final answer, and the design principles — with Mermaid
   diagrams.
4. [`system-diagrams.md`](system-diagrams.md) — the detailed Mermaid
   diagram collection: turn-loop internals, tool dispatch, provider
   stack, prompt assembly, the context-management pipeline (economy /
   assembly / gauge / compression), subagent delegation, the HyperCode
   pipeline, the browser-explorer steering loop (035), the enterprise
   TaskGraph runtime (023), NeuroCode + RAG, the compute pool (033), the
   llm-selector, security layers, the TUI engine actor, cron, MCP,
   SpecKit UI, and the state/config layout.
5. [`agent-turn-loop.md`](agent-turn-loop.md) — the heart of the project:
   `Agent::run_turn`, the iteration loop, tool dispatch, retries, fallback
   providers, interrupts, and persistence.
6. [`agent-core-reference.md`](agent-core-reference.md) — the comprehensive
   `joey-agent-core` reference: system-prompt assembly tiers (including
   the Joey-only goal-directed guidance, feature 031), context
   compression triggers/thresholds/protection, the full `AgentEvent`
   catalog, hooks integration inside the loop, tunables, and subagent
   wiring. Complements `agent-turn-loop.md`.
7. [`context-economy.md`](context-economy.md) — the five default-on context
   economy mechanisms (feature 028): session scratchpad tool, deterministic
   state block, mid-turn tool-result hygiene, boundary-aligned cleanup, and
   economy guidance + retrieval verification nudge; the 10 additive config
   keys, byte-parity guarantees when disabled, and observability.
8. [`tools.md`](tools.md) — the `Tool` trait, registry and dispatch
   semantics, the complete built-in tool inventory (17 always-on + 6
   conditional), the 18-toolset hierarchy with recursive includes,
   parallel-safe vs sequential dispatch, fuzzy patch matching, checkpoint
   VCS, the sanitizer/guard layers, and the feature-034 payload store
   (pass-by-reference for heavyweight tool inputs).
9. [`providers.md`](providers.md) — provider wire protocols, the provider
   registry (10 profiles) and model routing
   (`model.provider`/`base_url`/API-key resolution), SSE streaming,
   retries/backoff and error classification, reasoning/thinking support
   per provider, tool-call wire formats, and the dynamic
   `joey-llm-selector` model allocator.
10. [`browser.md`](browser.md) — browser automation (feature 016): the
    `joey-browser` crate, CDP attach vs managed launch, the browser tool
    family, dedicated-tab discipline, snapshot/action semantics, overlay and
    settle policy, the per-provider image-model keys, and local decision
    acceleration via the Jev service (feature 036: `joey-jev` crate, four
    decision points, `jev.*` config, decision log).
11. [`cli.md`](cli.md) — the `joey` binary's complete user-facing surface:
    every subcommand and flag, exit codes, the REPL slash-command registry,
    setup wizard, and profiles.
12. [`state-and-config.md`](state-and-config.md) — `joey-core`: layered
    config (YAML + `.env` + env), ~80 meaningful config keys, `~/.joey`
    directory layout, the SQLite session store (schema v22, FTS5 search,
    non-destructive compaction), profiles, secret redaction, and logging.
13. [`security.md`](security.md) — the consolidated security model: secret
    redaction, credential storage, threat scanning, untrusted-content
    wrapping, SSRF/URL safety, file guards, dangerous-command approvals,
    and the deliberate Anthropic-OAuth omission.
14. [`orchestration.md`](orchestration.md) — subagent delegation
     (`delegate_task`, SubagentManager), OMO multi-agent orchestration
     (11 agents, 11 categories, intent gating, goals/boulder/notepads,
     team mode), the OMO integration (feature 025), subagent resource
     governance (feature 030), and the orchestrator-steered browser
     explorer (feature 035). Subagents inherit the parent's NeuroCode
     engine (same graph.db; task-targeted context).
15. [`cron.md`](cron.md) — the built-in scheduler: schedule kinds, job
     store format, delivery targets, script jobs, and the `joey cron` CLI.
16. [`mcp.md`](mcp.md) — the MCP stdio client: server configuration,
     `mcp__<server>__<tool>` naming, sanitization layers, and the
     `joey mcp` CLI.
17. [`gateway.md`](gateway.md) — the messaging-platform-neutral spine:
     Platform enum, session keys, `PlatformAdapter` trait (no concrete
     adapters ship yet).
18. [`tui.md`](tui.md) — the animated ratatui dashboard: views/panels,
     activity-scaled animations, run modes.
19. [`speckit-ui.md`](speckit-ui.md) — the SpecKit Visual UI backend:
     artifact parsing, workflow engine, HTTP/WS API, git-backed staging.
20. [`HOOKS.md`](HOOKS.md) — `PreToolUse` hooks: shell-command hooks
     configured in `config.yaml`, the JSON stdin contract, exit-code
     semantics (allow/deny/halt), and argument-rewriting via stdout.
21. [`LSP.md`](LSP.md) — the Language Server Protocol integration: how to
     configure a language server per file type, and the `lsp_diagnostics` /
     `lsp_definition` / `lsp_references` / `lsp_symbols` tools that appear
     only when one is configured.
22. [`speckit-workflow.md`](speckit-workflow.md) — the full spec-kit
     lifecycle as native slash commands (`/speckit-specify` → … →
     `/speckit-implement`): real `.specify/` scripts for pre-flight,
     bundled skill workflows as agent turns, `/speckit-status` readiness.
23. [`speckit-ui-launcher.md`](speckit-ui-launcher.md) — launch recipe for
     the SpecKit visual UI (`joey speckit`).
24. [`copilot.md`](copilot.md) — native GitHub Copilot `.github/`
     integration: copilot-instructions.md / instructions / prompts / skills
     parsing, the `joey copilot` plugin installer, `/copilot` + `/<prompt>`
     slash commands, and `.github/mcp.json` merge semantics.
25. [`features/README.md`](features/README.md) — per-crate feature
     reference — one deep-dive page per workspace crate (APIs, behaviors,
     configuration, defaults, testing), including
     [`features/joey-compute.md`](features/joey-compute.md) for the
     compute-pool crate.
26. [`context-assembly.md`](context-assembly.md) — the dynamic
     context-assembly layer: relevance-ranked tool-schema selection
     ("tool schema retrieval"), always-keep tool pinning, state-block
     character budget, per-step JSONL assembly logging, the
     `/context-assembly` (`/ctxasm`) REPL command, byte parity when
     disabled, cache-stability guarantees — and the feature-034 group
     (live context gauge, unified notice channel, reasoning pruning).
27. [`compute-pool.md`](compute-pool.md) — the `joey-compute` CPU-bound
     work execution substrate (feature 033): dedicated OS worker threads
     (never tokio workers), earliest-deadline-first scheduling with
     bounded starvation, semaphore admission/backpressure, panic
     isolation, chunked cancellation, the watchdog, metrics, the four
     `orchestration.compute.*` config keys, and the orchestrator-side
     weight policy.

All pages were verified against workspace source in September 2026. If code
and docs disagree, the code wins — and please fix the doc.

## Project layout

```
joey-agent/
├── Cargo.toml                 workspace manifest (19 member crates)
├── crates/
│   ├── joey-core/             branding, config, SQLite state store, logging, redaction
│   ├── joey-compute/          CPU-bound compute pool (feature 033)
│   ├── joey-providers/        LLM provider wire protocols + client
│   ├── joey-tools/            Tool trait, registry, toolsets, built-in tools
│   ├── joey-agent-core/       the turn loop, system prompt, context compression
│   ├── joey-cron/             built-in scheduler
│   ├── joey-mcp/              MCP (Model Context Protocol) stdio client
│   ├── joey-gateway/          messaging-platform-neutral spine
│   ├── joey-cli/              the `joey` binary (REPL + subcommands)
│   ├── joey-tui/              --tui animated ratatui dashboard
│   ├── joey-llm-selector/     dynamic model allocator (model.default = auto)
│   ├── joey-orchestration/    subagent manager + delegate_task + TaskGraph + browser explorer
│   ├── joey-omo/              multi-agent personas, routing, Atlas plan execution
│   ├── joey-neurocode/        NeuroCode code-graph intelligence (ingest, tiers, context assembly)
│   ├── joey-neurocode-rag/    NeuroCode RAG: embedding backends, hybrid search, consent
│   ├── joey-jev/              client for the local Jev decision service (feature 036)
│   ├── joey-copilot/          GitHub Copilot `.github/` bundle discovery & plugin manifest
│   └── joey-speckit-ui/       backend for the SpecKit Visual UI (parsers shared with joey-cli)
├── docs/                      you are here
├── skills/                    Agent Skills bundled with the project
├── README.md                  user-facing quick start
└── PORTING.md                 upstream (Hermes Agent) parity tracker
```

## Conventions used throughout the docs

- File/line references like `agent.rs:290` point at
  `crates/joey-agent-core/src/agent.rs` line 290 as of the time of writing;
  treat them as approximate signposts, not permanent anchors.
- Diagrams are [Mermaid](https://mermaid.js.org) — GitHub renders them
  natively; when a subsystem changes shape, update its diagram in the same
  commit (see the maintenance note in `system-diagrams.md`).
- "Upstream" always means Hermes Agent (the Python project this is a port
  of). Comments in the Rust source frequently cite the exact upstream
  Python file/line the logic was ported from (e.g. `conversation_loop.py:4309`)
  — these citations are preserved in the docs below because they are the
  most precise description of *why* a given constant or behavior exists.
- `~/.joey` (the "joey home") is the default state directory; it is
  fully overridable via `JOEY_HOME` and is per-profile
  (`~/.joey/profiles/<name>`) when `-p/--profile` is used.
