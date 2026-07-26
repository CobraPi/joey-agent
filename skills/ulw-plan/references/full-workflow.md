# ulw-plan Full Workflow

The complete end-to-end planning workflow. This is the canonical reference;
the SKILL.md is the concise entry point.

## Preconditions

- Prometheus is the active agent.
- The user has described work, OR `@plan "..."` was used from any primary
  agent.

## Phase 0 — Intent Classification

Classify intent as CLEAR or UNCLEAR.

- **CLEAR signals**: specific files/features named, bounded scope, concrete
  outcome stated, no obvious missing requirement. → use `intent-clear.md`.
- **UNCLEAR signals**: "improve X", "make it better", open-ended refactor,
  unstated scope, multiple possible interpretations, missing constraints. →
  use `intent-unclear.md`.

Default to UNCLEAR when genuinely unsure.

## Phase 1 — Context Gathering (always)

Regardless of intent track, gather maximum codebase context in parallel:

- Fire `explore` subagents (background, parallel) for the relevant code areas.
- Fire `librarian` subagents for relevant library/API documentation.
- Read the discovered files directly when depth is needed.

Do not ask the user anything that repo evidence can answer.

## Phase 2 — Interview (unclear intent only)

See `intent-unclear.md` for the full protocol. Run interview rounds until
intent is CLEAR, then proceed.

## Phase 3 — Clearance Checks

All must pass before drafting:

1. Intent clear (goal, scope, constraints, non-goals).
2. Codebase context gathered.
3. No open ambiguity forcing a worker to guess.
4. Dependencies and integration points identified.

## Phase 4 — Gap Analysis (Metis) — MANDATORY

Delegate to Metis with the draft plan + context. Incorporate every finding.
If a finding changes scope, return to Phase 1 for that point only.

## Phase 5 — High-Accuracy Review (Momus + Oracle)

Dual review. Both must approve.

- Momus: clarity, verification, context completeness.
- Oracle: architectural soundness, risk, feasibility.
- Either rejects → fix → re-run Metis on revised draft → re-submit. No retry
  limit.

## Phase 6 — Write Plan to .omo/plans/{name}.md

Decision-complete plan. A worker executes it without another interview.

### Required sections

- Goal (one sentence)
- Scope / Non-goals
- Tasks (numbered): title, files, dependencies, acceptance criteria
  (binary), verification method
- Final verification (F-numbers): whole-feature proof

### Task format

```markdown
- [ ] T{n} {title}
  - Files: {paths}
  - Depends on: {task ids or none}
  - Acceptance: {binary pass condition}
  - Verify: {command or check}
```

## Phase 7 — Handoff

- Report the plan path to the user.
- Do NOT execute. The user runs `/start-work [plan-name]`.

## Exit Criteria

The planning session is complete when:

- [ ] Plan written to `.omo/plans/{name}.md`.
- [ ] Metis consulted and findings incorporated.
- [ ] Momus + Oracle both approved (or review was explicitly waived for a
  trivial plan — record the waiver reason).
- [ ] User told the plan path and the `/start-work` next step.
