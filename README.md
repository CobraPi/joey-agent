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
joey model                              # see the resolved model/provider + whether a key is set
joey config set OPENROUTER_API_KEY sk-… # store a provider key (goes to ~/.joey/.env)
joey                                    # start an interactive chat session
joey -q "what changed in the last commit?"   # one-shot, prints only the final answer
```

Pick any provider with no code changes:

```bash
joey config set model.provider anthropic
joey config set model.default anthropic/claude-opus-4.6
joey config set ANTHROPIC_API_KEY sk-ant-…
```

Supported provider wire protocols out of the box: **OpenAI Chat Completions** (OpenRouter,
OpenAI, Nous, DeepSeek, Groq, Gemini, xAI, Z.ai, Ollama, and any custom OpenAI-compatible
endpoint) and **Anthropic Messages** (native, with extended thinking). SSE streaming is
supported on both.

## Commands

```
joey                       Start the interactive REPL
joey -q "<prompt>"         One-shot headless query (prints only the final answer)
joey -m <model>            Override the model for this run
joey -r <session-prefix>   Resume a past session

joey model                 Show the resolved model/provider and credential status
joey tools                 List toolsets and built-in tools
joey skills                List installed skills
joey config get|set|show|path <key> [value]
joey doctor                Diagnose the environment
joey version               Show version + upstream attribution
joey home                  Print the resolved ~/.joey directory

joey cron list                         List scheduled jobs
joey cron create "<sched>" "<prompt>"  Create a job ("30m", "every 2h", "0 9 * * *", ISO)
joey cron pause|resume|remove <id>     Manage a job
joey cron tick                         Run all due jobs once
joey cron run                          Run the 60s scheduler loop

joey mcp list <command> [args…]        Connect to a stdio MCP server and list its tools
```

Inside the REPL, slash commands mirror the CLI: `/help`, `/new` (`/reset`, `/clear`),
`/model`, `/tools`, `/toolsets`, `/skills`, `/reasoning <level>`, `/version`, `/quit`.

## Built-in tools

| Tool | What it does |
|------|--------------|
| `read_file` | Read a text file with line numbers + pagination |
| `write_file` | Create/overwrite a file (atomic) |
| `patch` | Targeted find/replace with a 9-strategy fuzzy matcher |
| `search_files` | Regex content search / glob file search (gitignore-aware) |
| `terminal` | Run a shell command (head/tail-bounded output, secret-redacted) |
| `todo` | Track a plan for multi-step work |
| `memory` | Persist notes (`MEMORY.md`) and a user profile (`USER.md`) |
| `web_search` | Web search via Tavily |
| `web_extract` | Fetch + extract page text (SSRF-guarded) |
| `skills_list` / `skill_view` | Discover and load Agent Skills |

Tools are grouped into toolsets (`file`, `terminal`, `web`, `coding`, `joey-cli`, …) and
resolved exactly like upstream, including recursive `includes`.

## Configuration

State lives under `~/.joey/` (override with `JOEY_HOME`):

```
~/.joey/config.yaml     layered config (defaults ← config.yaml ← .env ← CLI flags)
~/.joey/.env            provider keys and secrets
~/.joey/state.db        SQLite session store (transcripts + FTS5 search)
~/.joey/memories/       MEMORY.md, USER.md
~/.joey/skills/         installed Agent Skills
~/.joey/cron/jobs.json  scheduled jobs
~/.joey/logs/           rotating logs
```

Config keys use dotted paths (`agent.max_turns`, `terminal.backend`, `model.provider`).
Keys ending in `_KEY`/`_TOKEN`/`_SECRET`/`_PASSWORD` are routed to `.env` automatically.

Preserved defaults from upstream: default model `anthropic/claude-opus-4.6`, OpenRouter
base `https://openrouter.ai/api/v1`, `max_turns` 60, reasoning `medium`, tool-output cap
50 000 chars (40/60 head-tail), cron ticker 60 s.

## Architecture

A Cargo workspace of focused crates:

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

## Relationship to Hermes Agent

Joey Agent is a rewrite, not a fork of the Python — it shares no code, but it deliberately
matches Hermes's data formats and defaults so behavior is faithful. The `~/.joey/state.db`
schema, cron `jobs.json` shape, `SKILL.md` format, session-key grammar, and provider wire
payloads all follow upstream. See `PORTING.md` for the full mapping of what is complete,
partial, and deferred.

Hermes Agent is © Nous Research and MIT-licensed; Joey Agent retains that license and
attribution. This project is not affiliated with or endorsed by Nous Research.

## License

MIT — see [LICENSE](LICENSE).
