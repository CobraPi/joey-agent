# joey CLI — Complete User-Facing Surface

Verified against source (`crates/joey-cli/src/`, debug binary `--help` output).

## 1. Top-level invocation

`joey [OPTIONS] [COMMAND]` — bare `joey` starts the line REPL (first-run guard may launch the setup wizard). SIGPIPE is reset to default so `| head` works.

### Global flags (main.rs)
- `-V, --version` — print version + install dir/method + upstream attribution, exit 0.
- `-z, --oneshot <PROMPT>` — one-shot: prints ONLY final answer text (no banner/spinner/session line); tools & AGENTS.md loaded; approvals auto-bypassed.
- `--usage-file <PATH>` — with `-z` only: write JSON usage report (tokens, model, api_calls) even on failure.
- `-m, --model <MODEL>` — per-invocation model override (e.g. `anthropic/claude-sonnet-4.6`). Also `JOEY_INFERENCE_MODEL` (one-shot only).
- `--provider <PROVIDER>` — per-invocation provider override (persistent one lives in `model.provider`).
- `-t, --toolsets <TOOLSETS>` — comma-separated toolsets for this run.
- `-r, --resume <SESSION>` — resume by ID or title.
- `-c, --continue [<SESSION_NAME>]` — resume by name; no value = most recent.
- `-s, --skills <SKILLS>` — preload skills (repeatable or comma-separated).
- `--max-turns <N>` — tool-calling iterations per turn (default 90 / `agent.max_turns`).
- `--yolo` — bypass dangerous-command approvals (sets JOEY_YOLO_MODE=1).
- `--pass-session-id` — include session ID in system prompt.
- `--ignore-user-config` — ignore config.yaml (creds in .env still loaded).
- `--safe-mode` — disable user config + MCP (implies --ignore-user-config).
- `--tui` — animated ratatui dashboard instead of line REPL. `JOEY_TUI=1|true` enables implicitly; falls back to line REPL when stdio isn't a terminal.
- `-p, --profile <NAME>` / `--profile=NAME` — NOT a clap flag: pre-argparse scan strips it and sets `JOEY_HOME=<root>/profiles/<name>`. Name: 1–64 chars, `[a-z0-9_-]`, must start lowercase/digit. Missing profile (explicit flag) → error exit 1. Falls back to sticky `<root>/active_profile` file. Child `--profile` inside `mcp add --args …` is respected as the MCP server's own flag.

### Exit codes (main.rs)
- 0 success / clap help+version
- 1 runtime error (`render::error` path) or command-specific failure (config get missing key, oneshot no output, doctor --ack, deferred subcommands, unknown profile…)
- 2 clap usage errors; oneshot: toolset validation failure, or failed run with no response text
- Oneshot: failed w/ no text → 2; no text at all → 1; otherwise 0.

## 2. Subcommands

- **chat** — interactive chat; mirrors top-level flags plus `-q, --query <TEXT>` (single query, non-interactive), `-Q, --quiet` (suppress banner/spinner/tool previews), `-v, --verbose`. Chat-level flags win over top-level ones.
- **model [--refresh]** — TTY-only interactive provider+model wizard (`require_tty`, exit 1 if piped). `--refresh` clears cached catalogs.
- **auth copilot login|status|logout** — GitHub Copilot OAuth device-code login (5 min), show credential source, remove Joey-owned `COPILOT_GITHUB_TOKEN` (never touches GH_TOKEN/GITHUB_TOKEN).
- **tools** — `--summary`; `list [--platform cli|cron]`; `enable|disable <NAME…> [--platform P]`. Writes `platform_toolsets.<platform>`. Known platforms: cli, cron. MCP `name:tool` toggling rejected. Bare TTY `joey tools` prints list + hint; non-TTY keeps upstream TTY-required error.
- **config** — `show` (default; grouped config display w/ masked secrets), `edit` ($EDITOR > $VISUAL > nano/vim/vi/code), `get <key> [--json]`, `set <key> <value> [--force]`, `unset <key>`, `path`, `env-path`, `check`/`migrate` (stubbed: "not available yet", exit 1). Env-shaped keys (`*_KEY/_TOKEN/_SECRET/_PASSWORD`) route to `.env`; secret leaf values masked on echo.
- **doctor [--fix] [--ack ID]** (ack stubbed) — checks config.yaml parses, .env permissions (600, auto-tightens with --fix), dirs (home/skills/logs, --fix creates), joey on PATH, model configured, provider credentials (incl. copilot token), external tools (git, rg, bash-required, node, docker), API connectivity (TCP probe w/ offline detection), toolset enablement, skills, profiles. Ends with `Found N issue(s)` / `All checks passed! 🎉`.
- **version** — identical to `-V`.
- **cron** — bare/list [--all]; `create|add SCHEDULE [PROMPT] [--name] [--deliver origin|local|platform:chat_id] [--repeat N] [--skill s]… [--skills a,b] [--script path] [--workdir path] [--no-agent]`; `pause|resume|remove|rm|delete <job_id>`; `run <job_id>` (trigger + one synchronous tick); `status` (ticker heartbeat health); `tick [--loop]` (once / 60s standalone scheduler daemon). edit/runs/history stubbed. Lists warn when scheduler isn't running.
- **mcp** — `add <name> (--url URL [--transport T] | --command CMD [--env K=V]… [--args arg…]) [--connect-timeout S]`; `remove|rm <name>`; `list|ls` (table); `test <name>` (connect + list_tools, timing). Entry passes security validation; suspicious configs refused. serve/catalog/picker/install/login/reauth/configure stubbed (exit 1); unknown → exit 2.
- **skills** — bare prints usage; `list [--enabled-only]` (Name/Category/Source/Status table). All other upstream subcommands (browse, search, install, … 26 total) recognized but stubbed exit 1.
- **discover** — probes local servers (Ollama :11434, LM Studio :1234, llama.cpp :8080, LiteLLM :4000, MLX :1234/*) and lists their models.
- **home** — prints resolved home dir (joey extension).
- **llm-selector [args…]** — CLI mirror of `/llm-selector`: status | pool | allocations | diagnostics | pin <module> <model> | unpin <module> | budget | diagnoser | enable | disable | refresh | help.
- **speckit [-p port] [--repo-root DIR] [--open]** — spawns joey-speckit-ui backend (default 4173) + Vite frontend (web/speckit-ui), waits on Ctrl+C.

## 3. REPL slash commands (repl.rs + slash.rs registry, prefix expansion + Tab completion menu)

Smart completions (Hermes parity, shared engine in `joey-tools::completion`):
Tab description menu for slash names/aliases and first-argument subcommands
(`/timestamps <Tab>` → on/off/status), @-context refs + fuzzy project file
search (`@query`), path completions (any `./`-style word), and fish-style
ghost-text hints (slash-name/subcommand remainder, history fallback).

**Implemented (works):**
- `/quit` (`/exit`) — exit. `/help` — grouped help incl. not-yet-implemented commands.
- `/new [name]` (`/reset`) — fresh session. `/clear` — clear screen+scrollback, new session.
- `/queue <prompt>` (`/q`; bare lists queue) — queue for next turn.
- `/steer <message>` — inject mid-turn after the next tool call, no
  interrupt (full mid-turn steering works in the TUI; in the line REPL it
  degrades to a queued message since input is read between turns).
- `/model [name] [--global]` — switch model session-scoped, or persist `model.default`.
- `/reasoning [none|minimal|low|medium|high|xhigh|max|ultra|on|off|show|hide] [--global]` — effort/display.
- `/config [get <k> | set <k> <v> | path]` (bare = show).
- `/status` — session/model/tokens/context. `/changes` — files changed this session w/ diffs.
- `/usage` — token usage. `/history` — last 30 messages (timestamps per /timestamps). `/sessions` — 10 recent. `/resume <id-or-title>`.
- `/compress [here [N] | focus topic | --preview|--dry-run]` (`/compact`) — manual context compression.
- `/checkpoint [msg]` (`/snap`); `/rollback [number]` (`/revert`) — filesystem checkpoints.
- `/tools` (list enabled), `/toolsets` (list all), `/skills` (installed list).
- `/copy [n]` — copy last response to clipboard.
- `/verbose` — cycle tool-progress off→new→all→verbose. `/timestamps [on|off|status]` (`/ts`).
- `/version` (`/v`). `/agents` (`/tasks`, `/agent`) — OMO agent registry. `/goal set|pause|resume|clear|show`. `/start-work [plan]`.
- `/llm-selector …` (subcommands as above). `/neurocode status|tier|index|query|patterns|anti-patterns|domain|ingest|help`.
  `/neurocode ingest` also accepts natural language — a free-text
  description hands off to an agent turn that locates (or writes) the
  source and calls `neurocode_ingest` (REPL and TUI; the strict
  `<category> <path>` form is unchanged).
- **Spec-Kit workflow** (full lifecycle, runs the real `.specify/` scripts
  for pre-flight + the bundled `speckit-*` skill workflow as one agent
  turn that authors the artifacts):
  `/speckit-constitution` · `/speckit-specify <description>` (scaffolds the
  feature branch + authors spec.md; re-runs update in place) ·
  `/speckit-clarify` · `/speckit-plan` · `/speckit-checklist` ·
  `/speckit-tasks` · `/speckit-analyze` · `/speckit-implement` ·
  `/speckit-converge` · `/speckit-taskstoissues` · `/speckit-status`
  (artifact readiness, no turn) · `/speckit-help`. Requires a `.specify/`
  directory in the repo (error explains how to init otherwise).

**Registered but "not available in joey-agent yet":** /redraw /save /retry /prompt /undo /title /handoff /branch /snapshot /stop /background /journey /moa /subgoal /whoami /profile /codex-runtime /personality /statusbar /footer /yolo /fast /skin /indicator /voice /busy /memory /bundles /pet /hatch /learn /cron /suggestions /blueprint /curator /kanban /reload /reload-mcp /reload-skills /browser /plugins /subscription /topup /insights /platforms /paste /image /update /debug.

Prefix resolution: exact match → unique prefix → unique-shortest (so `/qui`→/quit, `/q`→/queue). Ambiguous → "Did you mean" list. Unknown → "Unknown command".

## 4. Setup wizard / onboarding (setup_wizard.rs, commands/mod.rs)

- First-run guard before chat: if no provider configured (no provider key, no OPENAI_BASE_URL, no model+base_url, no Copilot token) → prompt "Run setup now? [Y/n]"; non-TTY prints env-var/config guidance and exits 1.
- `joey model` wizard (numbered-list UI, port of upstream fallback path): provider picker in canonical order (nous, openrouter, anthropic, openai-api, copilot, ai-usage-hud, gemini, deepseek, xai, zai) + saved `custom_providers` rows + "Custom endpoint" + optional "Remove a saved custom provider". Then credential prompt (masked entry; Keep/Replace/Clear when a key exists), Z.AI endpoint selection, model selection (models.dev catalog → curated fallback → live /models probe), persists config. Deliberately omitted: Anthropic-OAuth subscription login, curses UI, Gemini free-tier probe, secret-manager plugins, auxiliary-models submenu.

## 5. Profiles

`-p/--profile` (pre-parse), sticky `active_profile` file, homes at `~/.joey/profiles/<name>`, `joey home` prints resolved dir. Creation hint: `joey profile create <name>` is only referenced in an error message — there is no `profile` subcommand in the tree (creation is implicit via the directory/JOEY_HOME).

## 6. README verification

Root README.md command table matches the code, with two caveats: (a) README line "joey skills # Search, install, inspect, and manage skills" overstates — only `skills list` is implemented (code prints "(only 'list' is available in joey-agent so far)"); (b) README says "joey auth <provider>" — code only supports `auth copilot`. Everything else (flags, cron, mcp, speckit, discover, llm-selector, doctor, home, version) verified accurate.
