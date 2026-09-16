# Baseline Scoring Rubric — Feature 031

Score every transcript in `transcripts/` (pre) and `transcripts-post/` (post) with this rubric. All counting is done by the auditor reading the exported Markdown transcript.

## Metrics

### 1. Visible actions (SC-001 unit)

A **visible action** is one tool invocation or one assistant text message, counted separately and summed:

- +1 for every tool invocation in the transcript (each tool call block).
- +1 for every assistant text message (including the plan message, intermediate explanations, and the final report).
- Do not count: the user prompt, tool result blocks, system/status lines.

### 2. Plan-first (SC-002)

PASS if the first substantive assistant message states an ordered plan (numbered or bulleted steps) BEFORE the first tool invocation. FAIL otherwise. Judged on non-trivial tasks (all six manifest tasks qualify).

### 3. On-plan action ratio (SC-003)

on_plan = (# tool invocations that directly serve a stated plan step — or, if no plan was stated, an obvious core subtask of the prompt) / (total tool invocations). A tool invocation that re-derives an already-established result, or explores material unrelated to any stated step, counts as off-plan.

### 4. Task success (SC-004)

Compare the final transcript state against the task's Verifiable outcome in manifest.md: success / partial / fail.

### 5. Completion report & blocker statement (SC-005)

- completion_report: PASS if the final assistant message maps plan steps (or task components) to outcomes and states what verification was performed.
- blocker_statement: only for sessions that stopped incomplete — PASS if the final message explicitly states what is done, what is blocked, and why.

## Score table (fill one per transcript)

| Task | Tool invocations | Assistant messages | Visible actions | Plan-first | On-plan ratio | Success | Completion report | Blocker statement |
|------|------------------|--------------------|-----------------|------------|---------------|---------|-------------------|-------------------|
| 1    |                  |                    |                 |            |               |         | n/a               | n/a               |

## Verdict rules

- **SC-001**: for each task, post visible_actions <= 0.7 x pre visible_actions. Report per-task pass/fail and the overall count.
- **SC-002**: 100% of post transcripts PASS plan-first.
- **SC-003**: aggregate post on-plan ratio >= 0.90 (total on-plan tool invocations / total tool invocations across the six post transcripts).
- **SC-004**: post successes >= pre successes (per-task comparison; no task may drop from success to fail).
- **SC-005**: every post transcript that completed PASSes completion_report; every blocked one PASSes blocker_statement.
