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
joey                       Start the interactive TUI dashboard (default interface)
joey --cli                 Use the classic line-based REPL instead of the TUI
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
`/help`, `/usage`, `/copy`, `/version` (`/v`), `/quit` (`/exit`), and `/neurocode`
(always recognized; reports a disabled status unless the engine is on — see the
[NeuroCode](#neurocode-enterprise-java--pega-coding) section for the full subcommand
surface). Everything else in
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
| `neurocode_index` | Build/refresh the NeuroCode structural dependency graph of a project directory (tree-sitter multi-language parse → artifacts + edges persisted to `graph.db`). Only offered when `neurocode.enabled = true` |
| `neurocode_query` | Query the NeuroCode graph: `dependencies`, `dependents`, or FTS symbol search. Enabled-only, like all NeuroCode tools |
| `neurocode_status` | Engine status overview: enabled state, index size, tiers, patterns, domain sources |
| `neurocode_ingest` | Ingest a domain-knowledge source (file or directory) into the graph's FTS registry |

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
~/.joey/neurocode/      per-project NeuroCode graph databases (when enabled)
~/.joey/logs/           size-rotated, secret-redacted logs
```

Config keys use dotted paths (`agent.max_turns`, `terminal.backend`, `model.provider`).
Keys ending in `_KEY`/`_TOKEN`/`_SECRET`/`_PASSWORD` are routed to `.env` automatically.

Preserved defaults from upstream: model unset until you pick one (`joey model` or the
first-run setup), OpenRouter base `https://openrouter.ai/api/v1`, `max_turns` 90,
reasoning left to the provider default, tool-output cap 50 000 chars (40/60 head-tail),
cron ticker 60 s.

## NeuroCode: Enterprise Java & Pega Coding

NeuroCode (`crates/joey-neurocode/`, feature 015 — a joey-native addition with no
upstream equivalent) turns Joey into a coding agent specialized for **enterprise Java
and Pega Platform codebases**. When enabled, it:

1. **Classifies every coding request by complexity** and routes it between two model
   tiers — an *economical* tier for boilerplate work (getters, tests, DTOs, stubs)
   and a *frontier* tier for architectural work (refactors, concurrency, migrations).
   Classification is deterministic and O(1) — keyword, scope-fan-out, and graph-hub
   signals; no extra LLM call on the hot path.
2. **Maintains a structural dependency graph** of your project, parsed with
   tree-sitter (real AST parsing for every tree-sitter-supported programming
   language — Java, Python, JS/TS/TSX, Go, Rust, Ruby, PHP, C#, C, C++,
   Scala, Haskell, Julia, OCaml, Bash, Verilog, Agda — types, methods,
   fields, annotations, imports, injection points; heuristic fallback for
   the long tail), stored in a per-project SQLite database with FTS5/BM25
   symbol search. No Qdrant/vector server required.
3. **Assembles dependency-aware context** per request: when you edit
   `UserServiceImpl`, the `UserService` interface it implements and the
   `UserRepository` it injects are pulled into context automatically — no referenced
   type left absent. The economical tier gets a focused slice; the frontier tier gets
   the fuller graph.
4. **Understands the Pega Platform rule system** version-adaptively (auto-detects the
   Pega version from the Gradle BOM, or takes an explicit override), with built-in
   rule-type metadata (`Rule-Obj-*`, `Data-*`, `Work-*` families) and rule-to-rule
   references.
5. **Runs a build/verify feedback loop** (configurable steps, e.g. `mvn compile`)
   that records verified successes as *patterns* and recurring failures as
   *anti-patterns*, surfaced as warnings when you re-edit the same area.
6. **Ingests domain knowledge** — framework docs, entity catalogs, postmortems —
   from files or directories into the same searchable store.

**Off by default, byte-identical when disabled.** With `neurocode.enabled = false`
(the default), the engine is never invoked, no NeuroCode tools are offered to the
model, no messages are injected, and the system prompt is bit-for-bit identical to a
build without the feature.

### Enabling NeuroCode

Add the following to `~/.joey/config.yaml` (or use `joey config set` with the
dotted keys shown below):

```yaml
neurocode:
  enabled: true                                  # master switch (default: false)

  tier:
    economical:
      model: "openrouter/anthropic/claude-haiku" # any model id your provider serves
    frontier:
      model: "openrouter/anthropic/claude-opus"  # the heavyweight tier
    ambiguous_default: economical                # tier used when signals are tied

  verify:                                        # optional build/verify loop
    max_fix_iterations: 3                        # default: 3
    steps:
      - name: compile
        command: "mvn -q compile"
        parse: maven                              # plain | maven | compiler | checkstyle_xml
        timeout_sec: 120                         # default: 120

  classifier:                                    # optional classifier tuning
    scope_fanout_frontier_threshold: 4           # default: 4
    economical_keywords: []                      # empty = built-in defaults
    frontier_keywords: []                        # empty = built-in defaults

  pega:
    version: ""                                  # empty = auto-detect from Gradle BOM
```

Only `neurocode.enabled: true` is required to start; everything else falls back to
sane defaults. If a tier model is unset, the agent falls back to the configured
default model for that tier's requests.

Start the REPL from the root of the project you want indexed (the project root is
taken from the current working directory, and each project gets its own graph):

```bash
cd ~/work/my-enterprise-service
joey
```

### First-run workflow

```
/neurocode index          # parse the project, build the graph (~/.joey/neurocode/projects/<hash>/graph.db)
/neurocode status         # verify: artifact count, tiers, Pega version, pattern counts
```

After indexing, everything else is automatic: normal coding requests are classified,
routed to a tier, and dispatched with dependency-aware context prepended. You can
always inspect or override what the engine is doing with the subcommands below.

### The `/neurocode` command

Available in the line REPL and the `--tui` dashboard. Bare `/neurocode` (or
`/neurocode status`) shows the status overview. Subcommands:

```
/neurocode                               Status: enabled state, index size + last
                                         indexed time, tier models, Pega version,
                                         pattern/anti-pattern counts, domain sources,
                                         and any domain-knowledge conflicts.

/neurocode index [--force|-f]            (Re-)index the current project. Prints
                                         files scanned / artifacts / edges / errors.

/neurocode query symbol <name>           FTS symbol search — list artifacts (kind,
                                         FQCN, source path) matching a name.
/neurocode query definition <name>       Exact-match declaration lookup — kind,
                                         file, byte span, and captured
                                         signature (when indexed).
/neurocode query dependencies <FQCN>     Outgoing edges: what <FQCN> implements/
                                         injects/exchanges/references.
/neurocode query dependents <FQCN>       Incoming edges: everything that depends
                                         on <FQCN>. (alias: incoming;
                                          references also answers to this —
                                          where is it used)
                                         (dependencies also answers to outgoing;
                                          symbol also answers to fts)

/neurocode tier                          Show tier routing: mode (automatic or
                                         pinned), both tier models, ambiguous default.
/neurocode tier economical               Pin the economical tier for this session.
/neurocode tier frontier                 Pin the frontier tier for this session.
/neurocode tier auto                     Unpin — back to automatic classification.
/neurocode tier pin <economical|frontier>  Explicit pin form.
/neurocode tier unpin                    Same as `tier auto`.

/neurocode ingest <category> <path> [flags]
                                         Ingest domain knowledge. <path> is a file or
                                         a directory (capped: ≤32 files, ≤512 KiB,
                                         binary content skipped). Categories:
                                           FrameworkDocs   versioned framework docs
                                           EntityCatalog   entity/DTO schemas
                                           Postmortem      incident learnings
                                           PegaRuleType    Pega rule-type metadata
                                         Flags:
                                           --version <v>    e.g. "3.2", "infinity-24.2"
                                           --provenance <p> where it came from
                                                            (URL / doc ref / note);
                                                            defaults to the path

/neurocode patterns                      List learned patterns (verified successes:
                                         signature, tier, result, timestamp).
/neurocode anti-patterns                 List active anti-patterns (error signature,
                                         offending output, known resolution, hit
                                         count). Alias: antipatterns.

/neurocode domain list                   List ingested domain-knowledge sources
                                         (id, category, version, provenance, path).
/neurocode domain remove <id>            Remove a domain source by its numeric id.
                                         (aliases: rm, delete)

/neurocode --help | help | -h            Usage summary.
```

Unknown subcommands produce an error with a pointer to `--help`. Note that pins set
via `/neurocode tier …` last for the current session only; the tier models themselves
come from `config.yaml`.

Example session:

```
> /neurocode index
Indexing complete: 214 files scanned, 1,832 artifacts, 4,107 edges.
0 error(s).

> /neurocode query dependencies com.acme.user.UserServiceImpl
Dependencies of com.acme.user.UserServiceImpl (3):
  com.acme.user.UserServiceImpl --[Implements]--> com.acme.user.UserService
  com.acme.user.UserServiceImpl --[Injects]--> com.acme.user.UserRepository
  com.acme.user.UserServiceImpl --[ExchangesType]--> com.acme.user.UserDto

> /neurocode ingest FrameworkDocs ./docs/spring-boot-3.2 --version 3.2 --provenance "spring.io/docs"
Ingested FrameworkDocs source #1 from './docs/spring-boot-3.2' [category=FrameworkDocs].
```

### The four model-facing tools

When enabled, four NeuroCode tools are registered in the `coding` toolset so the
model itself can drive the engine mid-conversation (all of them return an error
notice rather than executing when the engine is disabled):

- **`neurocode_index`** — build or refresh the graph for a project path. Params:
  `path` (required), `force` (bool, default false). The model uses this after
  cloning or significantly changing a project, or when queries look stale.
- **`neurocode_query`** — graph queries. Params: `query_type` (`dependencies`,
  `dependents`, `definition`, or `references`), `symbol` (an FQCN or simple name,
  required), `limit` (default 20).
- **`neurocode_status`** — the same status overview as `/neurocode`.
- **`neurocode_ingest`** — ingest knowledge. Params: `category` (`pattern`,
  `antipattern`, `rule`, or `convention`), `path`, optional `version` and
  `provenance`.

### Where the data lives

```
~/.joey/neurocode/projects/<sha256-prefix-of-canonical-path>/graph.db
```

One SQLite database per indexed project (honours `JOEY_HOME`), shared across
profiles and across parent/subagent sessions — a delegated subagent querying the
graph reads the same `graph.db` the parent built, with no re-ingestion. The graph
schema (v1) stores code artifacts, typed edges (`Implements`, `IsImplementedBy`,
`Injects`, `ExchangesType`, `ReferencesRule`, `InheritsRule`), FTS indexes, learned
patterns and anti-patterns, and domain-knowledge sources with conflict detection
(overlapping category+version — newest source wins, and `/neurocode status` flags it).

### Behavior on non-Java projects

NeuroCode is a no-op on codebases with no Java/Pega artifacts: context assembly
detects the absence and returns an empty context with a notice, and ordinary
retrieval/generation proceed unmodified. You can leave `neurocode.enabled = true`
globally without affecting Rust/Python/JS projects.

### Design notes

- **SQLite + FTS5 instead of a vector DB** (deliberate deviation from the original
  design): the workspace already bundles SQLite with FTS5; symbol retrieval is
  BM25-ranked keyword search and the graph edges are exact typed traversals —
  nearest-neighbor search adds nothing here. Embedding-based retrieval is deferred.
- **tree-sitter + per-language grammar crates** is the feature's set of new
  external dependencies (~150-300 KB compiled each), chosen because
  deterministic syntax-aware parsing (generics, annotations, nested classes)
  cannot be done reliably with regexes. Covers every programming language
  with a grammar under the tree-sitter org; see
  `crates/joey-neurocode/src/parse/registry.rs`.
- Full design trail, constitution compliance, and every dependency decision:
  `specs/015-neurocode-enterprise-java/` (spec, plan, contracts, quickstart).

## Architecture

A Cargo workspace of 14 crates. The first eight are direct ports of Hermes Agent
modules; the remaining six (`joey-tui`, `joey-llm-selector`, `joey-orchestration`,
`joey-omo`, `joey-speckit-ui`, `joey-neurocode`) are joey-native additions layered
on top, not described by the upstream Python project:

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
| `joey-neurocode` | — (joey-native) | NeuroCode engine for enterprise codebases: complexity-tier routing, tree-sitter multi-language structural dependency graph in SQLite+FTS5 (all tree-sitter-supported languages; Pega-tuned for Java), dependency-aware context assembly, Pega rule awareness, verify-loop pattern memory. Consumed by `joey-agent-core` via the narrow `NeuroCodeEngine` trait; see the [NeuroCode](#neurocode-enterprise-java--pega-coding) section |

`joey-tui`, `joey-llm-selector`, `joey-orchestration`, and `joey-omo` are all wired
into the live `joey` binary (REPL, one-shot, and cron paths); `joey-neurocode` is
wired into the REPL and one-shot paths (engine injection + the four NeuroCode tools
+ `/neurocode`); `joey-speckit-ui` is an
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
