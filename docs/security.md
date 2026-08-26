# Security Model

Joey Agent handles untrusted content (web pages, MCP server output, context
files, tool errors) and secrets (API keys, tokens) across several defense
layers. This page consolidates them; each layer links to the subsystem doc
with the details.

## 1. Secret redaction (`joey-core::redact`)

Port of upstream `redact.py`. Kill switch `JOEY_REDACT_SECRETS` (default on;
config `security.redact_secrets: false` opts out) is **snapshotted at first
use** so runtime env mutation can't disable it mid-session.

Masking: tokens < 18 chars fully masked; longer keep first 6 + last 4.
Pattern families:

- ~45 known API-key prefixes (sk-, sk-ant, gh[pousr]_, github_pat_,
  xox[baprs]-, xapp-, AIza, pplx-, fal_, fc-, bb_live_, gAAAA, AKIA (AWS),
  sk_live_/sk_test_/rk_live_ (Stripe), SG. (SendGrid), hf_, r8_, npm_,
  pypi-, dop_/doo_v1_, am_, tvly-, exa_, gsk_, syt_, mem0_, xai-, ntn_
  (Notion), fw-/fpk_, …)
- ENV assignments (`KEY=value` where KEY contains
  API_KEY/TOKEN/SECRET/PASSWORD/CREDENTIAL/AUTH), dotted/line-anchored
  config keys, YAML `password: x`, JSON fields, Authorization headers (any
  scheme + Proxy-Authorization), x-api-key-style headers
- Sensitive query params are matched against a canonicalized (lowercase,
  dash→underscore) copy of both the live param name and the sensitive list
  — catches `X-Amz-Signature`/`x-amz-signature` presigned-S3 signatures at
  strict egress boundaries (regression-tested; a dash/underscore
  canonicalization mismatch previously let them through)
- Telegram bot tokens, PEM private-key blocks, DB connection strings
  (postgres/mysql/mongodb/redis/amqp user:pass@), URL bare tokens and
  userinfo, JWTs (eyJ...), E.164 phone numbers, sensitive query params
  (`access_token`, `api_key`, `signature`, `code`, …), form bodies

Entry points: `redact_secrets` / `redact_sensitive_text(_opts)` (general),
`redact_terminal_output(output, command, force)` — env-dump commands (`env`,
`printenv`, `set`, `export`, `declare`, incl. inside pipelines) get the full
assignment pass; other commands use code-file mode to avoid mangling source
dumps. Every log line passes through redaction before disk, and terminal
output / search matches are redacted before reaching the model.

## 2. Credential storage

- Keys set via `config set` auto-route to `~/.joey/.env` (0600) when the key
  name is in the `ENV_API_KEYS` allowlist, ends with `_API_KEY`/`_TOKEN`, or
  starts with `TERMINAL_SSH`. Dotted keys never route. The env writer
  validates names, enforces a denylist (LD_PRELOAD, PATH, SHELL, EDITOR,
  GIT_SSH_COMMAND, JOEY_HOME, …), strips newlines, and writes atomically.
- `~/.joey/auth.json` (OAuth state) is 0600, atomic writes, flock-guarded.

## 3. Threat scanning and untrusted-content wrapping

- Context files (`.joey.md`, AGENTS.md, CLAUDE.md, `.cursorrules`) and
  `SOUL.md` are threat-scanned before being included in the system prompt.
- Tool results that carry attacker-controllable content — `web_search`,
  `web_extract`, `browser_*`, `mcp_*` — are wrapped in untrusted-content
  delimiters (min 32 chars) by the agent dispatch layer before the model
  sees them, so injected instructions can't masquerade as system guidance.
- MCP server configs pass `validate_mcp_server_entry`
  (prompt-injection-style suspicious config detection) before use.
- Tool errors run through `sanitize_tool_error` (strips framing tokens,
  2000-char cap); tool panics become sanitized `[TOOL_ERROR]` results.

## 4. SSRF / URL safety (`joey-tools::url_safety`)

Used by the web tools:

- Cloud-metadata hosts/IPs blocked always: 169.254.0.0/16 (incl.
  169.254.169.254), 100.100.100.200 (Alibaba), fd00:ec2::254 (AWS v6),
  metadata.google.internal/goog.
- Private/loopback/link-local/reserved/multicast/unspecified/CGNAT
  (100.64/10) blocked unless `security.allow_private_urls` /
  `JOEY_ALLOW_PRIVATE_URLS`.
- DNS resolved with EVERY answer checked; resolution failure fails closed.
- Sensitive-query-param detector (e.g. tokens in URLs).

## 5. File-safety guards (`joey-tools::guards`)

Device-file blocks (reads of /dev nodes that would block or emit infinite
output), binary/image extension detection, internal/credential path read
blocks (e.g. `~/.joey` secrets), sensitive-write checks, read_file-content-
echo refusal, ANSI stripping, path-traversal detection (patch header paths
get extra `..` rejection).

## 6. Dangerous-command handling

`safe_commands.rs` classifies shell commands (`is_safe_read_only_command`,
`is_dangerous_command`, `contains_command_chaining`). Dangerous commands
require interactive approval (`approvals.mode`, default "smart";
`--yolo`/`JOEY_YOLO_MODE=1` bypasses; cron runs default to
`approvals.cron_mode: deny`). `PreToolUse` hooks can additionally deny or
halt tool calls — see [HOOKS.md](HOOKS.md).

## 7. Sandboxing knobs

- `--safe-mode` — disables user config and MCP servers entirely.
- `terminal.timeout` (180s default) / hard foreground max 600s; env
  overrides `TERMINAL_TIMEOUT` / `TERMINAL_MAX_FOREGROUND_TIMEOUT`.
- `terminal.max_concurrent` caps how many agent terminal processes run at
  once (`auto` = clamp(CPU cores, 4, 16); a positive integer pins the cap;
  malformed values fall back to `auto`; env `TERMINAL_MAX_CONCURRENT`).
  Excess requests queue (per-agent round-robin) instead of exhausting the
  process table / file descriptors.
- Per-profile homes fully isolate config, credentials, sessions, and logs
  (`-p/--profile`).

## 8. Deliberate divergence

Upstream's Anthropic-OAuth path impersonates Claude Code (spoofed client
identity/headers) so subscription billing accepts its traffic. Joey does
**not** do this — it's considered a ToS violation and is intentionally
omitted. Use an Anthropic API key instead.
