# joey-core — branding, config, state, logging, redaction

`joey-core` is the shared foundation of the joey-agent workspace: it owns the brand constants (`joey`, `JOEY_*` env prefix, `~/.joey` home resolution with per-profile scoping), the layered YAML+env configuration system, the SQLite session store (Hermes-compatible `SCHEMA_VERSION = 22`), rotating file logging with pre-disk secret redaction, the timezone-aware clock, reasoning-effort parsing, the OAuth/auth JSON store, and the CharmTone theme palette. It is a port of the load-bearing core of upstream Hermes Agent's `hermes_constants.py`, `hermes_cli/config.py`, `hermes_state.py`, `hermes_logging.py`, `agent/redact.py`, and `hermes_time.py`, keeping on-disk formats byte-compatible.

> See also: [../state-and-config.md](../state-and-config.md), [joey-agent-core.md](joey-agent-core.md)

## Overview

`joey-core` sits at the very bottom of the workspace DAG: every other crate (`joey-providers`, `joey-tools`, `joey-agent-core`, `joey-cron`, `joey-mcp`, `joey-gateway`, `joey-cli`, `joey-tui`, …) depends on it, and it depends on none of them. Anything that must be process-global or shared — home/profile paths, config merge semantics, session persistence, log routing, redaction — lives here so higher crates never re-implement it. Everything is import-safe with no heavy init: constants, config, and paths resolve lazily.

## Module map

| File | Purpose |
|---|---|
| `src/lib.rs` | Crate root; `ensure_home()` skeleton + `SOUL.md` seeding; re-exports (`Config`, `joey_home`, `SessionDb`, …) |
| `src/branding.rs` | Single source of truth for the brand: names, env-var prefix, attribution |
| `src/constants.rs` | Path resolution (`joey_home`, profiles, packaged data dirs), WSL/container/Termux detection, subprocess `HOME` contract |
| `src/config.rs` | Layered config (`DEFAULT_CONFIG_YAML` ← `config.yaml` ← env), dotted-path get/set/unset, `.env` loader/writer, root-key normalization |
| `src/state.rs` | SQLite session store (`~/.joey/state.db`, `SCHEMA_VERSION = 22`), FTS5 search, compression locks/cooldowns |
| `src/logging.rs` | Rotating `agent.log`/`errors.log` tracing layers, TUI console suppression/mirror |
| `src/redact.rs` | Regex secret redaction (41 token-prefix patterns, ENV/JSON/YAML assignments, JWT, PEM, phones) |
| `src/time.rs` | Timezone-aware clock; `JOEY_TIMEZONE` → `timezone:` config → local |
| `src/utils.rs` | Atomic writes, token estimation, truncation, truthy-string/env bool helpers |
| `src/reasoning.rs` | Reasoning-effort parsing and per-model override resolution |
| `src/auth_store.rs` | `~/.joey/auth.json` persistence with flock + atomic 0o600 writes |
| `src/theme.rs` | CharmTone palette, `Theme::pantera()`, gradient/diagonal-field renderers |
| `src/default_soul.rs` | Default `SOUL.md` persona text and legacy-template detection |
| `tests/redact_quoted_regression.rs` | Integration regression for quoted-value redaction |

## Branding & constants

`branding.rs` centralizes every user-visible name so the brand never leaks piecemeal:

| Constant | Value | Meaning |
|---|---|---|
| `AGENT_NAME` | `"Joey Agent"` | Human-readable name |
| `PACKAGE_NAME` | `"joey-agent"` | Package/repo name |
| `CLI_NAME` | `"joey"` | Binary name |
| `ENV_PREFIX` | `"JOEY_"` | Env-var prefix |
| `HOME_DIR_NAME` | `".joey"` | Dot-directory under `$HOME` (POSIX) |
| `WINDOWS_DIR_NAME` | `"joey"` | Directory under `%LOCALAPPDATA%` on Windows |
| `TOOLSET_PREFIX` | `"joey-"` | Toolset name prefix (upstream: `hermes-*`) |
| `VERSION` | `CARGO_PKG_VERSION` | Kept in lockstep with the ported upstream baseline |
| `UPSTREAM_ATTRIBUTION` | `"Rust port of Hermes Agent by Nous Research (…, MIT)"` | Retained MIT attribution |

Env-var names defined in `branding.rs`:

| Constant | Env var | Purpose |
|---|---|---|
| `ENV_HOME` | `JOEY_HOME` | State-directory override |
| `ENV_REAL_HOME` | `JOEY_REAL_HOME` | Explicit OS-user home override for subprocesses |
| `ENV_LOG` | `JOEY_LOG` | Tracing filter (like `RUST_LOG`) |
| `ENV_OPTIONAL_SKILLS` | `JOEY_OPTIONAL_SKILLS` | Packaged optional-skills dir override |
| `ENV_BUNDLED_SKILLS` | `JOEY_BUNDLED_SKILLS` | Packaged bundled-skills dir override |
| `ENV_OPTIONAL_MCPS` | `JOEY_OPTIONAL_MCPS` | Packaged optional-mcps dir override |

Other env vars referenced across the crate: `JOEY_IGNORE_USER_CONFIG` (exact `"1"`), `JOEY_TIMEZONE`, `JOEY_REDACT_SECRETS`, `TERMINAL_HOME_MODE` / `TERMINAL_MAX_CONCURRENT` / `TERMINAL_SSH_*`, `OP_SERVICE_ACCOUNT_TOKEN`, `TERMUX_VERSION`/`PREFIX`, `KUBERNETES_SERVICE_HOST`.

`constants.rs` adds wire-level constants: `PARTIAL_STREAM_STUB_ID = "partial-stream-stub"` (response id for partial stream stubs during error recovery), `FINISH_REASON_LENGTH = "length"`, `OPENROUTER_BASE_URL = "https://openrouter.ai/api/v1"` and `OPENROUTER_MODELS_URL = "https://openrouter.ai/api/v1/models"`.

## Home directory & profiles

Resolution order for the joey home (`joey_home()`):

1. Process-local override (`set_home_override`, `HomeOverrideGuard` RAII) — used for per-profile scoping; upstream uses a Python `ContextVar`, the port scopes per process.
2. `JOEY_HOME` env var (trimmed, non-empty).
3. Platform default: `~/.joey` on POSIX; `%LOCALAPPDATA%\joey` (or `<home>\AppData\Local\joey`) on Windows.

If `JOEY_HOME` is unset while `<native home>/active_profile` names a non-default profile, a one-time stderr warning fires (the process would silently write to the DEFAULT profile). `process_joey_home()` ignores the override for machine-level assets; `default_root()` maps a custom `JOEY_HOME` back to the profile root (recognizing `<root>/profiles/<name>`).

`ensure_home()` creates the home plus exactly these subdirectories, each chmod `0o700`:

`cron`, `sessions`, `logs`, `logs/curator`, `memories`, `pairing`, `hooks`, `image_cache`, `audio_cache`, `skills`

It is memoized per path (`HOME_ENSURED`). On first run it seeds `SOUL.md` from `default_soul::DEFAULT_SOUL_MD` (then `0o600`); a user-customized `SOUL.md` is never touched, but a legacy empty Hermes template is upgraded in place. Guard: if the home path is `<…>/profiles/<name>` and does not exist, `ensure_home()` bails with "Named profile home does not exist" — profiles must be created explicitly (e.g. `joey profile create`) so deleted profiles don't resurrect as empty skeletons.

`secure_parent_dir(path)` chmods the parent to `0o700` but refuses `/` and paths with fewer than 3 components, so a misdirected env var can't brick a host.

Environment helpers:

| Helper | Detection |
|---|---|
| `is_termux()` | `TERMUX_VERSION` set, or `PREFIX` containing `com.termux/files/usr` |
| `is_wsl()` | `/proc/version` contains "microsoft" (cached per process) |
| `is_container()` | `/.dockerenv`, `/run/.containerenv`, `KUBERNETES_SERVICE_HOST`, cgroup v1 markers (`docker`, `podman`, `/lxc/`, `kubepods`, `containerd`, `crio`), or cgroup-v2 mountinfo markers (cached) |

WSL path helpers: `windows_path_to_wsl` (`C:\…` → `/mnt/c/…`), `wsl_unc_path_to_posix` (`\\wsl.localhost\<distro>\…` → POSIX), `translate_cwd_for_wsl_backend`.

Subprocess `HOME` contract: `get_real_home()` walks candidates (`JOEY_REAL_HOME`, `HOME`, unix passwd `getpwuid` home, `USERPROFILE`, `HOMEDRIVE`+`HOMEPATH`, `dirs::home_dir`), skipping the profile home (`<joey home>/home`); `get_subprocess_home()` applies `TERMINAL_HOME_MODE` (`auto` default, `real`, `profile`); `apply_subprocess_home_env()` sets `JOEY_REAL_HOME` and optionally rewrites `HOME` for children.

## Configuration system

Precedence chain (lowest → highest): `DEFAULT_CONFIG_YAML` ← `~/.joey/config.yaml` ← `${VAR}` expansion from the process env (after `~/.joey/.env` loads with override semantics). `Config::load()` first runs `load_joey_dotenv`, then reads `constants::config_path()`. `CONFIG_VERSION = 34` is written as `_config_version` on every save.

Corrupt-config behavior: a YAML parse failure never fails the load — the last-known-good merged config for that path (or pure defaults) is served, a stderr/tracing warning fires (once per file mtime/size), and the corrupt file is preserved as `config.yaml.corrupt.<YYYYMMDD-HHMMSS>.bak` (deduped by same-size sibling backups; symlinks never followed).

`JOEY_IGNORE_USER_CONFIG` is honored on exact match `"1"` only — the user document is treated as empty.

Root-key normalization (`normalize_root_model_keys`): stale root-level `provider`/`base_url`/`context_length` move into `model.*`; `api_base` (root or inside `model`) is aliased to `base_url`; the model id canonicalizes to `model.default` (with `model`/`name` as last-resort aliases, then dropped). A root-level `max_turns` moves under `agent.max_turns` before merging. Deep-merge rule: a YAML `null` overlaying a mapping default is ignored (an empty `section:` must not wipe the section); sequences replace wholesale.

Saves persist ONLY the user document plus `_config_version` — merged defaults never contaminate `config.yaml`. Writes go through `utils::atomic_yaml_write` and the file is tightened to `0o600`.

`.env` loading (`load_joey_dotenv`, port of `env_loader.py`):

- `~/.joey/.env` loads with OVERRIDE semantics — user values beat stale shell exports.
- `~/.joey/.op.env` loads after it WITHOUT override, only when `OP_SERVICE_ACCOUNT_TOKEN` isn't already set.
- A project `.env` is a dev fallback: fills missing values when the user env exists; overrides stale shell vars when it doesn't.
- Files are pre-sanitized: UTF-8 BOM strip, NUL strip, and splitting of concatenated `KEY=VALUE` lines (guarded so URL/query-string values never split); credential-suffixed values (`_API_KEY`, `_TOKEN`, `_SECRET`, `_KEY`) are scrubbed to ASCII with a once-per-key warning.

Env routing for `config set` (`is_env_config_key`): dotted keys NEVER route; a key routes to `.env` when it is in the fixed `ENV_API_KEYS` allowlist (27 names: `OPENROUTER_API_KEY`, `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, … `GITHUB_TOKEN`, `HONCHO_API_KEY`, plus `TERMINAL_SSH_HOST/USER/KEY`, `SUDO_PASSWORD`, gateway/browser/search keys), OR ends with `_API_KEY`/`_TOKEN`, OR starts with `TERMINAL_SSH`.

The env writer (`save_env_value`) validates names against `^[A-Za-z_][A-Za-z0-9_]*$`, enforces a denylist of names that influence subprocess execution — loader/linker (`LD_PRELOAD`, `LD_LIBRARY_PATH`, `DYLD_*`), Python (`PYTHONPATH`, `PYTHONHOME`, …), Node (`NODE_OPTIONS`, `NODE_PATH`), general (`PATH`, `SHELL`, `BROWSER`, `EDITOR`, `VISUAL`, `PAGER`), Git (`GIT_SSH_COMMAND`, …), and joey runtime location (`JOEY_HOME`, `JOEY_PROFILE`, `JOEY_CONFIG`, `JOEY_ENV`) — strips newlines, scrubs non-ASCII credentials, quotes values with special dotenv meaning, recognizes `export KEY=` lines, writes atomically preserving mode (0600 default), then updates the process env.

Set-time coercion (`coerce_set_value`) follows upstream guards: when the schema default at the path is a string, the value is NEVER coerced (enum members like `approvals.mode: "off"` must not become YAML booleans); bools come only from the word sets `true/yes/on` and `false/no/off`; ints via `isdigit` (no negatives/exponents); floats only when a single `.` is present.

`${VAR}` expansion (`expand_env_vars`) rewrites every string value from the process env; unresolved references are kept verbatim. Example: `base_url: "${MY_GATEWAY}/v1"` becomes the env value at load time.

## Session store

`state.rs` persists to a single SQLite file, `~/.joey/state.db` (`SessionDb::open_default`), with `SCHEMA_VERSION: i64 = 22` — the exact upstream schema, so a hermes-created `state.db` opens unchanged. Tables:

| Table | Columns | Purpose |
|---|---|---|
| `sessions` | 46 | One row per session: ids/keys, model, system prompt snapshot, token/cost counters, handoff, compression cooldown/streak, `profile_name`, `rewind_count`, `archived` |
| `messages` | 21 | Transcript rows: role/content/tool fields, reasoning variants, `observed`, `active` (soft-delete), `compacted`, `api_content` |
| `session_model_usage` | 18 | Per-(session, model, billing, task) usage/cost rollup |
| `state_meta` | 2 | Generic key/value |
| `gateway_routing` | 4 | (scope, session_key) routing entries |
| `compression_locks` | 4 | Cross-process compression leases (holder, acquired/expires) |
| `async_delegations` | 18 | Delegated subagent tasks and delivery state |

Migrations are a declarative column reconciler: expected columns are extracted by executing `SCHEMA_SQL` into an in-memory reference DB, then any missing column is `ALTER TABLE … ADD COLUMN`-ed — old joey/hermes databases upgrade in place on open. Deferred indexes (referencing reconciler-added columns) and a unique partial index on `sessions(title)` are ensured after reconciliation; NULL `active` rows are healed to `1` on startup.

Full-text search: standalone FTS5 table `messages_fts` (content = `content + tool_name + tool_calls`) kept in sync by insert/delete/update triggers, plus a trigram twin `messages_fts_trigram` for CJK/substring search when the SQLite build has the trigram tokenizer (probed; falls back gracefully). Legacy external-content FTS shapes are dropped and backfilled. `search()` joins active-or-compacted messages and builds snippets with the upstream `'>>>' '<<<' '...' 40` parameters; user input passes through `sanitize_fts5_query` (preserve balanced quoted phrases, strip `[+{}():\"^]`, collapse `*` runs, drop dangling `AND/OR/NOT`, quote dotted/hyphenated terms) capped at `MAX_FTS5_QUERY_CHARS = 2048`.

Write contention: WAL journal mode (falling back to DELETE on filesystems that can't support it), `busy_timeout` of 1s, `foreign_keys=ON`, and `execute_write` wrapping every mutation in `BEGIN IMMEDIATE` with jittered retry — up to 15 attempts sleeping 20–150ms each. Every 50th successful write runs `PRAGMA wal_checkpoint(PASSIVE)`; every 1000th runs an FTS `optimize` merge.

Session ids (`new_session_id`) use the upstream shape `YYYYMMDD_HHMMSS_<hex6>` from a UUID v4 simple string — 22 characters total, stamped with the naive server-local clock (not the configured timezone).

Rewind (`rewind_last_user_exchanges(n)`) is soft-archive semantics: the last `n` active user messages and everything after the first of them are set `active = 0` (never destroyed), and session counters refresh to active totals. `archive_and_compact` similarly soft-archives all active messages (`active = 0, compacted = 1`) and inserts the compacted replacement rows in one transaction. `usage_over_days(days)` aggregates sessions/messages/total/assistant tokens across all sessions for `/insights`. Compression state helpers persist and query failure cooldowns, fallback streaks, and refreshable/expiring compression locks.

Session id resolution (`resolve_session_id`) matches an exact id first, then a `LIKE` prefix with `\`, `%`, `_` escaped — a prefix resolves only when it is unique (top-2 ordering by `started_at DESC`). `list_sessions` excludes archived rows; `most_recent_session` serves `--continue`; `update_system_prompt` stores the assembled prompt snapshot on the session row; `add_message` bumps `tool_call_count` by the number of entries in the message's `tool_calls` JSON array (not for tool-role result rows).

## Logging

`logging.rs` mirrors upstream file policy under `~/.joey/logs`:

| File | Rotation | Level |
|---|---|---|
| `agent.log` | `logging.max_size_mb` (default 5 MB) × `logging.backup_count` (default 3), cascade `agent.log.1..N` | `logging.level` (default `INFO`) |
| `errors.log` | 2 MB × 2 backups | WARNING and above |
| `tui-console.log` | append-only mirror | console layer output while the TUI owns the terminal |

Line format is the upstream `_LOG_FORMAT` shape: `<asctime,millis> <LEVEL>< [session-id]> <target>: <message> <fields>` (Python level names, so `tracing` WARN renders as `WARNING`). Every formatted line passes through `redact::redact_sensitive_text` before it reaches disk (the `RedactingFormatter` port). Console output is opt-in only (`init_verbose`); `init` installs just the file layers. The `EnvFilter` consults `JOEY_LOG` first, then `RUST_LOG`, else passes everything through.

While the ratatui TUI owns the terminal (`set_console_suppressed(true)`), the console `ConsoleWriter` route flips from stderr to appending ANSI-stripped text to `logs/tui-console.log` — never stderr, never a panic; write errors are dropped. The session tag is thread-local (`set_session_context`).

## Secret redaction

`redact.rs` ports upstream `agent/redact.py`. The kill switch `JOEY_REDACT_SECRETS` (bridged from `security.redact_secrets`, default on) is snapshotted at first use so runtime env mutations can't disable redaction mid-session.

41 known token-prefix patterns catch vendor credentials — e.g. `sk-…` (OpenAI/OpenRouter/Anthropic), `ghp_`/`github_pat_`/`gho_`/`ghu_`/`ghs_`/`ghr_` (GitHub), `xox[baprs]-`/`xapp-` (Slack), `AIza…` (Google), `AKIA…` (AWS), `sk_live_`/`sk_test_`/`rk_live_` (Stripe), `SG.…` (SendGrid), `hf_`, `r8_`, `npm_`, `pypi-`, `dop_v1_`, `tvly-`, `exa_`, `gsk_`, `syt_`, `mem0_`, `xai-`, `ntn_`, `fw-`/`fw_`/`fpk_`, and more — gated by a cheap literal-substring pre-screen before the combined regex runs.

Mask rules: `mask_token` keeps the first 6 and last 4 chars with `…` between; tokens under 18 chars become `***`. For file-read content, prefix-matched credentials use a NON-reusable sentinel (`«redacted:ghp_…»`) so an agent can't write a truncated mask back as a "real" key. `mask_secret_default` (display contexts) keeps head 4 / tail 4 with a 12-char floor.

Additional passes: ENV assignments (`OPENAI_API_KEY=…`, quoted values masked with quotes intact, programmatic lookups like `os.getenv`/`process.env` exempt), lowercase/dotted config keys, YAML `password: …` assignments, JSON fields (`"apiKey": "…"`), `Authorization`/`Proxy-Authorization` headers of any scheme, API-key headers (`x-api-key`, `x-goog-api-key`, …), Telegram bot tokens (`bot<digits>:<token>`), PEM `-----BEGIN … PRIVATE KEY-----` blocks, DB connection-string passwords, bare-token URLs, JWTs (`eyJ…`), and E.164 phone numbers (head/tail kept, middle starred).

URL query-param and `user:pass@` userinfo redaction is OFF by default (magic-link/OAuth callbacks must survive) and opt-in via `RedactOptions.redact_url_credentials` for non-navigation egress boundaries; the sensitive query-param names are a fixed case-insensitive list (`access_token`, `refresh_token`, `id_token`, `token`, `api_key`, `apikey`, `client_secret`, `password`, `auth`, `jwt`, `session`, `secret`, `key`, `code`, `signature`, `x-amz-signature`). `RedactOptions { force, code_file, file_read, redact_url_credentials }` tunes the passes: `code_file` skips ENV/JSON passes (source code), `file_read` implies `code_file` and switches prefix hits to the non-reusable sentinel, `force` redacts even with the kill switch off. `redact_terminal_output` additionally detects env-dump commands (`env`, `printenv`, `set`, `export`, `declare`) as the first token of any pipeline/sequence segment. `redact_secrets_par` fans out on the rayon pool for texts ≥ 512 KiB (`REDACT_PAR_CHUNK = 512 * 1024`), splitting on line boundaries (every pattern is line-local, so results are byte-identical); below the threshold the sequential path runs.

## Time & timezone

Resolution order for the configured zone: `JOEY_TIMEZONE` env var → `timezone:` key in `~/.joey/config.yaml` (raw read, no full config merge) → server-local time. The resolved `chrono_tz::Tz` is cached once per process (`reset_cache()` forces re-resolution); invalid strings warn and fall back — the clock never panics. `now()` returns a fixed-offset datetime in the configured zone; `now_iso()` formats Python `datetime.isoformat()` shape: `YYYY-MM-DDTHH:MM:SS.ffffff+HH:MM` (6-digit microseconds, colon offset).

## Utilities

- `atomic_replace(path, bytes)` — temp file in the destination dir, fsync before rename, symlink targets resolved so the symlink survives, permission bits and unix uid/gid preserved, `EXDEV`/`EBUSY` copy+fsync+unlink fallback; `atomic_json_write` (2-space indent, no trailing newline) and `atomic_yaml_write` ride on it.
- `estimate_tokens(text)` — `(chars + 3) // 4` ceiling division, so short non-empty text never estimates as 0.
- `truncate_middle` / `truncate_tail` — char-safe truncation with elision markers.
- `TRUTHY_STRINGS = ["1", "true", "yes", "on"]`; `is_truthy_str`, `env_bool` (upstream `getenv(key, "")` semantics — an unset var is false regardless of the default parameter), `env_var_enabled` (string default), `parse_bool`, `env_int`, `env_float`.
- `base_url_hostname` — lowercase hostname extraction (trailing FQDN dot stripped) for provider detection.
- `pretty_json_for_display` — pretty JSON whose embedded newlines render as real lines (TUI expandable views).
- `get_nested`/`set_nested`/`unset_nested` (also exported from `config.rs`) — dotted-path navigation with numeric list indexing; dict segments create intermediates on demand, list segments must already exist; `unset` prunes empty dict containers.

## Reasoning effort

`VALID_EFFORTS = ["minimal", "low", "medium", "high", "xhigh", "max", "ultra"]` (ascending capability). `ReasoningConfig` is either `Disabled` (thinking explicitly off) or `Effort(level)`. YAML `false` and the disabled-string set exactly `{"none", "false", "disabled"}` disable; `true`/null/empty/unrecognized (including the strings `"off"`/`"no"`) return `None` — the caller uses the provider default.

`resolve(cfg, model)` implements the upstream priority: (1) a per-model override from `agent.reasoning_overrides`, matched with spelling tolerance via `model_variants` (exact input, dot/dash cross-substitution, version-dot recovery, provider/aggregator prefix stripping and re-prepending against 12 known providers and 5 aggregators); (2) the global `agent.reasoning_effort`, whose raw value passes through so a YAML `false` means "disabled", never silently re-enabled. When `model` is empty it is derived from the config's `model` section.

## Auth store

`auth.json` under the active joey home, shaped `{"version": 1, "providers": {<id>: {…}}, "active_provider": …, "updated_at": "<iso8601>"}` (`AUTH_STORE_VERSION = 1`). Writes stamp `version` + `updated_at`, then persist atomically: temp file created with mode `0o600` (closing the TOCTOU window), fsync, rename, and a final `0o600` chmod; the parent dir is tightened via `secure_parent_dir`. Cross-process coordination is an advisory flock on `auth.lock` with a bounded 15s wait (100ms retry loop), degrading to unlocked operation — the write itself is still atomic. An unparseable store is preserved as `auth.json.corrupt` and replaced with an empty store rather than failing the caller. `deactivate_provider()` clears `active_provider` without deleting credentials.

## Theming

`theme.rs` ports the CharmTone palette verbatim from `charmbracelet/x/exp/charmtone`: 48 spectrum colors warm-to-cool plus `BUTTER` and 12 neutrals (61 named `Rgb` constants), e.g. `CHARPLE = 0x6B50FF`, `DOLLY = 0xFF60FF`, `MALIBU = 0x00A4FF`, `JULEP = 0x00FFB2`, `PEPPER = 0x201F26`. `Theme::pantera()` (Crush's default dark theme) maps them onto semantic tokens: 4 brand colors (`primary` = CHARPLE, `secondary`, `accent`, `keyword`), 4 foregrounds, `on_primary`, 4 backgrounds, `separator`, and 12 status colors (`destructive`, `error`, `warning*`, `denied`, `busy`, `info*`, `success*`). Helpers: `Rgb::from_hex`/`lerp`/`ansi`, per-grapheme `gradient_fg`/`gradient_fg_bold`, flowing multi-line `gradient_vertical`, and the signature `╱` `diagonal_field`/`gradient_diagonal_field` decorations, plus `paint`/`paint_bold`/`paint_dim`.

## Default configuration reference

Every key in `DEFAULT_CONFIG_YAML` (the embedded defaults; user keys merge on top and survive saves):

| Key | Default | Meaning |
|---|---|---|
| `model.default` | `"glm-5.2"` | Effective default model |
| `model.provider` | `"zai"` | Default provider id |
| `model.base_url` | `""` | Provider base-url override |
| `model.image_model` | `""` | Dedicated image-capable model; unset = provider default → primary if vision-capable (per-provider: `providers.<id>.image_model`) |
| `agent.max_turns` | `90` | Max turns per agent run |
| `agent.reasoning_overrides` | `{}` | Per-model reasoning-effort overrides |
| `agent.api_max_retries` | `3` | Provider API retry count |
| `agent.gateway_timeout` | `1800` | Gateway turn timeout (seconds) |
| `terminal.backend` | `"local"` | Terminal backend |
| `terminal.cwd` | `"."` | Working directory for commands |
| `terminal.timeout` | `180` | Command timeout (seconds) |
| `terminal.max_concurrent` | `auto` | Max concurrent agent commands; `auto` = clamp(CPU cores, 4, 16); `TERMINAL_MAX_CONCURRENT` overrides |
| `toolsets` | `["joey-cli"]` | Enabled toolset list |
| `compression.enabled` | `true` | Context compression on/off |
| `compression.threshold` | `0.50` | Context-fullness fraction triggering compression |
| `compression.target_ratio` | `0.20` | Target post-compression ratio |
| `compression.protect_last_n` | `20` | Recent messages never summarized |
| `compression.protect_first_n` | `3` | Leading messages never summarized |
| `compression.hygiene_hard_message_limit` | `5000` | Hard message-count ceiling |
| `compression.abort_on_summary_failure` | `false` | Abort turn vs fallback on summary failure |
| `auxiliary.compression.provider` | `"auto"` | Summarizer provider |
| `auxiliary.compression.model` | `""` | Summarizer model |
| `auxiliary.compression.base_url` | `""` | Summarizer base URL |
| `auxiliary.compression.api_key` | `""` | Summarizer API key |
| `auxiliary.compression.timeout` | `120` | Summarizer timeout (seconds) |
| `auxiliary.compression.extra_body` | `{}` | Extra request body |
| `auxiliary.compression.reasoning_effort` | `""` | Summarizer reasoning effort |
| `prompt_caching.cache_ttl` | `"5m"` | Provider prompt-cache TTL annotation |
| `memory.memory_enabled` | `true` | Memory system on/off |
| `memory.user_profile_enabled` | `true` | User-profile memory on/off |
| `memory.memory_char_limit` | `2200` | Max chars of injected memories |
| `memory.user_char_limit` | `1375` | Max chars of user-profile memory |
| `memory.nudge_interval` | `10` | Turns between memory nudges |
| `skills.creation_nudge_interval` | `10` | Turns between skill-creation nudges |
| `skills.external_dirs` | `[]` | Extra skill directories |
| `delegation.max_iterations` | `50` | Subagent delegation iteration cap |
| `delegation.max_concurrent_children` | `auto` | Concurrent subagent cap |
| `delegation.max_spawn_depth` | `1` | Subagent spawn-depth cap |
| `delegation.subagent_recovery_attempts` | `1` | Recovery attempts per subagent |
| `code_execution.mode` | `"project"` | Code-execution sandbox mode |
| `display.compact` | `false` | Compact output |
| `display.tool_progress` | `"all"` | Tool-progress display mode |
| `display.show_reasoning` | `true` | Show reasoning blocks |
| `display.streaming` | `false` | Stream output |
| `display.timestamps` | `false` | Show timestamps |
| `display.skin` | `"default"` | UI skin |
| `tool_output.max_bytes` | `50000` | Tool output byte cap |
| `tool_output.max_lines` | `2000` | Tool output line cap |
| `tool_output.max_line_length` | `2000` | Per-line char cap |
| `file_read_max_chars` | `100000` | File-read character cap |
| `approvals.mode` | `"smart"` | Approval policy |
| `approvals.timeout` | `60` | Approval prompt timeout (seconds) |
| `approvals.cron_mode` | `"deny"` | Approval policy for cron-originated actions |
| `approvals.deny` | `[]` | Always-denied tool list |
| `security.redact_secrets` | `true` | Secret redaction kill switch (bridged to `JOEY_REDACT_SECRETS`) |
| `logging.level` | `"INFO"` | agent.log level |
| `logging.max_size_mb` | `5` | agent.log rotation size |
| `logging.backup_count` | `3` | agent.log backups |
| `timezone` | `""` | IANA timezone (empty = server-local) |
| `cron.provider` | `""` | Model used by cron jobs |
| `neurocode.enterprise_context.enabled` | `true` | NeuroCode enterprise-context injection |
| `hypercode.enabled` | `false` | Hypercode orchestration off by default |
| `hypercode.max_workstreams` | `0` | Workstream cap |
| `hypercode.child_tool_delay` | `0.0` | Delay between child tool calls |
| `hypercode.explorer` / `hypercode.implementor` | `{}` | Role-specific overrides |
| `hypercode.reviewer.enabled` | `false` | Risk-review subagent off by default; high-risk graphs record a notice-and-proceed instead |
| `hypercode.reviewer` | `{}` | Reviewer per-provider overrides (model falls back to implementor → explorer → parent) |
| `hypercode.team.enabled` | `false` | Team orchestration off |
| `hypercode.team.lead_model` | `""` | Team lead model |
| `hypercode.team.max_members` | `8` | Max team members |
| `hypercode.team.max_parallel_members` | `4` | Max parallel members |
| `hypercode.team.message_limit` | `10` | Message budget per member |
| `hypercode.team.poll_interval_ms` | `500` | Poll interval |
| `hypercode.team.cleanup_days` | `7` | Cleanup age |
| `hypercode.execution_graph.enabled` | `true` | Execution-graph scheduling on |
| `hypercode.execution_graph.max_concurrent_workers` | `16` | Worker cap |
| `hypercode.execution_graph.max_repair_attempts` | `3` | Repair attempts per node |
| `_config_version` | `34` | Schema version stamp written on save |

## Testing

- `lib.rs` — home skeleton/SOUL.md seeding, customized-soul preservation, legacy-template upgrade, named-profile guard.
- `constants.rs` — WSL path conversions, lexical home normalization, passwd-home presence, override-guard restore.
- `config.rs` — defaults vs upstream, dotenv override/export/scrub semantics, `${VAR}` expansion, save-writes-only-user-keys, env-routing predicate table, coercion guards, root model normalization, corrupt-config backup, env-writer quoting/denylist, `JOEY_IGNORE_USER_CONFIG` exact-`"1"`, dotted/list get/set/unset pruning.
- `state.rs` — schema init/reconciliation, rewind soft-archive, `archive_and_compact`, `sanitize_fts5_query` shape table, usage aggregation, session-id resolution.
- `redact.rs` + `tests/redact_quoted_regression.rs` — pattern coverage, quoted-value regression, >512 KiB parallel-redaction fixture.
- `logging.rs` — TUI console suppression routing and ANSI stripping.
- `time.rs` — ISO format shape (6-digit micros, colon offset).
- `utils.rs` — `env_bool` table, token-estimate ceiling, atomic write roundtrips (incl. symlink preservation), display JSON.
- `reasoning.rs` — effort parsing (incl. `"off"`/`"no"` NOT disabling), variant table, override-then-global resolution, model derivation.
- `auth_store.rs` — missing/corrupt stores, provider-state round-trip, owner-only permissions.
- `theme.rs` — hex roundtrip, lerp endpoints, gradient rendering, diagonal field width.

## See also

- [README.md](README.md) — feature-doc index
- [joey-agent-core.md](joey-agent-core.md) — the turn loop that consumes this crate
- [joey-providers.md](joey-providers.md), [joey-tools.md](joey-tools.md) — next layers up the DAG
- [../state-and-config.md](../state-and-config.md) — user-facing config/state guide
- [../security.md](../security.md) — sanitization/threat-scan layers
- [../architecture.md](../architecture.md) — workspace overview
