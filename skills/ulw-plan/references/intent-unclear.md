# Intent Unclear — Full Interview Protocol

Use when intent is UNCLEAR (Phase 0): the request is open-ended, ambiguous, or
underspecified. Run the full interview until intent is clear enough to plan.

## Unclear-Intent Signals

- "Improve X", "make it better", "refactor" without a target outcome.
- Multiple possible interpretations of the goal.
- Scope is unstated or unbounded.
- Key constraints (language, framework, compatibility) are unknown.
- The user describes a problem, not a solution.

## Interview Principles

1. **Explore before asking.** Fire `explore`/`librarian` (parallel,
   background) for codebase context. Only ask the user what repo evidence
   cannot resolve.
2. **One focused round at a time.** Ask a small, coherent set of questions.
   Let the user answer. Then decide if intent is now clear.
3. **Adopt best-practice defaults** for low-stakes unknowns rather than
   blocking. Record the adopted default so the user can override.
4. **Pursue decisions, not trivia.** Questions must resolve a real ambiguity
   that affects the plan. If a detail does not change the plan, adopt a
   default and move on.
5. **Stop interviewing when intent is clear.** Do not over-interview. Once
   goal + scope + constraints + non-goals are established, proceed.

## Interview Topics (cover in priority order)

### Round 1 — Goal & outcome

- What does "done" look like? What should be true after this work?
- How will we know it succeeded? (the acceptance signal)

### Round 2 — Scope boundary

- What is explicitly IN scope?
- What is explicitly OUT of scope (non-goals)?
- Is this one feature or several? Should it be split?

### Round 3 — Constraints

- Hard constraints: language, framework, existing dependencies.
- Backward compatibility: what must not break?
- Performance budgets, if relevant.
- Deadlines or sequencing pressures.

### Round 4 — Existing context (gather, do not ask)

- Explore the codebase for the relevant area.
- Identify integration points and existing patterns to follow.
- Ask the user only if the repo cannot answer (e.g., "is this internal tooling
  or customer-facing?").

## Exit Conditions

Exit the interview and move to clearance checks + Metis when ALL are true:

- [ ] Goal is a single, concrete outcome.
- [ ] Scope boundary is explicit (IN and OUT).
- [ ] Hard constraints are known or adopted as defaults.
- [ ] Non-goals are stated.
- [ ] No remaining ambiguity would force a worker to guess mid-implementation.

If after 3 rounds intent is still unclear, say so explicitly to the user and
ask them to restate the goal in one sentence. Do not manufacture a plan from
persistent ambiguity.

## After the Interview

Once intent is clear:

1. Run clearance checks (Phase 2).
2. Draft the plan.
3. Proceed to Metis gap analysis (Phase 4) — mandatory.
