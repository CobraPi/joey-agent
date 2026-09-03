# joey-cli — the joey binary: commands, REPL, TUI

`joey-cli` builds the `joey` executable: the clap command tree, the pre-clap profile scanner, one-shot mode, the model picker, the config/tools/cron/mcp/skills/copilot/doctor subcommands, the reedline line REPL, and the ratatui TUI dashboard that is now the default interactive interface. It is the port of upstream's `hermes` CLI (`hermes_cli/_parser.py`, `hermes_cli/main.py`, `hermes_cli/cli.py`) and the place where every other workspace crate gets wired into a running agent: tools, sessions, orchestration, MCP, LSP, NeuroCode, RAG, OMO, hooks, and HyperCode.

> See also: [../cli.md](../cli.md)

## Overview

The crate is 44 files (43 Rust modules + `Cargo.toml`). `main.rs` declares the tree, applies the `-p/--profile` override before clap runs, selects the interface, and dispatches subcommands; `repl.rs` is the line-based interactive chat plus all agent-construction logic (`build_agent_parts`); `tui.rs` is the animated multi-panel dashboard (the default when stdin is a terminal). Bare `joey` starts the TUI; `joey --cli` (or `JOEY_TUI=0`) opts back into the classic line REPL; `joey chat` is the explicit interactive-chat subcommand, with `-q/--query` for a single programmatic query and `-Q/--quiet` for script-friendly output.

## Module map

| File | Purpose |
|---|---|
| `src/main.rs` | Entry point: `Cli` parser, epilogue, profile pre-parse, `JOEY_TUI`/flag resolution, dispatch, SIGPIPE reset |
| `src/repl.rs` | Line REPL: `ChatOptions`, `ReplState`, `build_agent_config`/`build_agent_parts`, `restore_history`, session resume, reedline editor, turn loop with Ctrl-C semantics, slash dispatch |
| `src/tui.rs` | Ratatui TUI dashboard (particle backdrop, live panels, slash dispatch, `handle_slash`) |
| `src/engine.rs` | TUI-side agent engine (turn driving, rebuild after force-kill) |
| `src/oneshot.rs` | `-z/--oneshot`: single prompt, final text only, `--usage-file` JSON report, exit-code mapping |
| `src/slash.rs` | Slash registry (`REGISTRY`, `CommandDef`), `lookup`, `resolve` with prefix expansion |
| `src/slash_extra.rs` | Shared slash handler implementations (save/undo/branch/snapshot/journey/…) used by REPL and TUI |
| `src/slash_menu.rs` | `SmartCompleter` + `SmartHinter` for the reedline Tab menu |
| `src/speckit_slash.rs` | The 12 `/speckit-*` lifecycle handlers |
| `src/speckit_cmd.rs` | `joey speckit`: spawn backend + Vite frontend |
| `src/cron_cmd.rs` | `joey cron` subcommand tree (list/create/pause/resume/run/remove/status/tick/edit/runs) |
| `src/config_cmd.rs` | `joey config` (show/edit/get/set/unset/path/env-path/check/migrate) |
| `src/tools_cmd.rs` | `joey tools` (`--summary`, list/enable/disable, per-platform toolsets) |
| `src/skills_cmd.rs` | `joey skills` (list/inspect/enable/disable/config; marketplace deferred) |
| `src/mcp_cmd.rs` | `joey mcp` (add/remove/list/test/configure/catalog; serve/picker/install/login/reauth deferred) |
| `src/copilot_cmd.rs` | `joey copilot` (install/list/remove/update/status), `.github/` discovery, prompt-file lookup |
| `src/auth_cmd.rs` | `joey auth copilot login\|status\|logout` |
| `src/doctor_cmd.rs` | `joey doctor` configuration/dependency checks |
| `src/model_catalog.rs` | Provider model catalogs for the picker |
| `src/llm_selector.rs` | Feature 011 allocator wiring + `/llm-selector` handler (CLI/REPL parity) |
| `src/neurocode_wiring.rs` | NeuroCode engine construction scoped to the live agent provider |
| `src/neurocode_rag_wiring.rs` | RAG refresh worker + pre-fetch injections (`neurocode.rag.enabled`) |
| `src/omo_resolver.rs` | OMO category resolver population (provider profile + active model) |
| `src/omo_render.rs` | OMO rendering helpers |
| `src/hypercode.rs` / `src/hypercode_gate.rs` | HyperCode orchestrator mode, context, overlay, gating |
| `src/mcp_tools.rs` | MCP server discovery + tool registration for chat sessions |
| `src/project_trust.rs` | Project trust scan + prompt + `TrustStore` |
| `src/setup_wizard.rs` | First-run guard and NeuroCode tier prompts |
| `src/profile.rs`, `src/history.rs`, `src/discover.rs`, `src/render.rs`, `src/markdown.rs`, `src/animation.rs`, `src/capability.rs`, `src/clipboard.rs`, `src/secret_prompt.rs`, `src/commands/mod.rs`, `src/commands/neurocode.rs`, `src/tests/*` | Support modules: profile utils, exit outro history, `joey discover` local-model scan, terminal rendering, markdown, animations, render-capability detection, clipboard, secret input prompts, version/info commands, tests |

## Installation & profiles

All state lives under `~/.joey/` (override with `JOEY_HOME`). Profiles give completely separate homes:

- `-p NAME` / `--profile NAME` / `--profile=NAME` is honored **before clap runs** — `apply_profile_override` scans raw argv, strips the flag pair, and sets `JOEY_HOME=<root>/profiles/<name>`. The scanner knows which flags consume values (`-z`, `-m`, `--provider`, `-t`, `-r`, `-s`, `--usage-file`, `--max-turns`, `-q`, …) so it never mistakes a flag's value for `-p`, and stops at `--` or inside `mcp add … --args <child argv>`.
- Profile names must match `[a-z0-9][a-z0-9_-]{0,63}` (lowercase letter or digit first, then lowercase/digits/`_`/`-`, max 64 chars). An invalid name fails the scan silently.
- A **sticky default** comes from `<root>/active_profile`; a stale sticky file degrades to the default home with a warning.
- An explicitly flagged profile whose directory does not exist hard-fails: `Error: Profile '<name>' does not exist…` and **exit 1**.
- An existing `JOEY_HOME` that already points inside a `profiles/` directory is trusted (no re-derivation).

## Command tree

### Global flags

| Flag | Meaning |
|---|---|
| `-V` / `--version` | Print version and exit (identical to `joey version`) |
| `-z` / `--oneshot PROMPT` | One-shot mode: print ONLY the final response text. No banner/spinner/tool previews/session line; tools and AGENTS.md load as normal; approvals auto-bypassed |
| `--usage-file PATH` | One-shot only: write a JSON usage report (tokens, model, api_calls) even when the run fails |
| `-m` / `--model` | Model override (e.g. `anthropic/claude-sonnet-4.6`); also `JOEY_INFERENCE_MODEL` in one-shot mode |
| `--provider` | Provider override; persistent provider lives in `model.provider` |
| `-t` / `--toolsets` | Comma-separated toolsets for this invocation |
| `-r` / `--resume SESSION` | Resume by session ID or title |
| `-c` / `--continue [SESSION_NAME]` | Resume by name, or the most recent session when omitted |
| `-s` / `--skills` | Preload skills (repeatable or comma-separated) |
| `--max-turns N` | Tool-calling iterations per turn (default 90 / `agent.max_turns`) |
| `--yolo` | Bypass dangerous-command approvals (sets `JOEY_YOLO_MODE=1`) |
| `--pass-session-id` | Include the session ID in the system prompt |
| `--ignore-user-config` | Ignore `~/.joey/config.yaml` (credentials in `.env` still load) |
| `--safe-mode` | Disable ALL customizations — user config and MCP servers (implies `--ignore-user-config`) |
| `--cli` | Force the line REPL (beats `--tui` and `JOEY_TUI`) |
| `--tui` | Force the animated TUI (matters only to beat `JOEY_TUI=0`) |

### Subcommands

| Command | Surface |
|---|---|
| `joey` (bare) | Interactive chat — TUI by default, `--cli`/`JOEY_TUI=0` for the line REPL |
| `joey chat` | Interactive chat with extras: `-q/--query` (single query, non-interactive), `-m`, `-t`, `-s`, `--provider`, `-v/--verbose`, `-Q/--quiet` (suppress banner/spinner/tool previews; still prints final response + session id), `-r`, `-c`, `--max-turns`, `--yolo`, `--pass-session-id`, `--ignore-user-config`, `--safe-mode`, `--cli`, `--tui`. Chat-level flags win over top-level ones |
| `joey model [--refresh]` | Provider + model picker wizard; `--refresh` clears cached catalogs first |
| `joey auth copilot login\|status\|logout` | GitHub Copilot OAuth device-code login, credential-source status, token removal (only `COPILOT_GITHUB_TOKEN`; `GH_TOKEN`/`GITHUB_TOKEN` are never touched) |
| `joey tools [--summary] list\|enable\|disable NAME… [--platform cli\|cron]` | Per-platform toolset configuration (`platform_toolsets.<platform>`); bare in a TTY prints list + hint (curses UI not ported); `post-setup` also recognized |
| `joey config show\|edit\|get KEY [--json]\|set KEY VAL [--force]\|unset KEY\|path\|env-path\|check\|migrate` | Layered config; env-shaped keys route to `.env`; missing key on `get` exits 1 with `Config key not set: <key>` |
| `joey doctor [--fix] [--ack ID]` | Health checks (config parse, `.env` permissions 600, directories, PATH, model/credentials); `--fix` auto-repairs |
| `joey version` | Same output as `-V` |
| `joey cron` | Full scheduler surface — see below |
| `joey mcp add NAME --url\|--command [--transport] [--connect-timeout] [--env KEY=VALUE…] [--args CHILD_ARGV…]` | Add an MCP server to `mcp_servers.*`; `--args` must be last (remainder belongs to the child) |
| `joey mcp remove\|rm NAME` / `list\|ls` / `test NAME` / `configure [NAME]` / `catalog` | Remove, list, connectivity-test, editor-jump, and the curated offline catalog of common servers |
| `joey mcp serve\|picker\|install\|login\|reauth` | Recognized but **deferred** (need upstream registry/marketplace or the gateway serve loop): explanatory message, exit 1 |
| `joey skills list [--enabled-only]` / `inspect NAME` / `enable NAME` / `disable NAME` / `config` | Local skill management (`skills.disabled` list); bare prints usage |
| `joey skills browse\|search\|install\|publish\|tap` (+ `repair-official`) | Marketplace subcommands — recognized but deferred, exit 1 |
| `joey copilot install SRC [--ref REF]` / `list` / `remove NAME` / `update [NAME]` / `status` | Manage GitHub-Copilot-style plugins (`~/.joey/copilot/plugins/<name>`); SRC is a git URL, `owner/repo`, or local path |
| `joey discover` | Scan for local model servers (Ollama, LM Studio, llama.cpp, …) |
| `joey home` | Print the resolved joey home directory (joey extension) |
| `joey llm-selector [args…]` | CLI mirror of `/llm-selector` (byte-identical output; trailing args forwarded: `status`, `pool`, `pin <module> <model>`, `allocations`, …) |
| `joey speckit [--port/-p 4173] [--repo-root DIR] [--open]` | Launch the SpecKit Visual UI: spawns the `joey-speckit-ui` backend plus the Vite dev server, waits on Ctrl+C |

`joey cron` subcommand detail:

| Subcommand | Flags / notes |
|---|---|
| (bare) / `list [--all]` | List jobs (`--all` includes disabled) |
| `create` (alias `add`) `SCHEDULE [PROMPT]` | `--name`, `--deliver` (`origin`/`local`/`platform:chat_id`), `--repeat N`, `--skill` (repeatable), `--skills` (comma list), `--script` (path under `~/.joey/scripts/`), `--workdir`, `--no-agent` (run the script, deliver stdout, skip the LLM) |
| `pause` / `resume` / `remove` (`rm`, `delete`) `JOB_ID` | Lifecycle actions |
| `run JOB_ID` | Trigger now (one synchronous scheduler tick) |
| `status` | Ticker heartbeat ages |
| `tick [--loop]` | Run due jobs once and exit; `--loop` is the standalone scheduler for hosts without a gateway |
| `edit [JOB]` | Open the `jobs.json` store in `$EDITOR` (snapshot → edit → validate → save) |
| `runs` / `history` [JOB] | Recent run outputs from the per-job `output/` directories |

### Exit codes

| Situation | Code |
|---|---|
| clap help / version | 0 |
| clap usage error | 2 |
| One-shot: run failed and produced no (non-whitespace) text | 2 |
| One-shot: `--provider` without `--model`/`JOEY_INFERENCE_MODEL`; all-invalid model | 2 |
| One-shot: empty response, not failed | 1 |
| One-shot: success (even failed-but-produced-text) | 0 |
| Any `run()` error (rendered via `render::error`) | 1 |
| Missing resume/continue target; `llm-selector` handler error | 1 |
| Unknown `mcp` or `skills` subcommand | 2 |
| Explicit profile directory missing (pre-clap) | 1 |
| Normal completion | 0 |

## REPL

The line REPL (`repl.rs`) is built on **reedline** with the Emacs keybindings:

- Prompt: `❯ ` rendered in Cyan (static color; a blink was descoped with reedline's blocking editor loop); multiline continuation indicator `… `; history-search indicators `(search) ` / `(failing search) `.
- History: `FileBackedHistory` capped at **10,000 lines**, persisted at `~/.joey/.joey_history`.
- `Alt+Enter` inserts a newline (multiline input); `Tab` opens/advances a reedline **description menu** of all slash names + aliases fed by `SmartCompleter` (with `SmartHinter` inline hints); arrow keys navigate, Enter accepts.
- **Idle Ctrl-C**: first press clears the buffer; a second press **within 2 seconds** exits (`(press Ctrl-C again to exit)`). `Ctrl-D` exits immediately.
- **Mid-turn Ctrl-C** (upstream cli.py:13640-13727 semantics): the turn runs under `tokio::select!` against `tokio::signal::ctrl_c()`. First press sets the agent's interrupt handle — `⚡ Interrupting agent... (press Ctrl+C again to force exit)`; a second press within 2 s prints `⚡ Force exiting...` and exits the process.
- `/queue <prompt>` (alias `/q`) parks a prompt that is **drained into the next turn's input** — it never interrupts the running one.
- Batch mode: when stdin is not a terminal, lines are read from the pipe without the line editor (session ends at EOF with reason `stdin_eof`).

## Slash commands

Resolution (`slash::resolve`) mirrors upstream prefix expansion exactly:

1. Exact name/alias match wins (`/help`, `/q` → `queue`).
2. Otherwise, prefix matching over all names **and** aliases: a **unique** prefix expands (`/hel` → `/help`, preserving the argument tail verbatim).
3. If several match, the **unique shortest** match wins (`/qui` → `/quit`).
4. Anything else is `Ambiguous` (sorted candidate list) or `Unknown`.

An `Unknown` `/x` is not dead: the REPL and TUI fall back to **Copilot prompt files** — `find_prompt_body(name, cwd)` looks in the project's `.github/prompts` and installed copilot plugins; a hit expands `/name [args]` into the prompt body (args appended) and runs it as a normal turn. Only then is `Unknown command: /x` printed.

Grouped catalog — every command in `REGISTRY` (all currently implemented):

**Session**

| Command | Aliases | Summary |
|---|---|---|
| `/new` | `reset` | Fresh session ID + history |
| `/clear` | | Clear screen, new session |
| `/redraw` | | Full UI repaint |
| `/history` | | Show conversation history |
| `/save` | | Export session as markdown to `~/.joey/saves/` |
| `/retry` | | Resend the last message |
| `/prompt` | `compose` | Compose next prompt in `$EDITOR` |
| `/undo [N]` | | Rewind N user exchanges and re-prompt |
| `/title [name]` | | Set session title |
| `/handoff <platform>` | | Hand the session to a messaging platform |
| `/branch` | `fork` | Fork the session |
| `/compress` | `compact` | Compress context (`here [N]`, `focus`, `--preview`) |
| `/rollback [n]` | | List/restore filesystem checkpoints |
| `/checkpoint` | `snap` | Create a filesystem checkpoint |
| `/snapshot` | | create/restore/prune config-state zips |
| `/stop` | | Kill background processes |
| `/background` | `bg`, `btw` | Run a prompt in the background |
| `/agents` | `tasks` | Active agents, tasks, OMO registry |
| `/journey` | `learning`, `memory-graph` | Learning-journey timeline |
| `/start-work [plan]` | | Activate Atlas on a `.omo/plans/` plan |
| `/queue` | `q` | Queue a prompt for the next turn |
| `/steer <prompt>` | | Inject after the next tool call |
| `/goal` | | set/pause/resume/clear/show standing goal |
| `/moa <prompt>` | | Mixture-of-Agents preset run |
| `/subgoal` | | Manage extra goal criteria |
| `/hypercode` | | status/run/toggle/configure pipelines |
| `/status` | | Session, model, token, context info |
| `/changes` | | Files changed this session with diffs |
| `/resume [name]` | | Resume a named session |
| `/sessions` | | Browse and resume previous sessions |

**Configuration**

| Command | Aliases | Summary |
|---|---|---|
| `/config` | | Show current configuration |
| `/model` | | Switch model / configure neurocode tiers |
| `/llm-selector` | | status/pool/enable/disable |
| `/neurocode` | | status/tier/index/query/search/backend/patterns/domain |
| `/codex-runtime` | `codex_runtime` | Toggle codex app-server runtime |
| `/personality [name]` | | Set a predefined personality |
| `/statusbar` | `sb` | Toggle the context/model status bar |
| `/timestamps` | `ts` | Toggle `[HH:MM]` timestamps |
| `/verbose` | | Cycle tool progress: off → new → all → verbose |
| `/footer` | | Toggle gateway metadata footer |
| `/yolo` | | Toggle approval bypass |
| `/reasoning` | | level/show/hide |
| `/fast` | | normal/fast/status |
| `/skin [name]` | | Show/change display skin |
| `/indicator` | | kaomoji/emoji/unicode/ascii busy style |
| `/voice` | | on/off/tts/status |
| `/busy` | | queue/steer/interrupt/status (Enter-while-working) |

**Tools & Skills**

| Command | Aliases | Summary |
|---|---|---|
| `/tools` | | list/disable/enable tools |
| `/toolsets` | | List available toolsets |
| `/skills` | | Search/install/inspect/manage skills |
| `/memory` | | pending/approve/reject/approval |
| `/bundles` | | List skill bundles |
| `/pet` | | toggle/list/scale/adopt a mascot |
| `/hatch` | `generate-pet` | Generate a petdex pet |
| `/learn` | | Learn a reusable skill |
| `/cron` | | Manage scheduled tasks |
| `/suggestions` | `suggest` | accept/dismiss/catalog automations |
| `/blueprint` | `bp` | Automations from templates |
| `/curator` | | Background skill maintenance |
| `/kanban` | | Multi-profile collaboration board |
| `/reload` | | Reload `.env` into the session |
| `/reload-mcp` | `reload_mcp` | Reload MCP servers from config |
| `/reload-skills` | `reload_skills` | Re-scan `~/.joey/skills/` |
| `/browser` | | connect/disconnect/status CDP |
| `/plugins` | | List installed plugins |
| `/copilot` | | status/list/install/remove/update of `.github/` extensions |

**Spec-Kit (12)**

`/speckit-constitution` · `/speckit-specify` · `/speckit-clarify` · `/speckit-plan` · `/speckit-checklist` · `/speckit-tasks` · `/speckit-analyze` · `/speckit-implement` · `/speckit-converge` · `/speckit-taskstoissues` · `/speckit-status` · `/speckit-help`

**Info**

| Command | Aliases | Summary |
|---|---|---|
| `/whoami` | | Slash access level (admin/user) |
| `/profile` | | Active profile name + home dir |
| `/help` | | Show available commands |
| `/usage` | | Token usage for this session |
| `/subscription` | `upgrade` | Plan view/change |
| `/topup` | | Balance/billing portal |
| `/insights [days]` | | Usage analytics |
| `/platforms` | `gateway` | Gateway/messaging platform status |
| `/copy [n]` | | Copy last response to clipboard |
| `/paste` | | Attach clipboard image |
| `/image <path>` | | Attach a local image |
| `/update` | | Update Joey Agent |
| `/version` | `v` | Show version |
| `/debug` | | nous/local debug report |

**Exit**: `/quit` (alias `/exit`).

## TUI vs REPL selection

`use_tui(cli_flag, tui_flag)` decides, highest precedence first:

1. `--cli` (or `chat --cli`) — force the **line REPL**; beats everything, so users/scripts can always recover the classic UI.
2. `--tui` — force the TUI (matters mainly to beat `JOEY_TUI=0`, or to render the dashboard for a `chat -q` query).
3. `JOEY_TUI` env: `0` or `false` (case-insensitive) → line REPL; anything else (or unset) → TUI.
4. Default: **TUI**.

Independently, the TUI falls back to the line REPL when stdio isn't a terminal (`IsTerminal` check), keeping pipes working without flags.

## Agent wiring

`run_chat` (and the TUI engine) assemble the agent in a fixed order:

1. `Config::load()` — layered YAML + env.
2. **First-run guard** (`first_run_guard`): model/credential setup on a fresh install, then config reload.
3. **Project-trust prompt**: `scan_project(&cwd)` detects project-local resources; when a trust prompt is required and the cwd isn't yet trusted, the user chooses `[y/N/session]` (`Trusted`/`SessionOnly`/`Untrusted`) and the `TrustStore` persists it.
4. **Session establish/resume**: `-r` resolves by session ID or title; `-c` by name or the most recent session; otherwise a new `cli` session row is created. Resumed sessions restore history via `restore_history`, which **rebuilds assistant `tool_calls`** (from the stored JSON) and tool results with their owning ids so provider replay stays protocol-valid; stored system messages are re-derived, not replayed.
5. **`build_agent_parts`**:
   - `build_agent_config` — `AgentConfig::from_config` + overrides (an explicit `--model` pins the model and auto-detects its provider when `--provider` is absent; `--toolsets` resolves via `resolve_toolsets`, else the platform toolsets for `cli`).
   - `ToolRegistry::with_builtins()`, then **session tools** (wired to the session DB), the **clarify** tool (interactive channel attached at runtime), **LSP tools** when `LspManager::from_joey_config` finds configured servers, and **MCP tools** from `mcp_servers` (their wire names extend `enabled_tools`).
   - **SubagentManager + `delegate_task`** (`register_orchestration_with_resolver_and_allocator`), with the startup purge of team dirs older than `hypercode.team.cleanup_days`.
   - **llm_selector allocator** (feature 011) threaded into `delegate_task` and installed on the parent agent.
   - **NeuroCode engine + tools** when enabled — built scoped to the agent's live `(provider, base_url, model)` triple; the 4 NeuroCode tools register, `neurocode_search` only when `neurocode.rag.enabled`; the engine is shared with the orchestration manager so children reuse the same graph.db; RAG refresh injections install on the agent.
   - **OMO resolver** populated after agent construction (provider profile + active model + user-defined custom categories).
   - **PreToolUse hooks** from config (crush-style), attached to the agent.
   - **HyperCode overlay**: when orchestrator mode is enabled the main agent becomes a pure orchestrator (`delegate_task` its only tool) with the delegation-only identity prompt as extra instructions.
6. Rendering options: quiet `-Q` suppresses the banner (and spinner/tool previews); `chat -v` verbose logging via `init_verbose`. Interactive `display.streaming` defaults **on** when unset. The exit outro prints session stats and the active profile's resume hint.

## Configuration & environment

Key config keys the CLI reads (dotted paths in `~/.joey/config.yaml`):

| Key | Default | Used for |
|---|---|---|
| `model.provider` | `zai` (flag default `auto` — detect from the model string) | provider selection |
| `model.default` | `glm-5.2` | default model |
| `model.base_url` | `""` | provider endpoint override |
| `model.context_length` | unset (`0`) | banner context-length display |
| `agent.max_turns` | `90` | tool-calling iterations per turn |
| `display.streaming` | `false` (interactive REPL forces **on** when unset) | token streaming |
| `display.show_reasoning` | `true` | render reasoning blocks |
| `display.tool_progress` | `all` | tool progress detail level |
| `display.animation_fps` | `0` (clamped 0–60; 0 → capability-based ≥12) | animation rate |
| `display.syntax_highlighting` | `true` | syntax highlighting |
| `terminal.backend` | `local` | terminal execution backend |
| `terminal.cwd` | `.` | session-persistent shell cwd |
| `terminal.timeout` | `180` | foreground command timeout (seconds) |
| `compression.threshold` | `0.50` | context-compression trigger ratio |
| `skills.disabled` | (empty) | comma-separated hidden skills |
| `hypercode.team.cleanup_days` | `7` | team-dir retention window |
| `neurocode.rag.enabled` | `false` | registers `neurocode_search` + RAG workers |
| `timezone` | `""` | wall-clock timezone for schedules |

Environment variables:

| Variable | Effect |
|---|---|
| `JOEY_HOME` | Root directory override (also how profiles relocate the home) |
| `JOEY_TUI` | `0`/`false` → line REPL; else TUI (below both flags) |
| `JOEY_YOLO_MODE` | Set by `--yolo`; read by joey-tools approvals |
| `JOEY_SAFE_MODE` | Set by `--safe-mode` (also sets `JOEY_IGNORE_USER_CONFIG`); read by joey-mcp |
| `JOEY_IGNORE_USER_CONFIG` | Set by `--ignore-user-config`/`--safe-mode`; read at `Config::load` |
| `JOEY_INFERENCE_MODEL` | One-shot-only model selection (with `-z`) |
| `COPILOT_GITHUB_TOKEN` | Copilot credential (written by `joey auth copilot login`) |

Any config key ending in `_KEY`, `_TOKEN`, `_SECRET`, or `_PASSWORD` is auto-routed to `~/.joey/.env` instead of `config.yaml` (both `config set` and the loader honor this).

## Testing

Tests live inline (`#[cfg(test)]` in `main.rs`, `repl.rs`, `slash.rs`, `slash_extra.rs`, `cron_cmd.rs`, `config_cmd.rs`, `mcp_cmd.rs`, `skills_cmd.rs`, `tools_cmd.rs`, `doctor_cmd.rs`, `copilot_cmd.rs`, `oneshot.rs`, `speckit_cmd.rs`, …) plus `src/tests/` (hypercode team) and `tests/responsiveness_probe.rs`. They pin: flag parsing (including "no invented flags" like a top-level `-q` or `--cwd`), the profile scanner's value-flag and `mcp add --args` skipping, TUI-default precedence in all flag/env combinations, slash resolution (exact/unique-prefix/shortest/ambiguous/unknown), the one-shot exit-code mapping, and honest deferral messages for unported subcommands. Run with: `cargo test -p joey-cli`.

## See also

- [../cli.md](../cli.md) — CLI subsystem overview
- [README.md](README.md) — the docs/features/ crate-by-crate index
- [joey-agent-core.md](joey-agent-core.md) — the turn loop and system prompt this CLI drives
- [joey-tools.md](joey-tools.md) — the tool registry, toolsets, and approvals
- [joey-tui.md](joey-tui.md) — the dashboard widget layer
- [joey-cron.md](joey-cron.md) — the scheduler behind `joey cron`
- [joey-mcp.md](joey-mcp.md) — the MCP client behind `joey mcp`
- [joey-gateway.md](joey-gateway.md) — session keys and platform identity
