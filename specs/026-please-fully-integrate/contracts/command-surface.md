# Contract: Command Surface

Public dispatch contract (constitution VII: additive; existing twelve `/speckit-*` names and behavior unchanged).

## Commands (10 lifecycle + 2 auxiliary)

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

## Dispatch invariants
1. Both forms of a row MUST dispatch through the identical native path (one implementation, two spellings).
2. Unknown speckit command in either form → error listing available commands with closest suggestion.
3. Dotted intercept matches only the literal `speckit.` prefix on bare (non-slash) input; collisions with user commands error, never shadow.
4. Missing pre-flight script → internal fallback for known scaffolds + warning (FR-002); script present but failing → hard error naming script and platform variant; no partial artifacts.
5. Completion surfaces offer both forms.
