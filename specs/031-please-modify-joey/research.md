# Research: Goal-Directed Task Execution (Feature 031)

**Date**: 2026-09-15

Grounded in repository facts gathered 2026-09-15 (file:line references verified read-only).

## R1 — Where the goal-directed guidance lands

**Decision**: Add a new `pub const GOAL_DIRECTED_GUIDANCE` in `crates/joey-agent-core/src/guidance.rs`, inserted into the stable tier of `build_system_prompt` (prompt.rs) immediately after `TASK_COMPLETION_GUIDANCE` (prompt.rs:809-811), gated by a new config key `agent.goal_directed_guidance` (default true), mirroring the gating pattern of `agent.adaptive_coding_guidance` / `agent.context_economy_guidance` (parity.rs fixtures demonstrate the pattern).

**Rationale**: Stable tier is built once per session and never re-rendered (cache-warmth constraint, AGENTS.md); guidance siblings live exactly there; config-gating with default-on matches the established additive-guidance pattern (features 027/028 precedent) and gives users an escape hatch to restore prior behavior, satisfying Principle VII.

**Alternatives considered**: (a) per-turn injection during the turn loop — rejected: violates the build-once constraint and adds per-turn cost (Principle VIII); (b) rewording only the existing `TASK_COMPLETION_GUIDANCE` — rejected as sole mechanism: it is verbatim-ported and model-family-independent, while the new framing needs its own constant for clean gating and contract tests; (c) volatile-tier placement — rejected: plan framing must be stable across the whole session, not context-dependent.

## R2 — Guidance content: what replaces open-ended framing

**Decision**: `GOAL_DIRECTED_GUIDANCE` carries the goal-oriented operational loop: before non-trivial work, state a concrete ordered plan of verifiable steps (proportionality rule: single-step tasks act directly); execute steps in order — every action serves the current step; reuse established results instead of re-deriving; on step failure/blocker, stop, report evidence, present revised plan or stop with status; on completion, report per-step outcomes and verification; re-state current plan state at step transitions so it survives context compression. Rewording of `TASK_COMPLETION_GUIDANCE` removes exploration-permissive phrasing ("look around", "digging"-style language) in favor of plan-execution-report framing, staying within its existing scope (no silent partial completion).

**Rationale**: Maps 1:1 to spec FR-001..FR-009; proportionality guard comes from User Story 1 Scenario 2; compression-survival from clarification Q1 (session-transient visible text re-stated at transitions).

**Alternatives considered**: (a) a separate `plan` tool forcing structured output — rejected for v1: spec assumes guidance-first delivery, no new user-facing tools; (b) multi-constant split (PLAN_GUIDANCE + EXECUTE_GUIDANCE + REPORT_GUIDANCE) — rejected: three gated sections triple config/testing surface for no proven benefit; single constant is leaner (Principle VIII).

## R3 — Predecessor-brand removal scope

**Decision**: Model- and user-facing strings only, exactly: `AGENT_HELP_GUIDANCE` attribution line (guidance.rs:14,17 — "based on Hermes Agent by Nous Research" + docs URL), `DEFAULT_SOUL_MD` persona (joey-core default_soul.rs:7), and the TUI banner line (joey-tui render.rs:1968 — "· based on Hermes Agent by Nous Research"). Retained deliberately: `hermes-0day` IOC string and HERMES env-var regex (threat-scan functional literals, not branding); real upstream model names (`hermes-3-405b` etc.) in provider registries and test fixtures; legacy SOUL.md template-detection literals (needed to detect/convert old files); `~/.hermes` home-directory compat; doc comments citing upstream Python files (audit records per spec assumption); `UPSTREAM_ATTRIBUTION` constant in branding.rs (license attribution retained — open-source license compliance; not shown to model/user as branding).

**Rationale**: FR-011 covers text shown to the user or sent to the model; functional identifiers, compatibility paths, license attribution, and engineering audit records are out of its scope by the spec's own Assumptions section. The existing `no_hermes_branding_in_model_visible_text` test (guidance.rs:300-325) already enforces this split with an allowlist of the two attribution strings — the feature removes exactly those two allowlisted strings and tightens the test to zero-allowlist.

**Alternatives considered**: (a) scrub all 'hermes' occurrences case-insensitively — rejected: breaks functional literals, compat paths, model registries, and license attribution; (b) rename `~/.hermes` compat handling — rejected: public on-disk contract (Hermes-compatible home must keep working, AGENTS.md); (c) remove UPSTREAM_ATTRIBUTION — rejected: MIT license attribution obligation.

## R4 — Token-neutrality verification (SC-007)

**Decision**: A deterministic test in `crates/joey-agent-core/tests/` compares the post-change system prompt — assembled live from fixed fixtures (all environment-dependent inputs pinned: fixed model/provider, empty skills index, no context files, no SOUL.md, no memory/user-profile blocks, no copilot) — against a committed pre-change assembled-prompt fixture (`specs/031-please-modify-joey/baseline/pre-change-prompt.txt`, captured during the pre-edit baseline window from the same pinned inputs), asserting `estimate_tokens(post) <= estimate_tokens(pre)` using public `joey_core::utils::estimate_tokens` (joey-core utils.rs:224, the established ~4 chars/token estimator already used for system_tokens accounting). The committed fixture is required because the predecessor-brand removals are not config-gated, so the pre-change prompt cannot be reconstructed by toggling a key at test time.

**Rationale**: `build_system_prompt` is environment-dependent (filesystem, env, clock), so the test must pin inputs; parity.rs `prompt_for` (parity.rs:139-149) already demonstrates deterministic fixture-based prompt construction. Byte-length token estimate is the same measure the agent itself uses for context accounting, making the budget operationally meaningful.

**Alternatives considered**: (a) real tokenizer crate — rejected: new dependency, Principle VIII violation for a relative-size assertion; (b) char-count comparison — rejected: estimator is the codebase's canonical measure and equally cheap; (c) manual audit of prompt diffs — rejected: not automated, SC-006 clarification demands automated verification.

## R5 — Baseline bundle for SC-001/SC-003/SC-004

**Decision**: Create `specs/031-please-modify-joey/baseline/` containing a task manifest (6 tasks, Markdown) and captured transcripts of the current version run against them (stored as session exports / transcript files), captured once before implementation lands, plus a scoring rubric (action-count, on-plan-action ratio, completion status) applied identically before/after.

**Rationale**: Spec clarification Q4 mandates a committed bundle of 5-10 tasks with once-captured current-version transcripts; Markdown manifest + rubric keeps it tooling-free and reviewable; 6 tasks span coding (2), analysis (2), writing (1), multi-file refactor (1) to cover the representative surface without ballooning effort.

**Alternatives considered**: (a) formal benchmark harness — rejected: spec assumption explicitly defers it ("no formal benchmark harness required for v1"); (b) ad-hoc rerun by hand — rejected: not reproducible; clarification Q4 chose the committed bundle.

## R6 — Test updates required by the wording changes

**Decision**: Update in lockstep: guidance.rs:344-347 (`identity_matches_seeded_soul` stays, trivially re-pointed if persona text changes); prompt.rs:1114-1130 section-order pin (replace the Hermes attribution key with the de-branded identity line, add GOAL_DIRECTED_GUIDANCE position); guidance.rs no-hermes test — remove the two allowlisted attribution strings, assert zero; parity.rs — add goal-directed on/off parity pair (guidance present when enabled, byte-identical to pre-feature prompt when disabled) mirroring the adaptive-coding precedent (parity.rs:536-539).

**Rationale**: These tests exist precisely to pin guidance wording; updating them alongside the change is the repo's established discipline ("tests assert exact prompt text deliberately, don't loosen without checking parity intent", AGENTS.md). New wording gets NEW pins, keeping the regression net intact (Principle VII).

**Alternatives considered**: deleting outdated assertions — rejected: erodes the parity net the constitution mandates.
