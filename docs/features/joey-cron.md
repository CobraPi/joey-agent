# joey-cron — self-contained job scheduler

`joey-cron` is the built-in cron subsystem of the joey-agent workspace: a port of upstream Hermes Agent's `cron/` package that needs no system crontab and no external cron crate. Schedule matching is a croniter-compatible matcher written in-crate (`croniter.rs`); jobs live in a Hermes-compatible `jobs.json` store under `~/.joey/cron/`; a 60-second ticker scans for due jobs, claims them with cross-process file locks, and dispatches them concurrently to a caller-supplied async runner (the CLI injects a headless agent turn). Every run — success or failure — is archived as a Markdown document under `~/.joey/cron/output/<job_id>/`.

> See also: [../cron.md](../cron.md)

## Overview

The crate is deliberately self-contained and sits low in the workspace DAG (its only workspace dependency is `joey-core`, for the home directory, config, and the timezone-aware clock). Three modules split the work:

- `croniter.rs` parses and evaluates cron expressions (5 fields, or 6 with a trailing seconds field) on the configured timezone's wall clock.
- `jobs.rs` owns the data model (`Job`, `Schedule`, `Repeat`), the tolerant `jobs.json` store (`CronStore`), schedule parsing (`parse_schedule`), ISO timestamp handling in Python `datetime.isoformat()` shape, and all grace-window/claim bookkeeping.
- `scheduler.rs` owns the tick loop (`Scheduler`): the `.tick.lock` whole-tick lock, due-job dispatch, the in-flight guard set, the per-run output documents, and the `CRON_PROMPT_HINT` prepended to agent-run prompts.

All schedule math and stored timestamps use the configured-timezone clock (`joey_core::time::now()`), serialized in Python `datetime.isoformat()` shape (`+HH:MM` offset, microseconds, never `Z`) so hermes' `datetime.fromisoformat` can always parse them.

## Module map

| File | Purpose |
|---|---|
| `src/lib.rs` | Crate root; re-exports the public API |
| `src/croniter.rs` | `CronExpr`: croniter-compatible cron parsing and next-run search |
| `src/jobs.rs` | `CronStore`, `Job`/`Schedule`/`Repeat`, `CreateJobOptions`, ISO helpers, duration/schedule parsing, locking, repair, retention |
| `src/scheduler.rs` | `Scheduler` ticker, `JobRunner`, `run_one_job`, `build_cron_prompt`, `CRON_PROMPT_HINT`, `RUNNING_JOB_IDS` |

## Public API

### Constants

| Constant | Value | Meaning |
|---|---|---|
| `TICKER_INTERVAL_SECONDS` | `60` | Ticker loop interval (upstream `TICKER_INTERVAL_SECONDS`) |
| `ONESHOT_GRACE_SECONDS` | `120` | Grace window for one-shot jobs (upstream `ONESHOT_GRACE_SECONDS`) |
| `ONESHOT_RUN_CLAIM_TTL_SECONDS` | `1800.0` | Fallback stale-recovery TTL for a one-shot's running-claim |
| `CRON_PROMPT_HINT` | `"[IMPORTANT: You are running as a scheduled cron job. …]"` | Execution hint prepended to every agent-run job prompt (ported verbatim) |

### Types and free functions

| Item | Kind | Notes |
|---|---|---|
| `CronExpr` | struct | Parsed, validated cron expression (see below) |
| `CronExpr::parse(expr)` | method | Parse 5 or 6 fields; 7 fields rejected |
| `CronExpr::has_seconds()` | method | True for the 6-field (trailing seconds) form |
| `CronExpr::next_after(after)` | method | Next occurrence strictly after `after`, on the configured zone's wall clock |
| `CronExpr::next_after_in_tz(after, tz)` | method | Same, in an explicit named timezone (test hook) |
| `Job` | struct | One job record; field order mirrors upstream `create_job` (full table below) |
| `Schedule` | struct | `{"kind": …}` schedule dict; malformed schedules become all-`None` |
| `Schedule::once/interval/cron` | constructors | Build the three schedule kinds |
| `Repeat` | struct | `{"times": N\|null, "completed": M}` bounded-repeat bookkeeping |
| `CreateJobOptions` | struct | Everything beyond prompt+schedule for `create_job` |
| `IsoStamp` | struct | Parsed ISO timestamp: `naive` wall-clock fields + optional `offset` |
| `parse_schedule(schedule)` | fn | Schedule string → `Schedule` (grammar table below) |
| `parse_duration(s)` | fn | `"30m"` → 30, `"2h"` → 120, `"1d"` → 1440 (minutes) |
| `parse_isoformat(input)` / `fmt_isoformat(dt)` / `now_isoformat()` | fns | Python `fromisoformat`/`isoformat` compatible time handling |
| `ensure_aware(stamp)` / `ensure_aware_str(s)` | fns | Normalize a (possibly naive) stamp into the configured timezone |
| `compute_next_run(schedule, last_run_at)` | fn | Next run ISO string, or `None` when there are no more runs |
| `new_job_id()` | fn | uuid4 hex, first 12 chars (upstream shape) |
| `build_cron_prompt(job)` | fn | `CRON_PROMPT_HINT` + `job.prompt`; runners MUST use this, not the raw prompt |
| `get_running_job_ids()` | fn | Snapshot of job ids executing in this process |
| `JobRunner` | type alias | `Box<dyn Fn(Job) -> …Future<Output = Result<String>>>` — runs one job, returns the agent's final response |
| `Scheduler` | struct | Pairs a `CronStore` with a `JobRunner` |

### `Scheduler`

| Method | Behavior |
|---|---|
| `new(store, runner)` | Construct |
| `tick()` | One tick, waits for every dispatched job (upstream `tick(sync=True)`; manual `joey cron tick`); returns jobs processed |
| `tick_detached()` | One tick without waiting (upstream gateway ticker `tick(sync=False)`); returns jobs dispatched |
| `run_forever()` | Tick every `TICKER_INTERVAL_SECONDS` regardless of job runtime; heartbeat files written every loop |

### `CronStore` (every method)

| Method | Behavior |
|---|---|
| `open_default()` | Store under the active `~/.joey` home's `cron/` directory |
| `with_dir(dir)` | Store rooted at an explicit directory (tests) |
| `dir()` | The cron directory this store is rooted at |
| `tick_lock_path()` | `<cron_dir>/.tick.lock` — serializes whole ticks |
| `ensure_dirs()` | Create `cron/` and `cron/output/` with owner-only `0700` permissions |
| `job_output_dir(job_id)` | Resolve a job's output dir; rejects empty/`.`/`..`/`/`/`\` path-escape ids |
| `load()` | All jobs, tolerantly (BOM strip, control-char repair, bare-list wrap, per-record repair) |
| `save(jobs)` | Persist all jobs under the jobs lock |
| `create_job(prompt, schedule, opts)` | Create a job — defaults, snapshots and validation live here (see below) |
| `get_job(job_id)` | Exact-ID lookup |
| `resolve_job_ref(job_ref)` | ID or case-insensitive name; ambiguous names are an error naming the matching IDs |
| `list_jobs(include_disabled)` | All jobs, or only enabled ones |
| `pause_job(job_ref, reason)` | `enabled=false`, `state="paused"`, stamps `paused_at`/`paused_reason` |
| `resume_job(job_ref)` | Recomputes a FUTURE `next_run_at`; refuses an expired one-shot |
| `trigger_job(job_ref)` | Sets `next_run_at` to now — fires on the next scheduler tick |
| `remove_job(job_ref)` | Removes the job AND deletes its output directory |
| `mark_job_run(job_id, success, error, delivery_error)` | Post-run bookkeeping (rules below) |
| `claim_dispatch(job_id)` | At-most-times claim for finite one-shots, taken BEFORE execution |
| `advance_next_run(job_id)` | Preemptively advance `next_run_at` for a recurring job BEFORE execution |
| `get_due_jobs()` | All jobs due now; persists repairs, claims, fast-forwards |
| `save_job_output(job_id, output)` | Write one run's `.md` document, then prune to retention |
| `record_ticker_heartbeat(success)` | Best-effort liveness marker every ticker loop |
| `get_ticker_heartbeat_age()` / `get_ticker_success_age()` | Seconds since the last loop / last error-free tick |

`create_job` normalization rules: `repeat` of 0/negative means infinite; one-shot schedules default `repeat=1`; `deliver` defaults to `"origin"` when `opts.origin` is set, else `"local"`; `no_agent=true` without a `script` is rejected; `workdir` must be an absolute, existing directory (tilde-expanded); empty strings in `skills`/`context_from`/`enabled_toolsets` are dropped; the unpinned `model` axis is snapshotted from config into `model_snapshot`; a one-shot whose `run_at` is more than `ONESHOT_GRACE_SECONDS` in the past is rejected.

## Schedule grammar

`parse_schedule` accepts four shapes, tried in this order:

| Input | Kind | Result |
|---|---|---|
| `"every 30m"`, `"Every 2 hours"` | `interval` | Recurring every N minutes; display `every {N}m` |
| `"30m"`, `"2h"`, `"1d"` | `once` | One-shot that far from now; display `once in {input}` |
| `"0 9 * * *"` (5 fields) or `"0 9 * * * 30"` (6 fields) | `cron` | Cron expression, re-validated via `CronExpr::parse` |
| `"2026-02-03T14:00"`, `"2026-02-03 14:00:00Z"` | `once` | One-shot at that timestamp (naive → configured timezone) |

Duration grammar — regex `^(\d+)\s*(m|min|mins|minute|minutes|h|hr|hrs|hour|hours|d|day|days)$`, matched case-insensitively after trimming:

| Unit family | Multiplier (minutes) |
|---|---|
| `m`, `min`, `mins`, `minute`, `minutes` | 1 |
| `h`, `hr`, `hrs`, `hour`, `hours` | 60 |
| `d`, `day`, `days` | 1440 |

Cron detection: at least 5 whitespace-separated fields whose first five all match `^[\d\*,/]+$` (names like `feb`/`mon` are not routed here — only `CronExpr::parse` accepts them).

ISO timestamps accept `YYYY-MM-DD` (→ midnight), `T` or space (in fact any single character, CPython-style) separator, optional seconds, optional `.fraction` truncated to microseconds, trailing `Z`, and `+HH[:MM[:SS]]` / `+HHMM` offsets; anything else is `"Invalid isoformat string: '…'"`.

Accepted / rejected examples (from the crate's own test tables):

| Input | Parses to |
|---|---|
| `2026-02-03` | midnight, naive |
| `2026-02-03T14:00` / `2026-02-03 14:00` / `2026-02-03X14:00` | 14:00, naive (any single separator char) |
| `2026-02-03T14:00:30.123456789` | microseconds truncated to `123456` |
| `2026-02-03T14:00:00Z` / `…+05:30` / `…-0530` / `…+05` | aware, offset preserved |
| `nope`, `2026-13-99`, `2026-2-3`, `2026-02-03T25:00`, `2026-02-03T14:00abc` | rejected |

Output shape (`fmt_isoformat`): `+HH:MM` offset (never `Z`), a `.ffffff` microsecond fraction only when nonzero — e.g. `2026-02-03T14:00:30+05:30` or `2026-02-03T14:00:30.123456+05:30`; UTC renders `+00:00`.

### Next-run computation (`compute_next_run`)

| Schedule kind | Rule |
|---|---|
| `once` | The ORIGINAL stored `run_at` string, returned unchanged when still eligible: inside the `ONESHOT_GRACE_SECONDS` window and never run (`last_run_at` empty); otherwise `None` |
| `interval` | `last_run_at + N minutes` (falls back to `now + N` when the stamp is unparseable); first run is `now + N` |
| `cron` | `CronExpr::next_after` anchored on the actual `last_run_at` when available, so restarts don't re-base the schedule on an arbitrary restart time |
| anything else | `None` (a repaired/empty schedule has no next run) |

## Cron expression semantics

- Fields: `minute hour day-of-month month day-of-week`, plus an optional trailing 6th `seconds` field (croniter's default 6-field form). 7-field expressions (year column) are rejected with a clear error; other field counts fail with `expected 5 or 6 fields, got N`.
- Per-field syntax: `*`, lists (`,`), ranges (`-` — wrap-around allowed, so `22-2` means 22,23,0,1,2 and `fri-mon` means Fri–Mon), steps (`/`), and `N/step` which croniter treats as `N-max/step`.
- Names: months `jan`–`dec` (1-based) and days `sun`–`sat` (0-based), case-insensitive, valid in ranges too.
- Day-of-week is `0-7` where both `0` and `7` are Sunday.
- Vixie DOM/DOW OR-rule: when BOTH day-of-month and day-of-week are restricted (not `*`), a day matches when EITHER matches; otherwise the one restricted field must match.
- Next-run search steps minute-by-minute through the configured local timezone's wall clock, capped at `SEARCH_CAP_DAYS = 1461` days (four years) — no match within the cap returns `None`.
- Naive inputs are anchored to the configured joey timezone at parse time, so `"20:07"` means 20:07 on the same clock the scheduler checks against.
- DST: a wall time inside a spring-forward gap doesn't exist and is skipped; an ambiguous fall-back time takes the earlier occurrence.

Worked examples (from the crate's tests, all in UTC):

| Expression | After | Next match | Why |
|---|---|---|---|
| `0 9 * * 1` | Wed 2026-07-15 | `2026-07-20 09:00` | Monday (DOW `1` = Monday) |
| `0 9 * * 0` / `0 9 * * 7` | Wed 2026-07-15 | `2026-07-19 09:00` | `0` and `7` both Sunday |
| `0 9 1 * 1` | Wed 2026-07-15 | `2026-07-20 09:00` | Vixie OR-rule: next Monday before Aug 1 |
| `0 9 1 * 1` | Jul 31 23:00 | `2026-08-01 09:00` | OR-rule: Aug 1 (a Saturday) matches via DOM |
| `0 9 1 * *` | Wed 2026-07-15 | `2026-08-01 09:00` | DOW unrestricted → only DOM applies |
| `*/15 * * * *` | 12:07 / 12:15 | 12:15 / 12:30 | step minutes, strictly after base |
| `10-30/10 * * * *` | 12:00 | 12:10 | range with step |
| `0 9 * feb mon` / `0 9 * FEB MON` | Jul 2026 | `2027-02-01 09:00` | month + DOW names, case-insensitive |
| `0 9 * * mon-fri` | Sat 2026-07-18 | `2026-07-20 09:00` | name range |
| `0 22-2 * * *` | 12:00 / 23:30 | 22:00 same day / 00:00 next day | wrap-around hour range |
| `0 9 * * fri-mon` | Fri 10:00 | Sat 09:00 | name wrap-around (`{5,6,0,1}`) |

Parse rejections: `expected 5 or 6 fields, got N`; `7-field cron expressions are not supported (use 5 fields, or 6 with a trailing seconds field)`; `value N out of range [min,max]`; `invalid field value '…'`; `invalid step '…'`; `empty cron field`; `empty list item in cron field '…'`; `cron field '…' matches nothing`.

## jobs.json format

Path: `~/.joey/cron/jobs.json` (per-profile under `~/.joey/profiles/<name>/cron/`). Envelope: `{"jobs": [...], "updated_at": <iso>}` — serialized with `serde_json::to_string_pretty` (no trailing newline), written atomically (tempfile → fsync → rename) with `0600` file permissions inside `0700` directories. Load-time tolerance: UTF-8 BOM stripped, bare control characters inside strings escaped (Python `strict=False` retry), a bare top-level array wrapped back into the envelope, and one bad record skips only that record, never the whole load.

Full job field table (field order mirrors upstream `create_job` so fresh files look byte-identical; unknown keys round-trip via `extra`):

| Field | Type | Notes |
|---|---|---|
| `id` | string | `new_job_id()` — uuid4 hex, first 12 chars |
| `name` | string | Defaults to the first 50 chars of prompt/first skill/script, trimmed |
| `prompt` | string | May be empty (script-only jobs) |
| `skills` / `skill` | list / string\|null | Canonical deduped list; `skill` is the first entry |
| `model`, `provider` | string\|null | Pinned inference axes |
| `provider_snapshot`, `model_snapshot` | string\|null | Snapshots of unpinned axes at creation time |
| `base_url` | string\|null | Trailing `/` stripped |
| `script` | string\|null | Script to run (required for `no_agent`) |
| `no_agent` | bool | Run the script without an agent turn |
| `context_from` | list\|null | Context file paths |
| `schedule` | object | `Schedule` (below) |
| `schedule_display` | string | Human-readable form |
| `repeat` | object\|omitted | `{"times": N\|null, "completed": M}`; omitted when unset |
| `enabled` | bool | Default `true` |
| `state` | string | Default `"scheduled"`; also `"paused"`, `"error"`, `"completed"` |
| `paused_at`, `paused_reason` | string\|null | Stamped by `pause_job` |
| `created_at`, `next_run_at`, `last_run_at` | string\|null | ISO stamps in configured timezone |
| `last_status` | string\|null | `"ok"` or `"error"` |
| `last_error` | string\|null | Set on failure, cleared on success |
| `last_delivery_error` | string\|null | Delivery failures tracked separately |
| `deliver` | string | `"origin"`, `"local"`, or `platform:chat_id` |
| `origin` | any\|null | Originating session descriptor |
| `enabled_toolsets` | list\|null | Toolset allowlist for the job's agent |
| `workdir` | string\|null | Absolute, canonicalized, existing directory |
| `attach_to_session` | bool\|omitted | Only persisted when explicitly set |
| `fire_claim`, `run_claim` | object\|omitted | Transient claims `{"at": iso, "by": machine}` |
| `extra` | (flattened) | Any fields this port doesn't model, preserved round-trip |

`Schedule` fields: `kind` (`"once"`/`"interval"`/`"cron"`, `""` when repaired), `run_at` (once), `minutes` (interval), `expr` (cron), `display`, plus flattened `extra`.

Record repair (tolerant load): regenerate missing ids (recovering legacy `job_id`, else synthesize), null prompts → `""`, scalar schedules → `{}` keeping the text for display, derive missing `name`/`schedule_display`/`state`, strip invalid `next_run_at`/`last_run_at` values, and coerce odd types (float minutes, string booleans, numeric repeat counts) so one hand-edited field can't abort the record.

## Job output & retention

Every run writes `~/.joey/cron/output/<job_id>/%Y-%m-%d_%H-%M-%S.md` (configured timezone) — including failures. Success documents embed the ASSEMBLED prompt (`build_cron_prompt`, not the raw prompt), job id, run time, schedule, and the final response; failure documents carry `(FAILED)` in the title and the error in a fenced block. An empty agent response is still logged raw but counted as a soft failure.

Document shapes (upstream `run_job`):

```markdown
# Cron Job: <name>

**Job ID:** <id>
**Run Time:** %Y-%m-%d %H:%M:%S
**Schedule:** <schedule or N/A>

## Prompt

<assembled prompt>

## Response

<final response — "(No response generated)" when empty>
```

The failure variant titles the document `# Cron Job: <name> (FAILED)` and replaces the Response section with `## Error` containing the message in a fenced code block.

After each write, the job's output dir is pruned to the newest `cron.output_retention` files (default 50; reverse-lexical timestamp-filename sort; non-positive values disable pruning).

## Ticker semantics

- **Tick lock**: each tick takes a non-blocking exclusive flock on `<cron_dir>/.tick.lock`; a concurrent tick (another process, or an in-flight `tick()`) is a no-op returning `0`.
- **Jobs lock**: every load-modify-save critical section holds a process-wide mutex plus an advisory flock on `<cron_dir>/.jobs.lock`, bounded to a 30s wait (`JOBS_LOCK_TIMEOUT_SECONDS`); on timeout/failure it degrades to in-process-only locking so the scheduler stays alive.
- **Due-scan rules** (`get_due_jobs`, per job): disabled jobs are skipped; a one-shot with a fresh `run_claim` (age < TTL, where TTL = `max(JOEY_CRON_TIMEOUT × 3, 1800.0)` seconds; `JOEY_CRON_TIMEOUT` defaults to 600) is skipped as in-flight in another process; a missing `next_run_at` is recovered (one-shot via the grace window, recurring via recomputation); a cron `next_run_at` stored under a different UTC offset whose local wall clock is still future is recomputed to preserve wall-clock intent; a stale recurring job (more than its grace window late, where grace = half the period clamped to `[120, 7200]` seconds) is fast-forwarded (persisted) but still fires ONCE; a one-shot past its dispatch limit is removed unless its run is still in flight; due one-shots get a durable `run_claim` `{"at": now, "by": machine}`.
- **Tick order**: `get_due_jobs` → `advance_next_run` for every due recurring job (at-most-once: `next_run_at` is already in the future before execution begins) → dispatch due jobs CONCURRENTLY via `tokio::spawn`, skipping ids already in the in-process `RUNNING_JOB_IDS` set.
- **`run_one_job`**: `claim_dispatch` (finite one-shots pre-increment `repeat.completed` BEFORE the side effect runs, so a crash mid-execution can't re-fire it) → execute via the `JobRunner` → save output → `mark_job_run`.
- **`mark_job_run` rules**: stamps `last_run_at`/`last_status`/`last_error` (cleared on success)/`last_delivery_error`; clears `fire_claim`/`run_claim`; pre-claimed one-shots don't double-count; repeat limit reached → the job is DELETED; no next run computable → a recurring job gets `state="error"` and stays enabled (never silently disabled), a one-shot gets `enabled=false`, `state="completed"`; otherwise `state` returns to `"scheduled"` (unless paused). An empty-but-successful response is a soft failure: `"Agent completed but produced empty response (model error, timeout, or misconfiguration)"` — `last_status` is not `"ok"`.
- **Delivery**: `deliver` defaults to `"origin"` when the job has an origin session, else `"local"` (delivery to platforms is handled by the gateway layer, not this crate).
- **Heartbeat files**: `ticker_heartbeat` (every loop) and `ticker_last_success` (error-free ticks) hold epoch-seconds floats; `run_forever` writes a heartbeat once before the first sleep so status sees a live ticker immediately after startup.

## Configuration & CLI cross-reference

| Key / env | Effect |
|---|---|
| `cron.output_retention` | Output files kept per job (default 50; `<=0` disables pruning) |
| `JOEY_CRON_TIMEOUT` | Drives the one-shot run-claim TTL: `max(value × 3, 1800)` seconds; default 600 |
| `JOEY_MACHINE_ID` / `HOSTNAME` | Claim attribution (`"by"` field) — diagnostics only, not correctness |
| `JOEY_HOME` / `-p --profile` | Selects which `cron/` directory the store opens |

The `joey cron` subcommands (create/list/pause/resume/trigger/remove/tick/status) live in the CLI crate — see [joey-cli.md](joey-cli.md). Timezone selection (`JOEY_TIMEZONE` / `timezone:` config) is documented in [joey-core.md](joey-core.md).

A minimal Hermes-compatible `jobs.json` envelope as this crate writes it:

```json
{
  "jobs": [
    {
      "id": "1b0e9d3a4c5f",
      "name": "say hi",
      "prompt": "say hi",
      "skills": [],
      "skill": null,
      "model": null,
      "provider": null,
      "provider_snapshot": null,
      "model_snapshot": "gpt-5.2",
      "base_url": null,
      "script": null,
      "no_agent": false,
      "context_from": null,
      "schedule": {
        "kind": "interval",
        "minutes": 30,
        "display": "every 30m"
      },
      "schedule_display": "every 30m",
      "repeat": { "times": null, "completed": 0 },
      "enabled": true,
      "state": "scheduled",
      "paused_at": null,
      "paused_reason": null,
      "created_at": "2026-09-03T10:00:00+00:00",
      "next_run_at": "2026-09-03T10:30:00+00:00",
      "last_run_at": null,
      "last_status": null,
      "last_error": null,
      "last_delivery_error": null,
      "deliver": "local",
      "origin": null,
      "enabled_toolsets": null,
      "workdir": null
    }
  ],
  "updated_at": "2026-09-03T10:00:00+00:00"
}
```

(Field order mirrors upstream `create_job`; `repeat` is stored nested under the `repeat` key; transient claims appear only while set.)

## Defaults & limits

| Value | Default / bound |
|---|---|
| `TICKER_INTERVAL_SECONDS` | `60` |
| `ONESHOT_GRACE_SECONDS` | `120` |
| `ONESHOT_RUN_CLAIM_TTL_SECONDS` | `1800.0` (fallback; `max(JOEY_CRON_TIMEOUT×3, 1800)`) |
| Jobs-lock wait | `30`s, then in-process-only |
| Recurring fast-forward grace | half the period, clamped to `[120, 7200]`s |
| Cron next-run search cap | `1461` days |
| Output retention | `50` files per job |
| Job-name derivation | first `50` chars of the label source |
| File/dir permissions | files `0600`, dirs `0700` |

## Testing

- `croniter.rs`: DOW numbering, Vixie OR-rule, steps, wrap-around ranges, month/DOW names, trailing seconds, strictly-after semantics, named-timezone wall clock, DST gap skip, rejections, `N/step`.
- `jobs.rs` duration/ISO: grammar table, `fromisoformat` leniency table, `isoformat` output shape, naive-legacy anchoring.
- `jobs.rs` store: envelope shape on save, upstream fixture round-trip, BOM/bare-list/control-char/record repair, create defaults, one-shot auto-repeat and delete-on-completion, past one-shot rejection, `no_agent` validation, claim/dispatch semantics, due-scan recovery + fast-forward + claims, pause/resume/trigger lifecycle, name resolution and ambiguity, uncomputable-next-run → error-not-disabled, output naming/modes/retention/path safety, remove-deletes-output, heartbeat files.
- `scheduler.rs`: tick runs due jobs and writes docs, failure doc + `state=error` stays enabled, empty-response soft failure, one-shot removal after run, next-run advances before execution, in-flight guard, concurrent-tick no-op under the tick lock.
- Malformed-input regressions: `parse_duration` / `parse_schedule` never panic on malformed input.

## See also

- [../cron.md](../cron.md) — user-facing cron overview
- [joey-cli.md](joey-cli.md) — `joey cron` subcommands and the headless job runner
- [joey-core.md](joey-core.md) — home/profile paths, config keys, the timezone-aware clock
- [joey-gateway.md](joey-gateway.md) — delivery of job output back to platforms
