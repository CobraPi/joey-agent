# Intent Clear — Fast Path

Use when intent is CLEAR (Phase 0): the request is specific, scoped, and
unambiguous. Skip the long interview; confirm scope, gather context, and go
straight to gap analysis.

## Clear-Intent Signals

- Specific files, modules, or features are named.
- The outcome is concrete and verifiable ("add a hello-world subcommand", not
  "improve the CLI").
- Scope is bounded — no obvious unstated requirement.
- Constraints are either stated or immaterial.

## Fast-Path Steps

### 1. Confirm scope (one round, not an interview)

State back to the user, in 1-3 sentences, what you understand the goal, scope,
and non-goals to be. Ask only for confirmation or correction. Do not run a
multi-round interview.

### 2. Gather codebase context

Fire `explore` and `librarian` subagents in parallel (background) for the
relevant areas. Read files directly when depth is needed.

### 3. Clearance checks

Verify the preconditions (Phase 2 of the main workflow):

- [ ] Intent clear (confirmed by the scope statement above).
- [ ] Codebase context gathered.
- [ ] No open ambiguity forcing a worker to guess.
- [ ] Dependencies and integration points identified.

### 4. Draft plan

Draft the plan directly. Move to Metis gap analysis.

## When to Escalate to the Unclear Path

Switch to `intent-unclear.md` mid-fast-path if:

- The scope confirmation reveals real ambiguity.
- Codebase exploration surfaces conflicting interpretations.
- A constraint is discovered that materially changes the outcome.

Do not force a clear-path plan through ambiguity. Escalate and interview.
