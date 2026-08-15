# joey-core: Configuration, State, Profiles, and Security

Foundation crate for joey-agent (Rust port of Hermes Agent). Owns branding,
path/profile resolution, layered config, timezone clock, logging, secret
redaction, reasoning-effort parsing, and the SQLite session store.

Modules: `branding`, `constants` (paths/env), `config`, `state` (SQLite),
`redact`, `logging`, `auth_store`, `reasoning`, `theme`, `time`, `utils`,
`default_soul`.

---

## 1. Layered Configuration (`config.rs`)

Resolution order (lowest → highest precedence):

1. **Embedded defaults** — `DEFAULT_CONFIG_YAML` (compile-time constant).
2. **`~/.joey/config.yaml`** — the user document, deep-merged on top of
   defaults. Missing/empty/non-mapping file = `{}`. `JOEY_IGNORE_USER_CONFIG=1`
   (exactly `"1"`) skips the file entirely.
3. **`${VAR}` expansion** — after merge, every string value is expanded from
   the process environment (loaded from `~/.joey/.env` first, with OVERRIDE
   semantics — .env values beat stale shell exports). Unresolved `${VAR}`
   references are kept verbatim.

Key behaviors:

- **Dotted-path keys** — reads and writes use dotted paths
  (`terminal.backend`, `agent.max_turns`); numeric segments index into lists
  (`custom_providers.0.name`).
- **`save` writes ONLY the user document** + `_config_version: 33` — the
  merged defaults tree never contaminates config.yaml. Atomic write, then
  chmod 0600.
- **Parse failures never fail the load**: last-known-good merged config (or
  defaults) is served, a stderr warning fires once per file mtime/size, and
  the corrupt file is backed up as `config.yaml.corrupt.<timestamp>.bak`.
- **Set-time normalizations** (ports of upstream config.py):
  - Root-level `max_turns` moves under `agent.max_turns`.
  - Root-level `provider` / `base_url` / `context_length` / `api_base` move
    under `model:`; `api_base` is an alias for `base_url`; model id
    canonicalizes to `model.default` (aliases `model.model`, `model.name`).
  - Value coercion: bool words (`true/false/yes/no/on/off`), digit-only
    ints (no negatives/exponents — `-3` stays a string), floats via
    single-dot parse; keys typed string in the schema are never coerced.
- **In-memory model override**: `set_model_override` (per-invocation
  `--model`) mutates only the merged view, never disk.

### `.env` loading (`load_joey_dotenv`)

Order: `~/.joey/.env` (override — beats shell env) → `~/.joey/.op.env`
(fill-only, bootstrap `OP_SERVICE_ACCOUNT_TOKEN` only) → optional project env
file (fill-only when user .env was loaded, override when it wasn't).
Before parsing, `.env` files are sanitized: UTF-8 BOM strip, NUL strip,
and **concatenated-line splitting** (`KEY1=vKEY2=v2` on one line is split at
known `KEY=` needles; structured values like URLs are never split). After
loading, credential values with non-ASCII characters (copy-paste artifacts)
are stripped with a warning.

### Credential routing to `.env`

`config set <key>` routes to `~/.joey/.env` instead of config.yaml when
`is_env_config_key(key)` is true:

- Key is in the allowlist (`ENV_API_KEYS`): OPENROUTER_API_KEY,
  OPENAI_API_KEY, ANTHROPIC_API_KEY, VOICE_TOOLS_OPENAI_KEY, EXA_API_KEY,
  PARALLEL_API_KEY, FIRECRAWL_API_KEY, FIRECRAWL_API_URL,
  FIRECRAWL_GATEWAY_URL, TOOL_GATEWAY_DOMAIN, TOOL_GATEWAY_SCHEME,
  TOOL_GATEWAY_USER_TOKEN, TAVILY_API_KEY, BROWSERBASE_API_KEY,
  BROWSERBASE_PROJECT_ID, BROWSER_USE_API_KEY, FAL_KEY, TELEGRAM_BOT_TOKEN,
  DISCORD_BOT_TOKEN, TERMINAL_SSH_HOST, TERMINAL_SSH_USER, TERMINAL_SSH_KEY,
  SUDO_PASSWORD, SLACK_BOT_TOKEN, SLACK_APP_TOKEN, GITHUB_TOKEN,
  HONCHO_API_KEY
- OR ends with `_API_KEY` / `_TOKEN` (any case)
- OR starts with `TERMINAL_SSH`

Dotted keys NEVER route. The env writer validates names
(`^[A-Za-z_][A-Za-z0-9_]*$`), enforces a **denylist** (LD_PRELOAD,
LD_LIBRARY_PATH, PYTHONPATH, NODE_OPTIONS, PATH, SHELL, EDITOR, BROWSER,
GIT_SSH_COMMAND, ..., plus JOEY_HOME / JOEY_PROFILE / JOEY_CONFIG / JOEY_ENV),
strips newlines, quotes values with special dotenv characters, writes
atomically preserving mode (0600 default), and updates the process env.

---

## 2. Meaningful Config Keys

Keys actually read in code (defaults in the embedded config):

**model**: `model.default` (default "glm-5.2"), `model.provider` ("zai"),
`model.base_url`, `model.context_length`, `model.api_mode`,
`model_catalog.excluded_providers`, `model.selector.enabled`,
`model.selector.budget`, `model.selector.diagnoser_model`
(`model` may be a bare string or mapping; `model.default` canonical).

**agent**: `agent.max_turns` (90), `agent.api_max_retries` (3),
`agent.gateway_timeout` (1800), `agent.reasoning_effort`,
`agent.reasoning_overrides`, `agent.tool_delay`,
`agent.disabled_toolsets`, `agent.environment_hint`,
`agent.parallel_tool_call_guidance`, `agent.task_completion_guidance`.

**terminal**: `terminal.backend` ("local"), `terminal.cwd` ("."),
`terminal.timeout` (180).

**toolsets**: `toolsets` (list; default `["joey-cli"]`).

**compression**: `compression.enabled` (true), `compression.threshold`
(0.50), `compression.target_ratio` (0.20), `compression.protect_last_n` (20),
`compression.protect_first_n` (3), `compression.hard_message_limit` (5000,
key `hygiene_hard_message_limit`), `compression.abort_on_summary_failure`
(false). Auxiliary compressor: `auxiliary.compression.provider` ("auto"),
`.model`, `.base_url`, `.api_key`, `.timeout` (120), `.reasoning_effort`.

**prompt_caching**: `prompt_caching.cache_ttl` ("5m").

**memory**: `memory.memory_enabled` (true), `memory.user_profile_enabled`
(true), `memory.memory_char_limit` (2200), `memory.user_char_limit` (1375),
`memory.nudge_interval` (10).

**skills**: `skills.creation_nudge_interval` (10), `skills.external_dirs`,
`skills.disabled`.

**delegation**: `delegation.max_iterations` (50),
`delegation.max_concurrent_children` (3), `delegation.max_spawn_depth` (1),
`delegation.default_model`, `delegation.default_max_turns`,
`delegation.default_persist`.

**code_execution**: `code_execution.mode` ("project").

**display**: `display.compact` (false), `display.tool_progress` ("all"),
`display.show_reasoning` (true), `display.streaming` (false),
`display.timestamps` (false), `display.skin` ("default"),
`display.animation_fps`, `display.syntax_highlighting`.

**tool_output**: `tool_output.max_bytes` (50000), `max_lines` (2000),
`max_line_length` (2000). Related: `file_read_max_chars` (100000, root key),
`context_file_max_chars` (root), `web.extract_char_limit`.

**approvals**: `approvals.mode` ("smart"), `approvals.timeout` (60),
`approvals.cron_mode` ("deny"), `approvals.deny` (list).

**security**: `security.redact_secrets` (true), `security.allow_private_urls`,
`browser.allow_private_urls`.

**logging**: `logging.level` ("INFO"), `logging.max_size_mb` (5),
`logging.backup_count` (3).

**cron**: `cron.provider` (""), `cron.output_retention`.

**timezone**: `timezone` ("" — empty means system-local).

**neurocode** (tiered routing): `neurocode.enabled`,
`neurocode.classifier.economical_keywords`, `.frontier_keywords`,
`neurocode.tier.frontier.model`, `.economical.model`,
`.ambiguous_default`, `neurocode.verify.max_fix_iterations`,
`neurocode.pega.version`.

**mcp**: `mcp_servers` (mapping of server configs; also project-level
`.joey/mcp.json` / `.mcp.json`).

Config schema version: `_config_version: 33` written on save.

---

## 3. `~/.joey` Directory Layout

Resolved via `constants::joey_home()`; override chain:
process-local override (profiles) → `JOEY_HOME` env → platform default
(`~/.joey` POSIX, `%LOCALAPPDATA%\joey` Windows).

```
~/.joey/
  config.yaml          layered config (user keys only)
  .env                 credentials (0600)
  .op.env              optional 1Password bootstrap token
  state.db             SQLite session store (schema v22)
  SOUL.md              persona file (seeded on first run; legacy template
                       upgraded in place; user edits never touched)
  auth.json            provider auth state (0600, atomic writes)
  auth.lock            advisory flock for auth.json
  active_profile       sticky profile name (at ROOT, not per-profile)
  skills/              user skills
  optional-skills/     packaged optional skills (env/packaging overrides)
  optional-mcps/       approved packaged MCP servers
  cron/                jobs.json + .jobs.lock + output/<job>/... .md files
  sessions/            (skeleton; state lives in state.db)
  logs/                agent.log, errors.log, curator/, mcp-stderr.log
  memories/            persistent memory
  pairing/             gateway pairing
  hooks/               hooks
  image_cache/ audio_cache/
  profiles/<name>/     full per-profile homes (see §5)
```

`ensure_home()` creates this skeleton with 0700 dirs / 0600 SOUL.md, refuses
to silently create a missing named-profile home, memoizes per process.

---

## 4. Session Store (`state.rs`)

Single SQLite file `~/.joey/state.db`, `SCHEMA_VERSION = 22` — byte-for-byte
upstream (hermes) schema; a renamed `~/.hermes` home opens unchanged. Old
joey/hermes DBs upgrade in place via a **declarative column reconciler**
(live tables diffed against SCHEMA_SQL, missing columns ADDed).

Tables:

- `schema_version` (version)
- `sessions` (46 cols): id (shape `YYYYMMDD_HHMMSS_hex6`, 22 chars), source,
  user_id/session_key/chat_id/chat_type/thread_id (gateway routing),
  display_name, origin_json, model, model_config, system_prompt (full
  assembled prompt snapshot), parent_session_id, started_at/ended_at/
  end_reason, message_count, tool_call_count, token counters
  (input/output/cache_read/cache_write/reasoning), cwd, git_branch,
  git_repo_root, billing_* + estimated/actual_cost_usd + cost_status/
  cost_source/pricing_version, title (unique index), api_call_count,
  handoff_state/platform/error (cross-platform session handoff),
  compression_failure_cooldown_until/error, compression_fallback_streak,
  profile_name, rewind_count, archived, expiry_finalized.
- `messages` (21 cols): role (system/user/assistant/tool), content,
  tool_call_id/tool_calls (OpenAI JSON)/tool_name, timestamp, token_count,
  finish_reason, reasoning (+ reasoning_content/details, codex_* items),
  platform_message_id, observed, **active** (compaction soft-archive flag),
  compacted, api_content, effect_disposition.
- `session_model_usage` (18 cols): per (session, model, billing tuple, task)
  usage/cost rollups.
- `state_meta` (key/value), `gateway_routing` (scope+session_key → entry),
  `compression_locks` (session_id, holder, acquired/expires — TTL lease),
  `async_delegations` (background child tasks + delivery state).
- FTS5: `messages_fts` (inline content) + optional `messages_fts_trigram`
  (CJK substring; tokenizer-availability dependent), maintained by insert/
  delete/update triggers. Legacy external-content FTS is dropped and
  backfilled.

Operational details:

- WAL journal mode with DELETE fallback; FK enforcement ON; 1s busy timeout
  + app-level jittered retries (15 × 20–150ms); periodic TRUNCATE
  checkpoint / optimize every 50 / 1000 writes.
- **Resume**: `resolve_session_id` accepts a full id or unambiguous prefix
  (`_` escaped in LIKE); `most_recent_session`, `list_sessions`,
  `messages(sid)` returns the ACTIVE (non-compacted) transcript.
- **Compaction** is non-destructive: `archive_and_compact` sets
  `active=0, compacted=1` on old rows and inserts the summary as new active
  rows in one transaction; `session_was_rotated_by_compression` detects
  rotation (ended_at + end_reason="compression").
- Full-text search with `sanitize_fts5_query` (2048-char cap, phrase
  protection, special stripping, dotted-term quoting) so hostile input never
  errors FTS5.

---

## 5. Profiles

- `-p/--profile <name>` is intercepted in `main.rs` BEFORE clap: sets
  `JOEY_HOME=<root>/profiles/<name>` and strips the flag. `<root>` is
  `default_root()` (native home; a custom JOEY_HOME outside it is its own
  root, minus the `profiles/<name>` tail).
- Fallback: sticky `<root>/active_profile` file (name, or "default").
  Missing profile via the explicit flag = hard error; via sticky file =
  warning + default home.
- Each profile is a **full independent home** (own config.yaml, .env,
  state.db, skills, logs, auth.json) — profile scoping is just home-path
  scoping.
- `ensure_home()` refuses to auto-create a missing named profile home
  (deleted profiles stay deleted); profiles are created explicitly
  (`joey profile create`).
- If `JOEY_HOME` is unset while `active_profile` names a non-default
  profile, a one-time stderr warning fires (fallback would write to the
  wrong profile).
- Subprocess HOME contract (`constants.rs`): `terminal.home_mode` /
  `TERMINAL_HOME_MODE` = `auto` (default: real home unless in container),
  `real`, or `profile`; `get_real_home` skips the profile's `home/` dir,
  consulting JOEY_REAL_HOME, $HOME, getpwuid, USERPROFILE, HOMEDRIVE/HOMEPATH.

---

## 6. Secret Redaction (`redact.rs`)

Port of upstream redact.py. Kill switch `JOEY_REDACT_SECRETS`
(default on; config `security.redact_secrets: false` opts out) is
**snapshotted at first use** so runtime env mutation can't disable it
mid-session.

Masking: tokens < 18 chars fully masked; longer keep first 6 + last 4.
Pattern families:

- ~45 known API-key prefixes (sk-, sk-ant, gh[pousr]_, github_pat_, xox[baprs]-,
  xapp-, AIza, pplx-, fal_, fc-, bb_live_, gAAAA, AKIA (AWS), sk_live_/sk_test_/
  rk_live_ (Stripe), SG. (SendGrid), hf_, r8_, npm_, pypi-, dop_/doo_v1_, am_,
  tvly-, exa_, gsk_, syt_, mem0_, xai-, ntn_ (Notion), fw-/fw_/fpk_, ...)
- ENV assignments (`KEY=value` where KEY contains API_KEY/TOKEN/SECRET/
  PASSWORD/CREDENTIAL/AUTH), dotted/line-anchored config keys, YAML
  `password: x`, JSON fields, Authorization headers (any scheme +
  Proxy-Authorization), x-api-key-style headers
- Telegram bot tokens, PEM private-key blocks, DB connection strings
  (postgres/mysql/mongodb/redis/amqp user:pass@), URL bare tokens and
  userinfo, JWTs (eyJ...), E.164 phone numbers, sensitive query params
  (`access_token`, `api_key`, `signature`, `code`, ...), form bodies.

Entry points: `redact_secrets` / `redact_sensitive_text(_opts)` (general),
`redact_terminal_output(output, command, force)` — env-dump commands
(`env`, `printenv`, `set`, `export`, `declare`, incl. inside pipelines) get
the full assignment pass; other commands use code-file mode to avoid
mangling source dumps. Every log line passes through redaction before disk.

---

## 7. Logging (`logging.rs`)

- `~/.joey/logs/agent.log` — shared catch-all at `logging.level`
  (default INFO), size-rotated at `logging.max_size_mb` (5 MB) keeping
  `logging.backup_count` (3) backups (`agent.log.1..N`, stdlib
  RotatingFileHandler cascade).
- `~/.joey/logs/errors.log` — WARNING+ triage log, fixed 2 MB / 2 backups.
- Format mirrors upstream: `TIMESTAMP LEVEL [session-id] target: message`;
  per-thread session tag via `set_session_context`.
- Filter: `JOEY_LOG` (RUST_LOG-style), then RUST_LOG, else everything to the
  file layer. Console (stderr) output is opt-in only via `init_verbose`
  (`--verbose`).
- Every formatted line is redacted (RedactingFormatter port).

---

## 8. Other Notable Pieces

- **`JOEY_HOME` override** (§3/§5) — env var; per-profile homes are just
  JOEY_HOME values under `profiles/`. Also `JOEY_REAL_HOME` (subprocess
  home), `JOEY_LOG`, `JOEY_IGNORE_USER_CONFIG=1`, `JOEY_REDACT_SECRETS`,
  `JOEY_OPTIONAL_SKILLS` / `JOEY_BUNDLED_SKILLS` / `JOEY_OPTIONAL_MCPS`
  (packaged dir overrides).
- **`auth_store.rs`** — `~/.joey/auth.json` v1 (`{version, providers,
  active_provider, updated_at}`); atomic writes, 0600, flock on auth.lock
  (15s bounded wait, degrades to lock-free), corrupt stores preserved as
  `auth.json.corrupt`. One store per profile home.
- **`branding.rs`** — single source of brand truth: AGENT_NAME "Joey Agent",
  CLI "joey", env prefix `JOEY_`, `~/.joey`, toolset prefix `joey-`.
- **cron** — `~/.joey/cron/jobs.json` (`{"jobs": [...]}`), `.jobs.lock`
  flock + tick lock, per-job markdown output under `cron/output/`.
- **Environment detection** — `is_termux`, `is_wsl`, `is_container`, WSL
  path translation (drive paths and `\\wsl$` UNC).
