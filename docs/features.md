# Joey Agent — Feature Overview

An up-to-date, feature-first tour of everything Joey Agent can do. For
per-subsystem depth, follow the links into the reference pages. (Last
updated against workspace code 2026-08.)

## What Joey Agent is

A self-improving, tool-using AI agent as a single native Rust binary
(`joey`), a from-scratch rewrite of Hermes Agent (Nous Research). It shares
no Python code with upstream but keeps data formats, defaults, and wire
behavior byte-for-byte compatible where possible — SQLite session schema,
cron jobs.json, SKILL.md format, session-key grammar, provider payloads.

## Core agent loop

- Multi-iteration tool-calling turn loop (`Agent::run_turn`) with retries,
  fallback provider chains, interrupts, and full session persistence.
  → [agent-turn-loop.md](agent-turn-loop.md)
- System prompt assembled once per session (identity/SOUL.md, guidance,
  skills index, project context files, memory) to keep provider
  prompt-prefix caches warm. → [agent-core-reference.md](agent-core-reference.md)
- Automatic context compression with protected head/tail regions,
  structured summaries, per-session locks, and overflow recovery.
  → [agent-core-reference.md](agent-core-reference.md#3-context-compression-compression)
- Streaming (SSE) responses, reasoning/thinking display, token accounting.

## Tools

- 17 always-on built-in tools + 6 conditional ones: file read/write/patch
  (9-strategy fuzzy matching), ripgrep search, terminal + background
  process management, todo, persistent memory, Tavily web search/extract,
  skills, 4 LSP tools, NeuroCode graph tools, session search, clarify.
  → [tools.md](tools.md)
- Parallel-safe read-only tools dispatch concurrently; everything else is
  sequential with configurable delay.
- Filesystem checkpointing (shadow-git VCS): snapshot, list, `/rollback`,
  per-project retention. → [tools.md](tools.md#7-checkpoint-vcs-src-vcsrs)
- Tool result persistence: oversized outputs spill to `~/.joey/storage`
  with head previews.

## Models and providers

- 10 registered provider profiles (OpenRouter, Anthropic, OpenAI, GitHub
  Copilot, ai-usage-hud, Nous, DeepSeek, Gemini, Z.AI, xAI) over three
  wire protocols, plus any OpenAI-compatible custom endpoint.
  → [providers.md](providers.md)
- `model.default = auto` engages the dynamic llm-selector: per-module
  (main turn / compression / subagent) model allocation across
  Flash/Standard/Versatile/Frontier tiers, with a diagnoser, budgets, and
  persisted allocations. → [providers.md](providers.md)
- NeuroCode tiered routing (`/neurocode` command family) for
  code-aware context injection. → [providers.md](providers.md)

## Multi-agent

- `delegate_task`: single or parallel-batch subagents with isolated
  contexts, per-task model/toolset/budget, concurrency limits.
  → [orchestration.md](orchestration.md)
- OMO (oh-my-openagent): 11 agent personas, 11 delegation categories,
  intent gating (ultrawork/hyperplan/team), goals, boulder plan execution,
  wisdom notepads, optional team mode. → [orchestration.md](orchestration.md)

## Interfaces

- Line REPL with 80+ registered slash commands (~30 implemented), prefix
  expansion, Tab completion. → [cli.md](cli.md)
- Animated ratatui TUI (`--tui`): synthwave theme, streaming transcript,
  reasoning panel, agent picker, activity-scaled animations.
  → [tui.md](tui.md)
- One-shot mode (`-z/--oneshot`) for scripting, with `--usage-file` JSON
  reports.
- SpecKit Visual UI (`joey speckit`): browser UI over `.specify/`
  artifacts, files stay source of truth. → [speckit-ui.md](speckit-ui.md)

## Scheduling and integrations

- Built-in cron scheduler: duration/interval/cron-expression jobs, agent
  or script jobs, delivery targets, standalone ticker. → [cron.md](cron.md)
- MCP stdio client: `mcp__<server>__<tool>` tools, config validation,
  result sanitization. → [mcp.md](mcp.md)
- Messaging gateway spine (platform-neutral session keys + adapter trait;
  concrete adapters not yet shipped). → [gateway.md](gateway.md)
- PreToolUse hooks: allow/deny/halt/rewrite tool calls from config-defined
  shell hooks. → [HOOKS.md](HOOKS.md)
- Local model discovery (`joey discover`): Ollama, LM Studio, llama.cpp,
  LiteLLM, MLX.

## State, config, and safety

- Layered YAML + `.env` config under `~/.joey/` (~80 documented keys),
  per-profile isolated homes, SQLite session store (schema v22, FTS5
  search, non-destructive compaction). → [state-and-config.md](state-and-config.md)
- Persistent cross-session memory and user profile (char-budgeted,
  injected every turn). → [state-and-config.md](state-and-config.md)
- Security: secret redaction everywhere, threat scanning, untrusted-content
  wrapping, SSRF guards, file guards, dangerous-command approvals,
  safe mode. → [security.md](security.md)

## Not yet implemented (upstream parity gaps)

Recognized but stubbed ("not available yet", exit 1): most `skills`
subcommands (only `list` works), `mcp serve/catalog/...`, `config
check/migrate`, `cron edit/runs/history`, ~50 REPL slash commands
(/save, /retry, /undo, /title, /browser, /plugins, …), browser_* /
vision_analyze / execute_code / computer_use / ha_* / kanban_* tools,
Anthropic-OAuth subscription login (deliberately omitted), gateway
platform adapters. See `PORTING.md` for the full tracker.
