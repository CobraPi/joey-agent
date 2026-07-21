# Porting status: Hermes Agent → Joey Agent (Rust)

This document tracks what has been ported from the ~690K-line Python
`hermes-agent` into the Rust `joey-agent`, and what remains. The goal is a
faithful clone: same architecture, defaults, data formats, and wire behavior,
rebranded `hermes → joey`.

## Complete and working (compiles, tested, runs end-to-end)

**Core foundation (`joey-core`)** — port of `hermes_constants.py`, `hermes_state.py`,
`hermes_time.py`, `hermes_logging.py`, `utils.py`, config layer:
- Home/profile resolution (`JOEY_HOME`, platform defaults, profile override, container/WSL/Termux detection, subprocess HOME contract).
- Layered config: `DEFAULT_CONFIG` ← `config.yaml` ← `.env` ← flags, with dotted get/set, env-key routing to `.env`, deep-merge preserving unknown keys.
- SQLite session store: sessions + messages + FTS5 search + triggers, session-id `YYYYMMDD_HHMMSS_hex6`, prefix resume, WAL.
- Timezone clock, rotating logs, secret redaction, reasoning-effort parsing (spelling-tolerant per-model overrides), atomic file writes.

**Provider layer (`joey-providers`)** — port of `providers/`, `agent/transports/`:
- Declarative provider profiles + registry (10 providers) with base-url + model-prefix resolution.
- OpenAI Chat Completions wire adapter (request build, tool calls, reasoning, usage) + SSE streaming with incremental tool-call assembly.
- Anthropic Messages wire adapter (tool_use/tool_result mapping, extended thinking, image blocks, x-api-key vs OAuth Bearer) + SSE streaming.
- Error taxonomy + classification (auth/rate-limit/payment/413/timeout), retryable/failover flags, jittered backoff.

**Tool system (`joey-tools`)** — port of `tools/`, `toolsets.py`:
- `Tool` trait + registry (schema gen, `check` gating, dispatch with the str/multimodal result contract).
- Toolset resolver with recursive includes, rebranded `joey-*` bundles.
- Schema sanitizer, output truncation (40/60 head-tail), 9-strategy fuzzy matcher behind `patch`.
- Built-in tools: `read_file`, `write_file`, `patch`, `search_files`, `terminal`, `todo`, `memory`, `web_search`, `web_extract`, `skills_list`, `skill_view`. SSRF guard on web fetch.

**Agent loop (`joey-agent-core`)** — port of `run_agent.py`, `agent/conversation_loop.py`, `agent/prompt_builder.py`:
- The turn loop: message assembly → provider call → tool dispatch → loop until no tool calls or `max_turns`.
- System-prompt builder (identity, environment, memory/user files, `<available_skills>` index).
- Streaming event bridge (content/reasoning deltas, tool progress), provider retries with backoff, usage accounting.

**Cron (`joey-cron`)** — port of `cron/`:
- Job store (`jobs.json`), schedule parsing (`30m` / `every 2h` / cron expr / ISO), next-run computation, 60s ticker, per-job output files. Each job runs its prompt through a headless agent.

**MCP client (`joey-mcp`)** — port of the client side of `tools/mcp_tool.py`:
- Stdio JSON-RPC: spawn server, `initialize` handshake, `tools/list`, `tools/call`, `mcp__server__tool` naming.

**Gateway core (`joey-gateway`)** — port of `gateway/` spine:
- Session-key builder (namespace/dm/group/thread grammar), `SessionSource`, `MessageEvent`/`SendResult`, `PlatformAdapter` trait.

**CLI (`joey-cli`)** — port of `hermes_cli/`, `cli.py`:
- The `joey` binary: clap tree (`model`, `tools`, `config`, `doctor`, `version`, `cron`, `mcp`, `skills`, `home`), one-shot `-q`, model/resume flags.
- Interactive REPL with line editing, history, streaming render, tool-progress display, slash commands.

**Verified end-to-end:** 49 unit tests pass; the binary runs `version`/`model`/`tools`/
`doctor`/`config set|get`/`cron create|list`; a full turn loop (assistant → `terminal`
tool → tool result → final answer) was exercised against a mock OpenAI server.

## Partial

- **Providers:** OpenAI + Anthropic wire modes are complete. Codex/Responses, Bedrock
  Converse, native Gemini REST, Vertex, and Azure adapters are not yet ported (the profile
  registry is structured to add them incrementally).
- **Tools:** the self-contained core is done. `session_search`, `delegate_task`, `clarify`,
  `process` (background procs), `cronjob` (agent-callable), and MCP-tool injection into the
  registry are stubbed for higher layers to wire.
- **Context compression** is not yet implemented (the agent loop tracks usage but does not
  auto-compact); large sessions will eventually hit the context window.
- **Skills** are discovered, indexed into the prompt, and viewable; the self-improvement
  loop (curator, skill authoring/patching) is not ported.

## Deferred (matches upstream's own "defer for a first port" guidance)

- The 20 messaging platform adapters (Telegram, Discord, Slack, WhatsApp, Matrix, …) —
  the `PlatformAdapter` trait is in place; concrete adapters are additive.
- The FastAPI dashboard / web server and the Electron desktop app.
- The TUI-gateway JSON-RPC protocol, ACP editor adapter, relay/connector.
- Kanban multi-agent coordination, projects, blueprints, memory providers (Honcho/mem0/…),
  computer-use, TTS/STT/voice, image/video generation, browser automation.
- The 6 terminal backends beyond `local` (docker/ssh/singularity/modal/daytona).
- Research tooling: batch runner, trajectory compressor, mini-swe runner.

## Branding conversion (complete)

`~/.hermes` → `~/.joey` · `HERMES_*` → `JOEY_*` · `hermes` command → `joey` ·
`hermes-*` toolsets → `joey-*` · package `hermes-agent` → `joey-agent`. MIT license and
upstream attribution retained throughout. The `mcp__` wire prefix and `§` memory delimiter
are kept identical for interoperability.
