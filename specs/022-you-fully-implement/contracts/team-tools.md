# Contracts — HyperCode Agent Teams (Phase 1)

Public surfaces added by this feature. All additions are strictly optional or additive (Constitution VII); nothing existing changes shape or behavior while the feature is disabled.

## 1. delegate_task parameter additions (joey-orchestration)

| Param | Type | Required | Semantics |
|--------|------|----------|-----------|
| team | string | no | Team name; registers the spawned child as a member of that team. The first reference (the lead) lazily creates the team record. Error `team mode is disabled` when hypercode.team.enabled is false. |
| name | string | no | Member name (mailbox identity). Defaults to the child id. Must be unique within the team. |

Existing parameters are unchanged. Teammates use the existing role profiles (explorer = read-only investigator, implementor = write-capable implementer). The lead is spawned as an Orchestrator-role child with the team-lead directive.

## 2. New tools (toolset `team`, registered alongside the delegation group)

Team children (lead and teammates) receive the `team` toolset appended to their role toolsets — role toolsets alone do not include it.

### team_status

- params: `{}`
- returns: team name, objective, members `[{name, role, status, current_task, completed, failed}]`, tasks `[{id, title, status, claimed_by, dependencies}]`

### team_message

- params: `to` (string, required), `content` (string, required)
- returns: `delivered to <to>`; error if the recipient is not a current member; drop-oldest applies when the recipient inbox is full

### team_tasks

- params: `action` enum `[add, list, claim, complete, release]`; `title` (string, for add); `task_id` (string, for claim/complete/release); `success` (bool, for complete, default true)
- returns: add → `{"id": "task_..."}`; list → task array; claim → the claimed task, or error `task not claimable` (status is not Pending, or a dependency is not Done); complete → final status; release → task returned to Pending (lead or stopping member)
- error style: single-line strings, matching the existing delegation tool error style

## 3. Configuration keys (joey-core defaults + hypercode.rs parsing)

| Key | Type | Default | Meaning |
|-----|------|---------|---------|
| hypercode.team.enabled | bool | false | Feature gate; disabled means byte-identical delegation behavior |
| hypercode.team.lead_model | string | "" | Lead model override; empty inherits the orchestrator's effective model |
| hypercode.team.max_members | int | 8 | Hard cap on members per team |
| hypercode.team.max_parallel_members | int | 4 | Concurrent teammates the lead keeps running at once (advisory cap applied via the lead directive; max_members is the hard spawn cap) |
| hypercode.team.message_limit | int | 10 | Pending messages per inbox (drop-oldest) |
| hypercode.team.poll_interval_ms | int | 500 | Mailbox poll cadence hint used by directives |
| hypercode.team.cleanup_days | int | 7 | Retention window for persisted task lists |

## 4. On-disk file formats (`~/.joey/teams/<team>/`)

- `config.json`: `{"name","objective","lead","created_at","members":[{name,role,model}]}`
- `tasks.json`: `{"tasks":[{id,title,status,claimed_by,dependencies}]}` with lowercase status
- `inboxes/<member>.json`: `{"messages":[{from,to,content,timestamp}]}`

Plain UTF-8 JSON, written synchronously on change (Constitution III).

## 5. Report surface (joey-cli HypercodeReport)

New optional field `mode_decisions: Vec<String>` with entries formatted `mode=<subagent|team> task=<summary> rationale=<text>`; empty when all work ran in subagent mode.
