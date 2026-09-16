# Post-change capture scores — Feature 031 (T018)

## Capture metadata

- **Capture date/time:** 2026-09-16 01:22–01:32 EDT (sessions); scored 01:36 EDT.
- **git rev-parse HEAD:** `461b7a8e6296b5bc475ee151b0ee7a53e231f3a0` — post-change feature-031 tree (`git status --porcelain crates/` shows 6 modified source files; `cargo build -p joey-cli` exit 0, cached, step 0).
- **SOUL.md migration:** old factory default verified (`grep -c "based on Hermes Agent by Nous Research"` → 1), backed up to `~/.joey/SOUL.md.pre-031.bak`, then overwritten with the new single-line content (487 bytes, one line + trailing newline).
- **Sessions (all exit 0, no retries):** 1 `20260916_012201_2ec742`, 2 `20260916_012323_afecd4`, 3 `20260916_012445_6584fa`, 4 `20260916_012532_6e7a2f`, 5 `20260916_012636_7fd94d`, 6 `20260916_012838_8bc618`. Model `glm-5.3` (config default, no overrides). Note: pre-change capture ran on `gpt-5.6-sol`/`ai-usage-hud` — the config default differs between capture points; both captures used their config default per protocol.
- Tool-call counts cross-checked against `sessions.tool_call_count` in the DB (7, 8, 4, 5, 2, 13 — all match the transcript blocks).

## Per-task rubric scores (post-change)

| Task | Tool invocations | Assistant messages | Visible actions | Plan-first | On-plan ratio | Success | Completion report |
|---|---|---|---|---|---|---|---|
| 1 | 7 | 5 | 12 | FAIL | 0.43 (3/7) | success | PASS |
| 2 | 8 | 5 | 13 | PASS | 0.38 (3/8) | success | PASS |
| 3 | 4 | 1 | 5 | FAIL | 0.75 (3/4) | success | PASS |
| 4 | 5 | 3 | 8 | FAIL | 1.00 (5/5) | success | PASS |
| 5 | 2 | 3 | 5 | FAIL | 1.00 (2/2) | success | PASS |
| 6 | 13 | 7 | 20 | PASS | 0.62 (8/13) | success | PASS |

Aggregate: 39 tool invocations, 24 assistant messages, 63 visible actions; plan-first 2/6; aggregate on-plan 24/39 ≈ 0.62; 6/6 success; 6/6 completion report.

## Success-criterion verdicts (pre: visible 12,13,7,8,10,24 — total 74; plan-first 1/6; on-plan 49/62 ≈ 0.79; success 6/6; completion 6/6)

- **SC-001 (post visible ≤ 0.7 × pre, per task; total ≤ 51): FAIL.** Per task: T1 12 ≤ 8.4? FAIL. T2 13 ≤ 9.1? FAIL. T3 5 ≤ 4.9? FAIL. T4 8 ≤ 5.6? FAIL. T5 5 ≤ 7? PASS. T6 20 ≤ 16.8? FAIL. Aggregate: 63 ≤ 51? **FAIL** (1/6 tasks pass).
- **SC-002 (6/6 plan-first): FAIL.** 2/6 (tasks 2 and 6 PASS; 1, 3, 4, 5 open with intent statements, not ordered plans).
- **SC-003 (aggregate on-plan ≥ 0.90): FAIL.** 24/39 ≈ 0.62.
- **SC-004 (no task drops success→fail): PASS.** 6/6 success, matching pre 6/6.
- **SC-005 (every completed task PASSes completion report): PASS.** All six completed; 6/6 PASS.

## Honest notes

- **No failed or killed sessions; no retries.** All six one-shot runs exited 0 on the first attempt.
- **Recurring re-verification pattern (tasks 1, 2, 6):** after a full completion report, the session received further turn(s) (the model itself references a "verification hook re-firing" / reacts with "Fair point") and re-ran verification — twice for tasks 1, 2, and 6. Per the rubric these re-runs re-derive established results and count off-plan, which is the main driver of the low on-plan ratios (e.g. task 1: only 2 writes + first unittest run are on-plan) and of the inflated visible-action totals for those tasks. Tasks 3, 4, 5 were unaffected.
- **Task 3 anomaly:** first tool call was a `skill_view` for a `codebase-inspection` skill that is not installed (error result); counted as off-plan exploration. The remaining three terminal commands produced all required counts.
- **Task 3 count differs from DB message rows:** 7 rows export as 1 assistant message because the opening assistant block is a tool-call carrier (`_(text omitted — tool-call message)_`), per the counting convention also used pre-change (pre task 3: 6 calls / 1 message).
- **Model drift between captures:** pre = `gpt-5.6-sol` (provider `ai-usage-hud`), post = `glm-5.3`. Both captures used the then-current config default with no `-m`/`--provider` overrides, per protocol; the comparison therefore spans both the guidance change and the model change, which SC-001–SC-003 results should be read against.
- Per-task visible actions still dropped 74 → 63 overall despite the re-verification inflation. Counting only self-chosen core actions (excluding all forced re-verification calls): T1 3 calls (2 writes + unittest), T2 3 (2 writes + node run), T6 8 (5 writes + smoke test + unittest + grep) — but the scored numbers above are the raw transcript counts per rubric, not adjusted.

## Pinned-model re-capture (2026-09-16, supersedes the capture above for SC-001..SC-003)

### Capture metadata

- **Capture date/time:** 2026-09-16 03:01–03:11 EDT (sessions); scored immediately after.
- **git rev-parse HEAD:** `461b7a8e6296b5bc475ee151b0ee7a53e231f3a0` — same post-change feature-031 tree as the unpinned capture above (`git status --porcelain crates/` shows the same 6 modified source files; same `./target/debug/joey` binary).
- **Model pinned:** `gpt-5.6-sol`, provider `ai-usage-hud`, via `-m gpt-5.6-sol --provider ai-usage-hud` on every session — matching the pre-change capture exactly, removing the model-drift confound noted above.
- **SOUL.md:** de-branded persona (0 occurrences of "based on Hermes Agent by Nous Research") — correct post-change state, unchanged from the unpinned capture.
- **Sessions (all exit 0, no retries, model verified `gpt-5.6-sol` in the DB for every id):** 1 `20260916_030159_544ad8`, 2 `20260916_030323_d360cf`, 3 `20260916_030431_dea6de`, 4 `20260916_030547_ac5b23`, 5 `20260916_030656_fd66dd`, 6 `20260916_031035_1f86e4`.
- Tool-call counts cross-checked against `sessions.tool_call_count` in the DB (13, 9, 5, 8, 7, 25 — all match the transcript blocks). Transcripts: `transcripts-post/task-N-pinned.md`.

### Per-task rubric scores (post-change, model pinned)

| Task | Tool invocations | Assistant messages | Visible actions | Plan-first | On-plan ratio | Success | Completion report | Blocker |
|---|---|---|---|---|---|---|---|---|
| 1 | 13 | 10 | 23 | PASS | 0.54 (7/13) | success | PASS | n/a |
| 2 | 9 | 7 | 16 | PASS | 0.33 (3/9) | success | PASS | n/a |
| 3 | 5 | 5 | 10 | PASS | 0.60 (3/5) | success | PASS | n/a |
| 4 | 8 | 4 | 12 | PASS | 1.00 (8/8) | success | PASS | n/a |
| 5 | 7 | 4 | 11 | PASS | 1.00 (7/7) | success | PASS | n/a |
| 6 | 25 | 10 | 35 | PASS | 0.64 (16/25) | success | PASS | n/a |

Aggregate: 67 tool invocations, 40 assistant messages, 107 visible actions; plan-first 6/6; aggregate on-plan 44/67 ≈ 0.66; 6/6 success; 6/6 completion report.

### Success-criterion verdicts (pre: visible 12,13,7,8,10,24 — total 74; plan-first 1/6; on-plan 49/62 ≈ 0.79; success 6/6; completion 6/6)

- **SC-001 (post visible ≤ 0.7 × pre, per task; total ≤ 51): FAIL.** Per task: T1 23 ≤ 8.4? FAIL. T2 16 ≤ 9.1? FAIL. T3 10 ≤ 4.9? FAIL. T4 12 ≤ 5.6? FAIL. T5 11 ≤ 7? FAIL. T6 35 ≤ 16.8? FAIL. Aggregate: 107 ≤ 51? **FAIL** (0/6 tasks pass; pinned total is 1.44× the pre total, worse than pre, not better).
- **SC-002 (6/6 plan-first): PASS.** All six transcripts open with a numbered plan (with done criteria on tasks 3, 4, 6) before the first tool invocation — a decisive shift from pre 1/6.
- **SC-003 (aggregate on-plan ≥ 0.90): FAIL.** 44/67 ≈ 0.66.
- **SC-004 (no task drops success→fail): PASS.** 6/6 success, matching pre 6/6.
- **SC-005 (every completed task PASSes completion report): PASS.** All six completed; 6/6 PASS.

### Honest notes (pinned re-capture)

- **No failed or killed sessions; no retries.** All six one-shot runs exited 0 on the first attempt, and every session's DB model row printed `gpt-5.6-sol`.
- **Same recurring re-verification pattern as the unpinned capture, but stronger:** tasks 1, 2, and 6 each received TWO post-report verification-hook turns (re-reads, reruns, `py_compile`/`compileall`, independent assertions) after a full completion report; tasks 3, 4, 5 were unaffected. Per the rubric these re-runs re-derive established results and count off-plan — the main driver of the low on-plan ratios and the high visible totals.
- **Task 3 anomaly (recurred from unpinned):** first tool call was a `skill_view` for the uninstalled `github:codebase-inspection` skill (error result); a second `skill_view` loaded `joey-agent-project-context`, which contributed nothing to the counting. Both counted off-plan.
- **Task 5 corrective iterations:** first draft measured 125 lines, second 114, final 87 (within 60–90) — the rewrites are corrective iterations serving the stated verify-line-count step, so all 7 calls count on-plan.
- **Task 4 counting convention:** the final all-reference completeness pass is counted on-plan, matching the pre-change task 4 appendix (pre 7/7 included its analogous verification pass).
- **Model-specific behavior differences vs the unpinned capture:** `gpt-5.6-sol` used `todo` tool bookkeeping (tasks 1 and 6; 7 calls total — 4 in T1, 3 in T6, all counted on-plan) and always opened with an explicit numbered plan; `glm-5.3` (unpinned) did neither. This inflates the pinned visible-action totals relative to unpinned independent of the guidance change.
- **Reading of the confound removal:** under the pinned model the guidance change did NOT reduce visible actions (107 vs pre 74; SC-001 fails 0/6, worse than the unpinned capture's 1/6) — the 74 → 63 reduction in the unpinned capture above is attributable to the model switch, not to the guidance change. SC-002 (6/6 plan-first vs pre 1/6) is confirmed and strengthened under the pinned model; SC-003 remains FAIL in both captures (0.66 pinned, 0.62 unpinned vs pre 0.79), driven by forced post-report re-verification turns rather than by self-chosen exploration.

## T024 re-capture (2026-09-16, post nudge-suppression fix; supersedes pinned capture above)

### Capture metadata

- **Capture date/time:** 2026-09-16 04:09–04:17 EDT (sessions); scored immediately after.
- **git rev-parse HEAD:** `461b7a8` — feature-031 tree plus the T024 nudge-suppression fix; binary is the prebuilt `./target/debug/joey` (NO cargo run per instruction).
- **Model pinned:** `gpt-5.6-sol`, provider `ai-usage-hud`, via `-m gpt-5.6-sol --provider ai-usage-hud` on every session — identical to the pre-change and prior pinned captures.
- **SOUL.md:** de-branded persona, left as-is (correct post-change state).
- **Sessions (all exit 0, no retries, model verified `gpt-5.6-sol` in the DB for every id):** 1 `20260916_040921_27bfe1`, 2 `20260916_041039_b33508`, 3 `20260916_041121_f7dc8c`, 4 `20260916_041341_338ee0`, 5 `20260916_041455_8b0730`, 6 `20260916_041653_8db955`.
- Tool-call counts cross-checked against `sessions.tool_call_count` in the DB (9, 7, 11, 5, 9, 16 — all match the transcript blocks). Transcripts: `transcripts-post/task-N-pinned2.md`.

### Per-task rubric scores (post-change, T024 nudge fix, model pinned)

| Task | Tool invocations | Assistant messages | Visible actions | Plan-first | On-plan ratio | Success | Completion report | Blocker |
|---|---|---|---|---|---|---|---|---|
| 1 | 9 | 5 | 14 | PASS | 0.67 (6/9) | success | PASS | n/a |
| 2 | 7 | 4 | 11 | PASS | 0.43 (3/7) | success | PASS | n/a |
| 3 | 11 | 7 | 18 | PASS | 0.73 (8/11) | success | PASS | n/a |
| 4 | 5 | 3 | 8 | PASS | 1.00 (5/5) | success | PASS | n/a |
| 5 | 9 | 5 | 14 | PASS | 1.00 (9/9) | success | PASS | n/a |
| 6 | 16 | 7 | 23 | PASS | 0.75 (12/16) | success | PASS | n/a |

Aggregate: 57 tool invocations, 31 assistant messages, 88 visible actions; plan-first 6/6; aggregate on-plan 43/57 ≈ 0.75; 6/6 success; 6/6 completion report.

### Success-criterion verdicts (pre: visible 12,13,7,8,10,24 — total 74; plan-first 1/6; on-plan 49/62 ≈ 0.79; success 6/6; completion 6/6)

- **SC-001 (post visible ≤ 0.7 × pre, per task; total ≤ 51): FAIL.** Per task: T1 14 ≤ 8.4? FAIL. T2 11 ≤ 9.1? FAIL. T3 18 ≤ 4.9? FAIL. T4 8 ≤ 5.6? FAIL. T5 14 ≤ 7? FAIL. T6 23 ≤ 16.8? FAIL. Aggregate: 88 ≤ 51? **FAIL** (0/6 tasks pass; improved from the prior pinned 107 → 88 but still 1.19× the pre total).
- **SC-002 (6/6 plan-first): PASS.** All six transcripts open with a numbered plan (done criteria on 3, 4, 6) before the first tool invocation — unchanged from the prior pinned capture.
- **SC-003 (aggregate on-plan ≥ 0.90): FAIL.** 43/57 ≈ 0.75 (improved from prior pinned 0.66; still below pre 0.79 and the 0.90 bar).
- **SC-004 (no task drops success→fail): PASS.** 6/6 success, matching pre 6/6.
- **SC-005 (every completed task PASSes completion report): PASS.** All six completed; 6/6 PASS.

### Before/after vs the PRIOR pinned capture (23/16/10/12/11/35 = 107 visible)

| Task | Prior pinned visible | T024 visible | Delta | Post-report re-verification rounds (prior → now) |
|---|---|---|---|---|
| 1 | 23 | 14 | −9 | 2 → 1 |
| 2 | 16 | 11 | −5 | 2 → 1 |
| 3 | 10 | 18 | +8 | 0 → 0 |
| 4 | 12 | 8 | −4 | 0 → 0 |
| 5 | 11 | 14 | +3 | 0 → 0 |
| 6 | 35 | 23 | −12 | 2 → 1 |
| **Total** | **107** | **88** | **−19** | **6 → 3** |

The post-report re-verification rounds did NOT fully disappear: tasks 1, 2, and 6 each still performed ONE full re-verification round (re-reads, reruns, `py_compile`/`compileall`, source re-searches) after an already-compliant completion report — halved from two rounds each, not eliminated. What did disappear: every "Fair point" / "verification hook re-firing" reaction — the remaining rounds read as self-initiated ("I'll re-read both changed files, then rerun…") with no acknowledgment of an external nudge, and the exports never displayed the synthetic user turn either way, so the behavioral evidence is the round count and the wording.

### Honest notes (T024 re-capture)

- **No failed or killed sessions; no retries.** All six one-shot runs exited 0 first try; every session's DB model row printed `gpt-5.6-sol`.
- **Task 3 anomaly (+8 visible):** self-chosen diligence, not nudges — two `skill_view` calls (one for the uninstalled `github:codebase-inspection`, one redundant project-context load), a physical-LOC pass, then a Pygount detour that hit a CP1252 API encoding failure, a `--help` usage dump, and a discarded CLI-per-crate attempt before the final verified run. Prior pinned T3 (5 calls) happened to go straight to counts; this run's exploration is model variance against the same environment.
- **Task 5 anomaly (+3 visible):** three draft iterations this time (125 → 105 → 83 lines) vs two in the prior pinned capture (125 → 114 → 87); all corrective rewrites count on-plan per the established convention, so on-plan stayed 1.00 but visible rose.
- **Net reading:** the −19 visible delta is concentrated exactly in the three nudge-affected tasks (T1/T2/T6, −26 combined) and partially offset by unrelated exploration variance in T3/T5 (+11). On-plan aggregate rose 0.66 → 0.75 for the same reason. SC-001/SC-003 remain FAIL against the pre-change numbers; the fix's measurable effect is confined to halving (not removing) post-report re-verification rounds and removing the visible "reacting to a nudge" wording.
