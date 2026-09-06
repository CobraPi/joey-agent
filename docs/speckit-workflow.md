# Spec-Kit Workflow (feature 026)

The native spec-kit integration: the full GitHub Spec Kit lifecycle runs
inside Joey itself — no external agent binaries are spawned at any point.

## Overview

Ten lifecycle commands are available natively:

`specify`, `clarify`, `plan`, `constitution`, `checklist`, `tasks`,
`analyze`, `implement`, `converge`, `taskstoissues`

plus two auxiliary commands, `status` and `help`. Every one of them is
handled by Joey's own command path — the workflow body is resolved, the
pre-flight script runs, and the step executes as a native agent turn.
Joey never dispatches to the Copilot binary (or any other external
agent CLI); see the deliberate-deviation notes in `PORTING.md`.

Both invocation forms are first-class:

- slash form: `/speckit-<name>` (REPL and TUI)
- dotted form: `speckit.<name>`

## Command surface

| Name | Slash form | Dotted form | Pre-flight (upstream parity) |
|---|---|---|---|
| specify | /speckit-specify | speckit.specify | create-new-feature.sh --json --allow-existing-branch |
| clarify | /speckit-clarify | speckit.clarify | check-prerequisites.sh --json --paths-only |
| plan | /speckit-plan | speckit.plan | setup-plan.sh --json |
| constitution | /speckit-constitution | speckit.constitution | resolve-template.sh constitution-template --json (optional; internal fallback per FR-002) |
| checklist | /speckit-checklist | speckit.checklist | check-prerequisites.sh --json --template checklist-template |
| tasks | /speckit-tasks | speckit.tasks | setup-tasks.sh --json |
| analyze | /speckit-analyze | speckit.analyze | check-prerequisites.sh --json --require-tasks --include-tasks |
| implement | /speckit-implement | speckit.implement | check-prerequisites.sh --json --require-tasks --include-tasks |
| converge | /speckit-converge | speckit.converge | check-prerequisites.sh --json --require-tasks --include-tasks |
| taskstoissues | /speckit-taskstoissues | speckit.taskstoissues | check-prerequisites.sh --json --require-tasks --include-tasks |
| status | /speckit-status | speckit.status | none (reads lifecycle state) |
| help | /speckit-help | speckit.help | none |

Dispatch invariant: both forms of every row dispatch through one
identical native path — one implementation, two spellings. An unknown
speckit command in either form errors with the available-command list
and a closest suggestion; dotted interception matches only the literal
`speckit.` prefix on bare (non-slash) input, and collisions with user
commands error rather than shadow.

## Workflow body resolution

The instruction body executed for a step is resolved by the following
chain — precedence order, first match wins:

1. `.github/skills/speckit-<name>/SKILL.md`
2. `.github/agents/speckit.<name>.agent.md` (or
   `.github/prompts/speckit.<name>.prompt.md`)
3. `.specify/commands/speckit-<name>.md`
4. `~/.joey/skills/speckit-<name>/SKILL.md` (then
   `~/.joey/optional-skills/speckit-<name>/SKILL.md`)
5. the bundled in-binary body — vendored verbatim from upstream
   spec-kit @ `e3e6a3c` (refresh procedure:
   `crates/joey-cli/src/speckit_bodies/PROVENANCE.md`)

Frontmatter metadata floor: an override body whose frontmatter lacks
`handoffs`/`scripts`/`tools` entries inherits them from the bundled
body, so partial overrides never silently drop metadata the step needs.

Placeholder rendering, applied to whichever body wins:

- `{SCRIPT}` → the concrete pre-flight invocation for the step
- `__SPECKIT_COMMAND_<NAME>__` → `/speckit-<name>`

## Pre-flight behavior

Pre-flight scripts live under `.specify/scripts/bash`. Scaffolds that
ship PowerShell or python script variants are recognized and the
platform-appropriate variant is used.

- Missing script (older scaffolds): Joey runs an internal fallback and
  emits a warning — the step proceeds (FR-002).
- Script present but failing: hard error naming the script and the
  platform variant; no partial artifacts are produced.

## Extension hooks

Hooks are discovered from `.specify/extensions.yml` at the repo root
(absent file → zero hooks, silently).

Hook points (20): `before_`/`after_` × each of the ten lifecycle steps
(specify, clarify, plan, constitution, checklist, tasks, analyze,
implement, converge, taskstoissues).

Semantics:

- Entries with `enabled: false` are excluded.
- Mandatory hooks (`optional: false` or absent) execute — as a native
  command turn — and block until finished; a failure stops the step and
  reports the hook name.
- Optional hooks (`optional: true`) surface an invocation block
  (command/description/prompt and invocation path) without blocking; a
  failure is logged and skipped.
- Invalid/unparseable YAML → hook discovery is skipped silently and the
  command proceeds.
- A non-empty `condition` value is passed through unevaluated — the
  extension runtime owns evaluation.
- `speckit.hooks=false` disables extension discovery entirely.

## Handoffs

Step bodies may declare handoffs in their frontmatter
(`label` / `agent` / `prompt` / `send`):

- `send: true` → the next step is auto-invoked when the current one
  completes;
- otherwise the handoff is offered (surfaced for the user to accept);
- the prior step's outputs are carried into the next step.

## Session-start lifecycle context

When a spec-kit project is detected and `speckit.lifecycle_context` is
enabled, one context block is injected pre-first-turn, once per session:

```
## Spec-Kit Lifecycle Context
Feature: <feature_directory>
Step: <step> (<one-line guidance per step>)
Artifacts: spec.md [present|absent], plan.md [...], tasks.md [...]
Note: detected automatically; refresh by restarting the session or running /speckit-status.
```

The state is a pure function of on-disk artifacts: `.specify/feature.json`
names the feature directory (missing/invalid → no step), and the
existence + checkbox state of `specs/<feature>/spec.md`, `plan.md`, and
`tasks.md` derive the step (ordered, first match wins): no `spec.md` →
Specify; no `plan.md` → Clarify if open questions remain else
Plan-ready; no `tasks.md` → Tasks; unchecked top-level boxes in
`tasks.md` → Implement; all checked → Acceptance.

The block is never re-rendered mid-session; restart the session or run
`/speckit-status` (same derivation, rendered on demand) to refresh.

## Configuration

| Key | Type | Default | Effect |
|---|---|---|---|
| `speckit.enabled` | bool | true | Master switch. `false` → every new code path short-circuits; behavior identical to pre-feature (FR-013) |
| `speckit.lifecycle_context` | bool | true | Session-start lifecycle detection + one-time context injection |
| `speckit.hooks` | bool | true | `extensions.yml` discovery + hook execution (20 points) |

The three keys are independent; `speckit.enabled=false` overrides the
sub-toggles. An absent section behaves exactly as the defaults (additive
merge — older config files are unaffected). Detection no-ops (one stat
of `.specify/`) on non-spec-kit repositories.

## Orchestration + code intelligence integration

- The conductor (orchestration layer) detects the active feature and
  lifecycle step at session start and adapts dispatch accordingly:
  read-only researchers during specify/clarify/plan; parallel
  implementors with exclusive write sets during implement; one final
  verification run at acceptance.
- NeuroCode prioritizes the feature's `plan.md`/`tasks.md` file lists
  for context assembly and indexing, and aligns verification plans with
  the spec's acceptance criteria.

## Terminology normalization

The following terms are equivalent and used interchangeably across
specs, code, and docs:

- "workflow body" ≡ "vendored body"
- "Dot-Form" ≡ the dotted `speckit.<name>` invocation
- "hypercode" ≡ the orchestration/conductor layer
