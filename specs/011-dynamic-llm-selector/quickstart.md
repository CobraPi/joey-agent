# Quickstart: Dynamic LLM Model Selector

**Feature**: 011-dynamic-llm-selector | **Phase**: 1 (validation guide)

Runnable validation scenarios proving the feature works end-to-end. This is a
**run/validation guide**, not an implementation doc — implementation details
belong in `tasks.md`. Refer to [data-model.md](./data-model.md) and
[contracts/](./contracts/) for shapes.

---

## Prerequisites

- A working `joey` build from this branch:
  `cargo build --workspace` (Constitution VII green).
- A provider configured with a **live model catalog** — GitHub Copilot or
  OpenRouter are the canonical catalog-exposing providers (research.md §6).
  Set it up via `joey setup` or `joey auth login` so `/models` is reachable.
- (Optional) A second catalog model from a different tier to observe genuine
  per-module allocation diversity.

## Build & test gates (Constitution IV, VII)

Before running the scenarios:

```bash
# New crate builds and tests in isolation (Constitution I, IV)
cargo build -p joey-llm-selector
cargo test  -p joey-llm-selector

# Whole workspace stays green (Constitution VII)
cargo build --workspace
cargo test  --workspace
```

**Expected**: all green. Key tests to confirm exist (tasks phase will
materialize them):
- `joey-llm-selector`: scorer unit tests, allocation-map round-trip
  (`map_round_trip.rs`), stale-entry re-resolve, diagnoser budget bounds.
- `joey-agent-core`: feature-off regression at the two intercepts (main turn +
  compression) — model id unchanged when selector inactive.
- `joey-cli`: `/llm-selector` dispatch + exit codes.

---

## Scenario 1 — Engage via `auto` and inspect (User Story 1, P1)

Proves: the `auto` model activates dynamic allocation; `/llm-selector` reports
state and candidate pool.

1. Start `joey` on a catalog-exposing provider.
2. Select the `auto` model:
   ```
   joey model
   ```
   (pick `auto`, or set `model.model: auto` in config).
3. Run:
   ```
   /llm-selector status
   ```
4. **Expected output** (shape per contracts/llm-selector-command.md):
   - `LLM Selector: enabled`
   - `Candidate pool: N chat-capable models (source: copilot|openrouter)` with
     `N >= 1`
   - an active `Diagnoser model: <id> (versatile)`
   - an `Allocations:` block listing `main_turn`, `compression`, `subagent`.
5. Run `/llm-selector pool` → **Expected**: the full list of chat-capable
   catalog models (SC-005: 100% of eligible models are considered, none
   silently excluded).

**Pass criterion**: selector reports enabled with a non-empty pool and has
populated cold-start allocations.

---

## Scenario 2 — Per-module allocation actually happens (User Story 2, P1)

Proves: distinct modules can run on different models in the same turn
(SC-002); allocated models satisfy hard capability gates (SC-004).

1. Engage the selector (Scenario 1).
2. Run a turn that exercises at least two modules — e.g. a turn long enough to
   trigger **history compression** alongside the **main turn** (send a large
   message, or several messages, then continue). A subagent delegation
   (`/delegate ...`) exercises the third module.
3. Inspect:
   ```
   /llm-selector allocations
   ```
4. **Expected**:
   - Each module's `model_id` is an explicitly chosen catalog model (not a
     blanket default), with a `reason` containing `cold-start` or `diagnoser`.
   - On a catalog with multi-tier models, at least two modules show **different**
     `model_id`s (SC-002). (If the catalog has only one eligible model, the
     selector reports no-op pass-through per the Edge Case — that is also valid.)
5. Send a message with an **image** → **Expected**: the `main_turn` allocation
   resolves to a model whose `supports_vision` is true (SC-004). Run
   `/llm-selector pool` to confirm the chosen model has the vision flag.

**Pass criterion**: genuine per-module allocation with capability gating, and
a visible selection reason per module.

---

## Scenario 3 — Learn and refine over time (User Story 3, P2)

Proves: the diagnoser runs within budget on observable failure and reallocates
toward better estimated performance (SC-003).

1. Engage the selector with a small budget:
   ```
   /llm-selector budget 4
   ```
2. Trigger an observable failure signal — e.g. force an **empty/null response**
   or a **retry** (a misconfigured/removed model id in the pool, or a rate
   limit). The diagnoser triggers ONLY on failure (FR-009): successful turns
   do not fire it.
3. Wait for the detached diagnoser task to complete (it does not block the
   turn; check asynchronously).
4. Inspect:
   ```
   /llm-selector diagnostics
   /llm-selector allocations
   ```
5. **Expected**:
   - `diagnostics` shows a record with the `signal`, the `implicated_model`,
     and a natural-language `rationale` (FR-018).
   - At least one module's allocation changed, with `reason` referencing the
     diagnoser and an `estimated_performance` value now in `[0,1]` (SC-003).
   - `budget_used_this_cycle` incremented and `<= learning_budget` (SC-003).

**Pass criterion**: bounded, failure-triggered learning produced a visible
reallocation with recorded rationale.

---

## Scenario 4 — Pin & override (User Story 4, P2)

Proves: a user pin is persisted, applied immediately, and exempt from
reallocation (SC-008).

1. Engage the selector. Pin a module:
   ```
   /llm-selector pin compression claude-haiku-4-5
   ```
2. Run the learning step (Scenario 3).
3. Inspect `/llm-selector allocations`.
4. **Expected**:
   - `compression` shows `pinned` and its `model_id` is exactly
     `claude-haiku-4-5` — unchanged by the diagnoser.
   - Other (unpinned) modules may have changed.
5. Unpin:
   ```
   /llm-selector unpin compression
   ```
   → next learning run may reallocate `compression` again.
6. Restart `joey` → **Expected**: the pin survived restart (persisted in
   `~/.joey/llm-selector/allocations.json`).

**Pass criterion**: pins are durable and honored over the learning loop.

---

## Scenario 5 — Graceful degradation (User Story 5, P3)

Proves: catalog failure / removed model → graceful fallback, turn completes
(SC-007); no unroutable model id is ever sent.

1. Engage the selector.
2. Simulate catalog failure: temporarily revoke network / point the provider
   endpoint at an unreachable host, OR manually edit a stale `model_id` into
   `~/.joey/llm-selector/allocations.json` that is absent from the live
   catalog.
3. Run a turn.
4. **Expected**:
   - The turn completes (no crash). The affected module falls back to a
     feasible model (last-known-good, then provider `fallback_models`, then
     `cfg.model()`).
   - `/llm-selector status` reports the fallback / degraded state with a
     notice; a `diagnostics` record may reference the stale id.
   - No API call is ever made with the bad/absent model id (SC-007). Verify
     with `JOEY_LOG=trace` if needed: every outgoing request carries a model
     id present in the live catalog.

**Pass criterion**: robustness — failure modes complete the turn via fallback.

---

## Scenario 6 — Disable / switch away (User Story 1, acceptance 3)

Proves: toggling is non-destructive and takes effect on the next turn.

1. Engage the selector (Scenario 1). Run a turn.
2. Disable:
   ```
   /llm-selector disable
   ```
3. Run another turn.
4. **Expected**:
   - Every module now uses the literal configured model (or the user's
     concrete model selection).
   - Prior conversation messages are byte-identical — nothing mutated,
     reordered, or supplemented (SC-006). The system prompt is unchanged.
5. Switch the active model to a concrete id (`/model gpt-4o`) → same fallback
   behavior.

**Pass criterion**: clean on/off with zero conversation mutation.

---

## Scenario 7 — Cross-profile map sharing (FR-014)

Proves: the allocation map is global across profiles.

1. Engage + learn under profile A (`joey -p A`).
2. Switch to profile B (`joey -p B`) on the same machine, same provider.
3. `/llm-selector allocations`.
4. **Expected**: the allocations learned under profile A are visible and
   applied (the map lives under `process_joey_home()`, not per-profile). Any
   entry whose model is absent from profile B's catalog is re-resolved via
   cold-start before use.

**Pass criterion**: learning transfers across profiles; no unavailable id sent.

---

## What is explicitly NOT validated here

- Implementation code (see `tasks.md` once generated).
- Full unit/contract test suite text (see `cargo test -p joey-llm-selector`).
- The exact model ids in your catalog (provider/account-specific).
- The four spec-named modules that don't exist as LLM call sites today
  (vision/title/web-extract/curator) — they are tools/stubs (research.md §2);
  the selector's real surface is `main_turn` + `compression` + `subagent`.

---

## Validation results (T058)

Run 2026-08-05 via test doubles (the automated test suite), since no live
catalog-exposing provider (Copilot/OpenRouter) with credentials is configured
in this environment. The task explicitly permits "via test doubles". Each
scenario's pass criterion is exercised by the named tests; all pass.

**Build & test gates (prerequisite)**:
- `cargo build -p joey-llm-selector` — PASS
- `cargo build --workspace` — PASS (green)
- `cargo test -p joey-llm-selector` — 66 passed, 0 failed, 0 warnings
- `cargo test -p joey-agent-core feature011` — 4 passed, 0 failed
- `cargo test -p joey-cli llm_selector` — 6 passed, 0 failed
- `cargo test --workspace` (excluding 4 pre-existing slow tests in
  `joey-cli` render.rs and `joey-tools` vcs.rs, both unrelated to feature 011)
  — all green, 0 failures across all crates.

| Scenario | Pass criterion | Test-double coverage | Result |
|---|---|---|---|
| 1 — Engage via `auto` + inspect (US1) | selector reports enabled + non-empty pool + cold-start allocations | `try_build_allocator_some_when_auto_active`, `test_is_active_true_after_set_pool`, `test_auto_disable_on_empty_pool`, `test_render_no_catalog` | PASS |
| 2 — Per-module allocation (US2) | ≥2 modules differ; capability gating; vision enforced | `test_never_assigns_incapable`, `test_satisfies_capability_gates`, `test_context_window_returns_pool_max` (allocator) | PASS |
| 3 — Learn & refine (US3) | diagnoser runs within budget on failure, reallocates ≥1 module | `test_learning_loop_uses_judge_when_present`, `test_learning_loop_falls_back_to_heuristic_when_judge_none`, `test_diagnoser_reallocates_on_failure`, `test_append_diagnostic_persists_and_increments_budget`, `test_diagnostics_ring_buffer_trim` | PASS |
| 4 — Pin & override (US4) | pins durable + honored over learning | `test_diagnoser_respects_pins`, `test_diagnoser_respects_implicit_pins`, map round-trip (`test_round_trip_preserves_entries`) covers pin persistence across restart | PASS |
| 5 — Graceful degradation (US5) | catalog failure/removed model → fallback completes turn; no stale id sent | `test_catalog_failure_completes_via_fallback`, `test_removed_model_reresolves_to_live_catalog_model`, `test_degraded_fallback_uses_fallback_models`, `test_degraded_fallback_falls_to_configured`, `test_report_permanent_error_reresolves_live_model` | PASS |
| 6 — Disable / switch away (US1 AC3) | clean on/off, zero conversation mutation | `feature011_prompt_and_history_stable_across_toggle`, `feature011_no_allocator_uses_configured_model_verbatim`, `feature011_inactive_allocator_falls_back_to_configured_model`, `try_build_allocator_none_when_disabled_and_not_auto` | PASS |
| 7 — Cross-profile map sharing (FR-014) | map global; learned allocations visible cross-profile; stale re-resolved | map path uses `process_joey_home()` (map.rs); stale re-resolve covered by `test_stale_entry_reresolved`; pin round-trip (`test_round_trip_preserves_entries`) | PASS |

**Notes**:
- Scenarios requiring an interactive REPL with a live catalog (`/llm-selector
  status` output shape, the `joey model` picker, sending a real image) are
  validated structurally via the command-contract tests
  (`llm_selector_help_succeeds`, `llm_selector_unknown_subcommand_errors`)
  and the underlying engine tests above, not by a literal REPL session.
- The two spec-named slow tests excluded from the workspace run
  (`render::tests::*_completes_under_full`, `vcs::tests::prune_*`) are
  pre-existing and unrelated to feature 011; feature-011 paths are fully
  covered by the per-crate runs above.
