# joey-cron — Built-in Scheduler

Self-contained scheduler: no system crontab, no external cron crate. Schedule
matching is a croniter-compatible matcher written in-crate.

**Storage (Hermes-compatible, byte-for-byte):**
- Jobs: `~/.joey/cron/jobs.json` — `{"jobs": [...], "updated_at": <iso>}` envelope
- Output: `~/.joey/cron/output/{job_id}/{timestamp}.md`
- Timestamps use Python `datetime.isoformat()` shape (`+HH:MM` offset, never `Z`)

## Schedules (`Schedule` struct, `kind` field)

- `once` — one-shot at a fixed time (`run_at`); 120s grace window (`ONESHOT_GRACE_SECONDS`)
- `interval` — every N minutes (`minutes`)
- `cron` — standard cron expression (`expr`), croniter-compatible
- Accepts human strings: `30m`, `every 2h`, `0 9 * * *`

## Job fields (selected)

id, name, prompt, skills/skill, model, provider, script, no_agent,
context_from, workdir, enabled_toolsets, repeat (`{"times": N, "completed": M}`
bounded-repeat bookkeeping), deliver, origin, last_error,
last_delivery_error, run/fire claims (cross-process flock with `.jobs.lock`,
30s wait bound, 1800s stale-claim TTL).

## Delivery targets

`deliver` (default `origin` if the job has an origin session, else `local`):
`origin`, `local`, or `platform:chat_id`. Note: delivery back into a CLI
terminal is not live — target a gateway-connected platform (e.g.
`deliver=telegram`) for push notifications; `local` output is viewable via
the cron output directory / `joey cron` listing.

## Runner

60-second ticker (`TICKER_INTERVAL_SECONDS`). Jobs run an Agent turn with a
cron-specific prompt (`build_cron_prompt`), or — with `--no-agent` — run a
script from `~/.joey/scripts/` and deliver its stdout verbatim (script stdout
can also be injected into the agent prompt each run). Output retention
defaults to keeping 50 files per job.

## CLI (`joey cron ...`)

- `joey cron` / `list [--all]` — list (including disabled with `--all`)
- `create|add SCHEDULE [PROMPT] [--name] [--deliver] [--repeat N]
  [--skill S]... [--skills a,b] [--script PATH] [--workdir DIR] [--no-agent]`
- `pause|resume|run|remove <job_id>` — `run` triggers now + one synchronous tick
- `status` — ticker heartbeat ages (is the scheduler alive?)
- `tick [--loop]` — run due jobs once; `--loop` is the standalone scheduler
  (upstream runs this inside the gateway; use on hosts without one)
- `edit` / `runs` / `history` — recognized but not yet implemented

## Related config keys

`cron.provider` (""), `cron.output_retention`, `approvals.cron_mode`
(default "deny" — dangerous-command approval behavior inside cron runs).
