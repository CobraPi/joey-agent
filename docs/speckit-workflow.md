# Spec-Kit Workflow Slash Commands (`speckit_slash.rs`)

The full GitHub Spec Kit lifecycle is available as native slash commands in
both the line REPL and the TUI. The design principle (mirroring
`joey-speckit-ui`): **SpecKit's own logic is never reimplemented** —

- repo scaffolding and prerequisites come from the real
  `.specify/scripts/bash/*.sh` scripts (run as subprocesses, argv-only,
  never a shell string), and
- the per-step workflow instructions come from the bundled
  `speckit-<step>` skills (`~/.joey/skills/speckit-*/SKILL.md`) — the
  canonical spec-kit step definitions.

Each lifecycle step: (1) runs its pre-flight script, (2) loads the skill's
workflow body, (3) submits ONE agent turn = skill workflow + pre-flight
JSON + the user's arguments. The agent authors the artifact
(spec.md / plan.md / tasks.md …) with its file tools — the same execution
model as running the skill by hand, minus the copy-paste.

## Lifecycle (in order)

| Command | Pre-flight script | What the agent does |
|---|---|---|
| `/speckit-constitution` | — | Create/update `.specify/memory/constitution.md` from Q&A |
| `/speckit-specify <description>` | `create-new-feature.sh` (`--allow-existing-branch`) | Scaffold the feature branch (specs/NNN-slug/spec.md template) and author the spec; re-runs update in place |
| `/speckit-clarify` | `check-prerequisites.sh` | Identify underspecified areas in spec.md |
| `/speckit-plan` | `setup-plan.sh` | Author plan.md (design artifacts) |
| `/speckit-checklist` | — | Generate a feature checklist |
| `/speckit-tasks` | `setup-tasks.sh` | Author dependency-ordered tasks.md |
| `/speckit-analyze` | `check-prerequisites.sh --include-tasks` | Cross-artifact consistency/coverage analysis |
| `/speckit-implement` | `check-prerequisites.sh --require-tasks --include-tasks` | Execute tasks one by one |
| `/speckit-converge` | `check-prerequisites.sh --include-tasks` | Assess implementation vs spec, list gaps |
| `/speckit-taskstoissues` | `check-prerequisites.sh --include-tasks` | Convert tasks to GitHub issues |

Auxiliary: `/speckit-status` (current feature + spec/plan/tasks readiness
checkboxes + next-step hint + existing features list; no agent turn) and
`/speckit-help` (lifecycle overview).

## Behavior details

- **Repo detection**: nearest ancestor with `.specify/`; otherwise a clear
  error pointing at spec-kit initialization.
- **Pre-flight failure is fatal**: when a gate script exits non-zero (e.g.
  `/speckit-plan` without a spec), the error is surfaced directly and no
  agent turn runs — the workflow can't proceed on missing prerequisites.
- **`/speckit-specify` is create-OR-update**: `--allow-existing-branch`
  reuses the existing feature scaffold, so re-running refine the spec.
- The user's description is passed to `create-new-feature.sh` as the
  positional feature description (short-name auto-derived), and the
  feature's `feature.json` tracks the active feature for all later steps.
- In the TUI, lifecycle steps submit through the engine actor (normal turn
  semantics: streaming, steer/queue/interrupt, kill/restart all apply).

## Files

- Implementation: `crates/joey-cli/src/speckit_slash.rs` (registry table,
  script runner, skill loader, prompt composer, status renderer).
- REPL wiring: `run_slash_command` (`repl.rs`); TUI wiring:
  `TuiSession::handle_slash` (`tui.rs`).
- Visual UI alternative: `joey speckit` (see [speckit-ui.md](speckit-ui.md)).
