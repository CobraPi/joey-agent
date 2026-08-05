<h1 align="center">Joey Agent ☤</h1>

<p align="center"><b>The self-improving AI agent — in Rust.</b></p>

<p align="center">
A ground-up Rust rewrite of <a href="https://github.com/NousResearch/hermes-agent">Hermes Agent</a> (Nous Research, MIT).
Same architecture, same behavior, native binary.
</p>

---

Joey Agent is an autonomous, tool-using AI agent you drive from your terminal. Point it
at any OpenAI-compatible or Anthropic model, and it reads and writes files, runs shell
commands, searches the web, manages its own memory and skills, and schedules recurring
work — all from a single native binary with no runtime to install.

It is a faithful port of Hermes Agent: the provider layer, tool system, turn loop,
toolsets, session store, cron scheduler, and MCP client are re-implemented in Rust with
the same defaults and wire behavior. Everything is rebranded `hermes → joey`
(`~/.hermes → ~/.joey`, `HERMES_* → JOEY_*`, the `hermes` command → `joey`).

## Install

```bash
git clone <this-repo> joey-agent && cd joey-agent
cargo build --release
# the binary is at target/release/joey — put it on your PATH
install -m755 target/release/joey ~/.local/bin/joey
```

Requires a recent stable Rust toolchain (1.80+). Bundled SQLite is compiled in — no
system SQLite needed. `ripgrep` (`rg`) is recommended for faster search.

## Quick start

```bash
joey model                              # interactive provider + model picker (persists to config)
joey config set OPENROUTER_API_KEY sk-… # store a provider key (goes to ~/.joey/.env)
joey                                    # start an interactive chat session
joey -z "what changed in the last commit?"   # one-shot, prints only the final answer
```

Pick any provider with no code changes:

```bash
joey config set model.provider anthropic
joey config set model.default anthropic/claude-opus-4.6
joey config set ANTHROPIC_API_KEY sk-ant-…
```

GitHub Copilot uses the same two-phase authentication flow as Hermes Agent:

```bash
joey auth copilot login                 # OAuth device-code login
joey model                              # select GitHub Copilot + a live catalog model
joey auth copilot status
```

Joey resolves `COPILOT_GITHUB_TOKEN`, `GH_TOKEN`, `GITHUB_TOKEN`, then
`gh auth token`; exchanges it for a short-lived Copilot API token; refreshes on
expiry/401; and honors enterprise endpoints returned by GitHub.

Supported provider wire protocols out of the box: **OpenAI Chat Completions** (OpenRouter,
OpenAI, Nous, DeepSeek, Groq, Gemini, xAI, Z.ai, Ollama, GitHub Copilot, and custom
OpenAI-compatible endpoints), **OpenAI Responses** (including Copilot GPT-5+/Codex), and
**Anthropic Messages** (native and Copilot Claude, with extended thinking). SSE streaming
is supported on all three.

## Commands

```
joey                       Start the interactive REPL
joey --tui                 Launch the animated ratatui dashboard instead of the line REPL
joey -z "<prompt>"         One-shot headless query (prints only the final answer)
joey chat -q "<prompt>"    One-shot through the chat path (banner/session unless -Q)
joey -m <model>            Override the model for this run
joey -r <id-or-title>      Resume a past session · joey -c resumes the most recent
joey -p <profile>          Use a named profile home (~/.joey/profiles/<name>)
joey -s <skills>           Preload one or more Agent Skills for the session
joey --yolo                Bypass all dangerous-command approval prompts
joey --safe-mode           Disable all customizations (user config + MCP servers) for troubleshooting

joey model                 Interactive provider + model picker (persists selection)
joey auth <provider>       Manage provider authentication (e.g. `auth copilot login|status`)
joey tools                 --summary | list | enable/disable <names> [--platform]
joey skills                Search, install, inspect, and manage skills
joey config                show | edit | get | set | unset | path | env-path
joey doctor [--fix]        Diagnose the environment (and fix what it can)
joey discover              Discover local model servers (Ollama, LM Studio, llama.cpp, …)
joey llm-selector <sub>    CLI mirror of `/llm-selector` (status | pool | pin | allocations | …)
joey version               Show version + upstream attribution
joey home                  Print the resolved ~/.joey directory (joey extension)

joey cron                              List scheduled jobs (also: cron list)
joey cron create "<sched>" "<prompt>"  Create a job ("30m", "every 2h", "0 9 * * *", ISO)
joey cron pause|resume|remove <job>    Manage a job (by id or name)
joey cron run <job>                    Trigger a job now
joey cron tick [--loop]                Run due jobs once (--loop: 60s scheduler daemon)
joey cron status                       Scheduler heartbeat + job counts

joey mcp add <name> --command …        Register a stdio MCP server (config.yaml mcp_servers)
joey mcp list | test <name> | remove   Inspect, probe, or remove configured servers
```

Inside the REPL the full upstream slash-command set is recognized (`/help` lists it,
grouped by Session / Configuration / Tools & Skills / Info / Exit); implemented today:
`/new` (`/reset`), `/clear`, `/history`, `/compress` (`/compact`), `/rollback`,
`/checkpoint` (`/snap`), `/agents` (`/tasks`), `/start-work`, `/queue` (`/q`), `/goal`,
`/status`, `/changes`, `/resume`, `/sessions`, `/config`, `/model`, `/llm-selector`,
`/timestamps` (`/ts`), `/verbose`, `/reasoning`, `/tools`, `/toolsets`, `/skills`,
`/help`, `/usage`, `/copy`, `/version` (`/v`), `/quit` (`/exit`). Everything else in
the registry is recognized (so `/handoff`, `/undo`, etc. answer honestly) but not yet
wired to a handler.

`/agents` shows the live OMO (Oh My OpenAgent) roster — 11 built-in agent personas with
per-agent model-family fallback chains — and `/start-work` activates the Atlas
plan-execution loop against a `.omo/plans/<name>.md` file, delegating tasks to
subagents via `delegate_task`. `/llm-selector` (and `joey llm-selector` at the top
level) controls the dynamic model allocator: pool status, per-module pinning, and
learned allocations, used when `model.default` is set to `auto`.

Long sessions auto-compact like upstream: when context usage crosses the configured
threshold (or the provider rejects an oversized request), older history is summarized
by the auxiliary model and archived — recent messages, the system prompt, and the
first turns are preserved verbatim, and archived rows remain searchable in `state.db`.

## Built-in tools

| Tool | What it does |
|------|--------------|
| `read_file` | Read a text file with line numbers + pagination |
| `write_file` | Create/overwrite a file (atomic) |
| `patch` | Targeted find/replace with a 9-strategy fuzzy matcher |
| `multi_edit` | Apply several find/replace edits to a file in one call |
| `search_files` | Regex content search / glob file search (gitignore-aware) |
| `terminal` | Run a shell command (head/tail-bounded output, secret-redacted) |
| `process` | Manage long-running background processes started by `terminal` |
| `todo` | Track a plan for multi-step work |
| `memory` | Persist notes (`MEMORY.md`) and a user profile (`USER.md`) |
| `web_search` | Web search via Tavily |
| `web_extract` | Fetch + extract page text (SSRF-guarded) |
| `skills_list` / `skill_view` | Discover and load Agent Skills |
| `session_search` | Full-text search over past session history (FTS5) |
| `clarify` | Ask the user a structured clarifying question mid-turn |
| `lsp_diagnostics` / `lsp_definition` / `lsp_references` / `lsp_symbols` | Language-server-backed code intelligence — registered only when a matching server is configured (see [`docs/LSP.md`](docs/LSP.md)) |

Tools are grouped into toolsets (`file`, `terminal`, `web`, `coding`, `joey-cli`, …) and
resolved exactly like upstream, including recursive `includes`. `PreToolUse` hooks can
gate or rewrite any tool call before it runs — see [`docs/HOOKS.md`](docs/HOOKS.md).

## Configuration

State lives under `~/.joey/` (override with `JOEY_HOME`):

```
~/.joey/config.yaml     layered config (defaults ← config.yaml ← .env ← CLI flags)
~/.joey/.env            provider keys and secrets (overrides shell env, like upstream)
~/.joey/SOUL.md         the agent identity (seeded on first run; edit to customize)
~/.joey/state.db        SQLite session store (hermes-compatible schema + FTS5 search)
~/.joey/memories/       MEMORY.md, USER.md
~/.joey/skills/         installed Agent Skills (20 skills ship in-repo)
~/.joey/cron/jobs.json  scheduled jobs (hermes-compatible format)
~/.joey/logs/           size-rotated, secret-redacted logs
```

Config keys use dotted paths (`agent.max_turns`, `terminal.backend`, `model.provider`).
Keys ending in `_KEY`/`_TOKEN`/`_SECRET`/`_PASSWORD` are routed to `.env` automatically.

Preserved defaults from upstream: model unset until you pick one (`joey model` or the
first-run setup), OpenRouter base `https://openrouter.ai/api/v1`, `max_turns` 90,
reasoning left to the provider default, tool-output cap 50 000 chars (40/60 head-tail),
cron ticker 60 s.

## Architecture

A Cargo workspace of 13 crates. The first eight are direct ports of Hermes Agent
modules; the remaining five (`joey-tui`, `joey-llm-selector`, `joey-orchestration`,
`joey-omo`, `joey-speckit-ui`) are joey-native additions layered on top, not described
by the upstream Python project:

| Crate | Ports | Responsibility |
|-------|-------|----------------|
| `joey-core` | `hermes_constants`, `hermes_state`, config, logging, time | Branding, path/profile resolution, layered config, SQLite session store, redaction |
| `joey-providers` | `providers/`, `agent/transports/` | Provider profiles + registry, OpenAI/Anthropic wire adapters, SSE streaming, error classification |
| `joey-tools` | `tools/`, `toolsets.py` | Tool trait + registry, toolsets, schema sanitizer, fuzzy matcher, built-in tools |
| `joey-agent-core` | `run_agent.py`, `agent/conversation_loop.py`, `agent/prompt_builder.py` | The turn loop: message assembly, system prompt, tool dispatch, retries |
| `joey-cron` | `cron/` | Self-contained scheduler (duration/interval/cron), job store, ticker |
| `joey-mcp` | `tools/mcp_tool.py` (client) | Stdio JSON-RPC MCP client with the `mcp__server__tool` convention |
| `joey-gateway` | `gateway/` (core) | Session-key builder, message/adapter types, `PlatformAdapter` trait |
| `joey-cli` | `hermes_cli/`, `cli.py` | The `joey` binary: clap command tree + interactive REPL |
| `joey-tui` | — (joey-native) | The `--tui` animated ratatui dashboard: theme, widgets, input editor, app state |
| `joey-llm-selector` | — (joey-native) | Dynamic model allocator for `model.default = auto`: candidate pool, per-module allocation map, cold-start scoring, `/llm-selector` |
| `joey-orchestration` | — (joey-native) | Subagent manager + `delegate_task`/`call_omo_agent` tools for multi-agent delegation |
| `joey-omo` | — (joey-native) | "Oh My OpenAgent": 11-agent persona registry, category/subagent routing, Atlas plan execution, intent gating (ultrawork/hyperplan/team), goals, team mode |
| `joey-speckit-ui` | — (joey-native) | Standalone HTTP+WebSocket backend for the SpecKit Visual UI (`specs/<feature>/{spec,plan,tasks}.md`); run separately with `cargo run -p joey-speckit-ui`, not embedded in the `joey` binary |

`joey-tui`, `joey-llm-selector`, `joey-orchestration`, and `joey-omo` are all wired
into the live `joey` binary (REPL, one-shot, and cron paths); `joey-speckit-ui` is an
independent backend process for the separate `web/speckit-ui` frontend. See
[`docs/architecture.md`](docs/architecture.md) for the full dependency graph.

## Relationship to Hermes Agent

Joey Agent is a rewrite, not a fork of the Python — it shares no code, but it deliberately
matches Hermes's data formats and defaults so behavior is faithful. The `~/.joey/state.db`
schema, cron `jobs.json` shape, `SKILL.md` format, session-key grammar, and provider wire
payloads all follow upstream. See `PORTING.md` for the full mapping of what is complete,
partial, and deferred.

Hermes Agent is © Nous Research and MIT-licensed; Joey Agent retains that license and
attribution. This project is not affiliated with or endorsed by Nous Research.

One deliberate behavioral difference: upstream's Anthropic-OAuth path impersonates
Claude Code (spoofed client identity and headers) so subscription billing accepts its
traffic; Joey omits that layer as it circumvents Anthropic's terms. Use an Anthropic
API key instead. `PORTING.md` lists this and every other known deviation.

## License

MIT — see [LICENSE](LICENSE).
