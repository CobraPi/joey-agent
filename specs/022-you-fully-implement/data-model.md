# Data Model — HyperCode Agent Teams (Phase 1)

Entities, validation rules (from spec FR-001..FR-019), and state transitions. Storage: one directory per team under the joey home (`~/.joey/teams/<team-name>/`, honoring the JOEY_HOME override). All files are plain UTF-8 JSON written synchronously on change (Constitution III).

## Team

One collaboration unit; at most one active team per session; teams never nest (FR-010).

- **name**: String — directory-safe slug, unique in the registry
- **objective**: String — the user's natural-language objective
- **lead**: TeamMember reference — fixed for the team's lifetime (FR-011)
- **members**: Vec<TeamMember> — lead plus teammates; length capped at max_members (8)
- **state**: Active → WindingDown → Closed
- **created_at**: ISO-8601 timestamp string

File: `config.json` — `{"name","objective","lead","created_at","members":[{name,role,model}]}`

Transitions: Active → WindingDown (all tasks terminal, or stop requested); WindingDown → Closed (every member stopped, mailboxes and config cleaned, tasks.json retained).

## TeamRecord

The process-wide registry's in-process entry (one per active team): the Team above plus live member statuses and registry state. The registry mutex over TeamRecords is the claiming authority; its durable projection is config.json + tasks.json (no separate on-disk file).

## TeamMember (Lead / Teammate)

- **name**: String — unique within the team; the mailbox identity
- **role**: "lead" | "explorer" | "implementor" — role profile assigned at spawn (read-only investigator or write-capable implementer, FR-017)
- **model**: String — effective model; the lead's model is configurable via configuration and defaults to the orchestrator's effective model (FR-019)
- **status**: Idle | Working | Stopped

Validation: names unique per team; teammates never hold delegate_task (Leaf retain rule); at most one active team per process registry.

## TeamTask

- **id**: String — `task_{uuid}`
- **title**: String
- **status**: Pending | Running | Done | Failed (serialized lowercase, matching joey-omo's TeamTaskStatus naming)
- **claimed_by**: Option<String> — member name currently holding the task
- **dependencies**: Vec<String> — ids of tasks that must be Done before this task is claimable (FR-003; new capability vs joey-omo's TeamTask, which has no dependencies field)

File: `tasks.json` — `{"tasks":[{id,title,status,claimed_by,dependencies}]}`

Validation (FR-004): a claim succeeds only when status = Pending AND every dependency is Done; exactly one claimant wins under concurrency (registry mutex); complete(success) sets Done or Failed and releases the claim; a member stopping or failing returns its Running tasks to Pending (FR-015).

Transitions: Pending → Running (claim); Running → Done | Failed (complete); Running → Pending (release on member stop or failure).

## TeamMailbox / TeamMessage

One inbox file per member: `inboxes/<member>.json`.

- **TeamMessage**: from (member name), to (member name), content (String), timestamp (ISO-8601)

Validation: recipient must be a current member; a member's pending inbox is capped at message_limit (10) messages with drop-oldest policy; delivery is teammate-pull via the team tools on each iteration (research.md D6). Teammate-to-teammate messages never route through the lead (FR-002).

Transitions: queued → delivered (recipient receive drains its inbox).

## ModeSelectionDecision

- **task**: String — summary of the delegated task
- **mode**: "subagent" | "team"
- **rationale**: String — one-to-two-sentence justification

Recorded per delegated task in the run report (FR-016); surface defined in contracts/team-tools.md.

## Retention and cleanup

- Session end: members stopped via manager shutdown; `inboxes/*.json` and `config.json` deleted; `tasks.json` retained for resumption (FR-012).
- Startup scan: `~/.joey/teams/*` directories older than cleanup_days (default 7) are purged entirely.
