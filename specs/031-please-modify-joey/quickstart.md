# Quickstart: Validating Goal-Directed Task Execution (Feature 031)

**Date**: 2026-09-15

Runnable validation scenarios proving the feature end-to-end. Prerequisites: a built workspace (`cargo build --workspace`) on branch `031-please-modify-joey` after implementation lands.

## Scenario 1 — Guidance contract & brand hygiene (automated)

```bash
cargo test -p joey-agent-core --lib guidance   # matches-contract + no-brand tests
cargo test -p joey-agent-core --lib prompt      # golden/section-order pins
cargo test -p joey-agent-core --test parity     # on/off parity + token-neutrality
```

**Expected**: all green — GOAL_DIRECTED_GUIDANCE matches its contract; zero 'hermes' across model-visible guidance; goal-directed-on prompt contains the new section; goal-directed-off prompt byte-identical to pre-feature; post-change estimated tokens <= pre-change.

## Scenario 2 — Banner & persona de-brand (automated)

```bash
cargo test -p joey-tui render
```

**Expected**: green; banner carries no predecessor-brand line. Manual echo: `cargo run -p joey-cli -- --help` shows no predecessor-brand text.

## Scenario 3 — Config gate round-trip (manual)
```bash
joey config set agent.goal_directed_guidance false   # or edit config.yaml
# start a session, confirm guidance absent (system prompt via /status or debug)
joey config set agent.goal_directed_guidance true
# confirm guidance present again
```

**Expected**: off = pre-feature behavior restored; on = goal-directed framing in effect.

## Scenario 4 — Behavioral smoke test (manual, scored)

Run 2-3 tasks from `baseline/manifest.md` against a fresh session; score with `baseline/rubric.md`: plan stated before work; actions map to steps; completion report per step. Compare against the captured baseline transcripts for the same tasks.

**Expected**: plan-before-action on non-trivial tasks; straight-line execution; per-step completion report; measurably fewer visible actions than baseline (target >=30%, SC-001).

## References

- Spec: [spec.md](spec.md) | Plan: [plan.md](plan.md) | Contracts: [contracts/config-key.md](contracts/config-key.md), [contracts/system-prompt.md](contracts/system-prompt.md) | Data model: [data-model.md](data-model.md)
