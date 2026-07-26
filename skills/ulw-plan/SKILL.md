---

name: "ulw-plan"
description: "Adversarial planning workflow for Prometheus — interview, gap-analyze (Metis), high-accuracy dual review (Momus + Oracle), then write a decision-complete plan to .omo/plans/."
version: 1.0.0
author: Joey Agent
license: MIT
metadata:
  joey:
    tags: [planning, orchestration, omo, prometheus, ulw-plan]
    related_skills: []
---

# ulw-plan — Adversarial Planning Workflow

This is the canonical planning workflow for **Prometheus**, the read-only
planner agent. Load this skill at the very start of every planning session and
follow it exactly. Prometheus does not implement code; it gathers maximum
context, interviews the user, runs adversarial gap analysis, submits the plan
to high-accuracy review, and writes the result to `.omo/plans/{name}.md`.

## When to Use

- Prometheus is the active agent and the user describes work to be planned.
- The `@plan "..."` prefix is used from any primary agent to request a plan.
- Any workflow where a decision-complete, review-approved plan is required
  before implementation begins.

## When NOT to Use

- Implementation work (use Sisyphus / Hephaestus / Atlas).
- Read-only consultation on architecture (use Oracle).
- This skill writes only to `.omo/`. It never edits product code.

## Phase 0 — Intent Classification

Classify the user's request into one of two tracks. This determines the whole
interview depth.

- **Intent CLEAR** → the request is specific, scoped, and unambiguous.
  Follow `references/intent-clear.md`. Skip the long interview; confirm scope,
  gather codebase context, and proceed to gap analysis.
- **Intent UNCLEAR** → the request is open-ended, ambiguous, or undersized.
  Follow `references/intent-unclear.md`. Run the full interview protocol
  (Phase 1) until intent is clear enough to plan.

Clear vs unclear is the single most important early decision. When in doubt,
treat as UNCLEAR and interview — the cost of one extra question is far lower
than the cost of planning the wrong thing.

## Phase 1 — Interview Protocol (unclear intent only)

Behave like a senior engineer scoping a real project. Ask clarifying questions
one focused topic at a time. Do not dump a questionnaire. Pursue the answers
that repo evidence cannot resolve:

1. **Goal & outcome** — What does "done" look like? What should be true after
   this work that is not true now?
2. **Scope boundary** — What is explicitly IN scope and OUT of scope?
3. **Constraints** — Hard constraints: language, framework, dependencies,
   backward compatibility, performance budgets, deadlines.
4. **Existing context** — What is already there? (Explore the codebase; do not
   ask the user what you can read yourself.)
5. **Non-goals** — What adjacent problems should this plan NOT solve?

Rules:
- **Explore before asking.** Fire `explore` and `librarian` subagents in
  parallel (background) to gather codebase context. Only ask the user for
  decisions or ambiguities that repo evidence cannot resolve.
- **One round at a time.** Ask a focused set of questions, let the user
  answer, then decide whether intent is now clear. Repeat until clear.
- **Adopt best-practice defaults** for anything the user does not care about,
  rather than blocking on low-stakes questions. Record the adopted default.

## Phase 2 — Clearance Checks

Before drafting the plan, verify the preconditions for a decision-complete
plan. If any check fails, resolve it (ask the user or explore more) before
proceeding.

- [ ] Intent is clear (goal, scope, constraints, non-goals all established).
- [ ] Codebase context gathered (relevant files, patterns, conventions read).
- [ ] No open ambiguity that would force a worker to guess mid-implementation.
- [ ] Dependencies and integration points identified.

## Phase 3 — Gap Analysis (Metis) — MANDATORY

Before the plan is finalized, consult **Metis** (the gap analyzer). This is
non-negotiable.

- Delegate to Metis with the draft plan and the gathered context.
- Metis catches what was missed: unstated assumptions, missing edge cases,
  incomplete acceptance criteria, missed dependencies, verification gaps.
- Incorporate every Metis finding into the plan. If a finding changes scope,
  return to Phase 1 for that point only.

## Phase 4 — High-Accuracy Review Loop (Momus + Oracle)

For plans where correctness matters (most plans), run dual adversarial review.
Both reviewers must approve; if either rejects, fix and resubmit. There is no
retry limit.

- **Momus** (reviewer) — validates the plan against clarity, verification, and
  context criteria. Rejects vague tasks, missing verification steps, and
  context-free instructions.
- **Oracle** (architecture) — validates the plan against architectural soundness,
  risk, and feasibility. Rejects plans that are technically unsound or that
  ignore a better approach.

Review gates:
- If **both approve** → proceed to Phase 5 (write the plan).
- If **either rejects** → apply the feedback, re-run Phase 3 (Metis) on the
  revised draft, then re-submit to the rejecting reviewer(s). Loop until both
  approve.

## Phase 5 — Write the Plan

Write the approved plan to `.omo/plans/{plan-name}.md` using the canonical
plan template (see `references/full-workflow.md`). The plan MUST be
**decision-complete**: a downstream worker (Atlas + Sisyphus-Junior) can
execute it without another interview.

Required sections in the plan file:

- **Goal** — one-sentence statement of done.
- **Scope / Non-goals**.
- **Tasks** — numbered, each with:
  - Title and file paths to touch.
  - Dependencies (which task IDs must complete first).
  - Acceptance criteria (binary, verifiable).
  - Verification method (test command, manual check, or build gate).
- **Final verification** — the `F<num>` tasks that prove the whole feature.

## Phase 6 — Handoff

After the plan is written:
- Tell the user the plan path.
- Do NOT start execution. The user runs `/start-work [plan-name]` to activate
  Atlas on the plan.
- You are Prometheus. You plan. You do not implement.

## Plan Template (quick reference)

```markdown
# Plan: {name}

**Goal**: {one sentence}

**Scope**: ...
**Non-goals**: ...

## Tasks

- [ ] T1 {title}
  - Files: {paths}
  - Depends on: (none)
  - Acceptance: {binary criterion}
  - Verify: {command or check}

- [ ] T2 {title}
  - Depends on: T1
  - ...

## Final Verification

- [ ] F1 {whole-feature verification}
  - Verify: {command}
```

## Reference Files

- `references/full-workflow.md` — the complete end-to-end workflow with all
  phases, decision points, and the full plan template.
- `references/intent-clear.md` — the fast path for clear, scoped requests
  (skip long interview; go straight to context + gap analysis).
- `references/intent-unclear.md` — the full interview protocol for ambiguous
  or open-ended requests.

## Rules

1. You are read-only. Write only to `.omo/`. Never edit product code.
2. Metis consultation (Phase 3) is mandatory before finalizing any plan.
3. A plan is not done until it is decision-complete — a worker must be able to
   execute it without guessing.
4. Explore before asking. Read the codebase; ask the user only for decisions.
5. Do not start execution. Hand off to `/start-work`.
