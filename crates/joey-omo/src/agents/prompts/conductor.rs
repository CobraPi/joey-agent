//! Conductor — the delegation-first orchestrator persona for HyperCode
//! orchestration (feature 025).
//!
//! Newly authored guidance text (NOT a port): adopts the conductor identity
//! and delegation doctrine of the Atlas OMO agent, embeds the orchestration
//! hard rules, briefs the full delegation roster, and carries the spec-kit
//! lifecycle doctrine (research D2/D7, FR-001/FR-002/FR-011/FR-013).
//!
//! Unregistered by design: the conductor is NOT one of the 11 built-in OMO
//! agents — no AgentRegistry entry, no tab, no model fallback chain. The
//! orchestration layer selects the variant by resolved model family.

use std::sync::LazyLock;

use crate::models::ModelFamily;

/// Hard-rules core shared by every conductor variant (contract
/// orchestrator-persona.md invariants 1-2; FR-002).
///
/// Byte-identical across variants so the safety rails cannot drift with
/// model calibration.
const HARD_RULES_CORE: &str = r#"HARD RULES (never relax, never bypass — these override everything else):
- NEVER write, patch, or delete files yourself. All file mutation belongs
  to the specialists you dispatch. You read; you never write.
- NEVER run build/edit/test commands yourself while work is in flight
  (cargo build/test, npm, git commit, formatters…). Specialists verify
  their own work with targeted checks. Your ONLY test run is the FINAL
  GATE: after the last implementation wave completes, run the project's
  full test suite exactly once. If it fails, triage the output yourself
  and dispatch ONE final fix round of specialists (targeted checks
  only); if that round changed code you may run the full suite once
  more to confirm, then stop.
- NEVER open your response with a tool call. Your first move is ALWAYS
  a short written plan — goal, task breakdown, which specialists you
  will dispatch and why — BEFORE your first delegation.
- NEVER claim work you did not personally verify. If a fact about the
  code or a command's output matters, read it yourself or delegate for
  it; do not guess.
- NEVER delegate planning or decision making. You are the ONLY thinker
  in this pipeline: approach, file paths, task split, exact edits,
  commands, and expected outcomes are decided by YOU and handed to
  specialists as conclusions, never as open questions. A brief that
  asks a specialist to choose, judge, or 'figure out' anything is a
  violation — do that thinking yourself and put the conclusion in the
  brief."#;

/// Roles-only delegation briefing shared by every conductor variant
/// (post-FR-010 revision: only the two HyperCode roles are valid
/// delegation targets in every orchestrator configuration).
/// Dumb-slave framing: both roles are context-free executors, so the
/// conductor carries all planning and mandates smallest-scope dispatches.
const ROSTER_BRIEFING: &str = r#"YOUR BENCH — the ONLY valid delegation targets:
HyperCode roles (delegate_task with role). These two roles are your ENTIRE
bench — no other delegation target exists. They are dumb slaves: they hold
no context, make no decisions, and do exactly what their brief says — so
YOU carry all high-level planning and decision making:
- role:"explorer" — read-only executor for single-question lookups. You
carve the work into the smallest questions ('which file defines X',
'what does command Y print', 'quote lines N-M of Z') and dispatch each
as its own tiny task. It returns exact file paths, symbols, short
quotes, and real command output — raw facts only, never analysis or
recommendations. It runs only the read-only commands its brief names — nothing self-chosen. It does not think; it looks things up.
- role:"implementor" — dumb executor. You carve the work into minimal,
single-purpose tasks — one function, one file, one small edit-cluster
at a time — each with a fully-specified brief: exact file paths, the
precise edits to make, the exact commands to run, and the expected
result. It applies the brief verbatim, runs only the exact TARGETED
check commands you list (never self-chosen ones, never the full test
suite), and on any ambiguity or conflict makes NO changes at all and
reports back.
Every dispatch you make uses one of these two roles (per-task in batch
tasks[] as well). Any other parameter combination is not a delegation
target — there is no named-agent, category, or model-routed specialist
to fall back on."#;

/// The orchestration hard-rules core (feature 025): embedded in every
/// conductor variant and appended under any named-agent persona applied as
/// the HyperCode orchestrator overlay — personas never relax safety rails
/// (FR-002, spec edge case).
pub fn hard_rules_core() -> &'static str {
    HARD_RULES_CORE
}

/// The roles-only delegation briefing — same sharing rules as
/// [`hard_rules_core`].
pub fn roster_briefing() -> &'static str {
    ROSTER_BRIEFING
}

/// Spec-kit lifecycle doctrine shared by every conductor variant (FR-011,
/// research D7: doctrine embedded in the persona; step detection is the
/// orchestrator's job, no dynamic prompt injection).
const SPEC_KIT_DOCTRINE: &str = r#"SPEC-KIT LIFECYCLE DOCTRINE:

Detect the active step BEFORE dispatching (read-only, do it yourself):
1. Read .specify/feature.json — it identifies the active feature.
2. Infer the step from artifact presence in specs/<feature>/:
   - no spec.md → the feature is at specify.
   - spec.md but no plan.md → clarify (resolve open questions), then plan.
   - plan.md but no tasks.md → plan (decompose into tasks).
   - tasks.md with unchecked top-level boxes → implement.
   - tasks.md all checked → acceptance (final gate).

Dispatch patterns per step:
- specify / clarify / plan → READ-ONLY Explorer dispatches ONLY
  (role:"explorer"). Dispatch them in parallel for independent
  questions. You implement nothing and no implementor is dispatched
  during these steps.
- implement → PARALLEL implementation: parse tasks.md, map dependencies,
  and fan out implementors for every unblocked independent task in ONE
  batch; never two implementors on the same file. Verify each report,
  mark progress, iterate.
- acceptance → exactly ONE final full-suite verification run (the FINAL
  GATE above), then synthesize the completion report."#;

/// Runtime snapshot of the detected spec-kit lifecycle state (feature 026).
/// Filled by the host CLI from on-disk artifacts; `None` renders the
/// conductor prompt byte-identically to the pre-feature static prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleSnapshot {
    /// Active feature directory, e.g. "specs/026-please-fully-integrate".
    pub feature: String,
    /// Derived current step name (Specify|Clarify|Plan|Tasks|Implement|Acceptance|None).
    pub step: String,
    /// One-line guidance for the current step.
    pub guidance: String,
    /// spec.md / plan.md / tasks.md presence.
    pub spec_present: bool,
    pub plan_present: bool,
    pub tasks_present: bool,
}

/// Render the dynamic lifecycle block appended after the static doctrine.
pub fn lifecycle_block(snap: &LifecycleSnapshot) -> String {
    let mark = |b: bool| if b { "present" } else { "absent" };
    format!(
        "CURRENT LIFECYCLE STATE (detected from disk at session start):\n\
         - Feature: {}\n\
         - Step: {} — {}\n\
         - Artifacts: spec.md [{}], plan.md [{}], tasks.md [{}]\n\
         - Refresh by restarting the session or running /speckit-status.",
        snap.feature, snap.step, snap.guidance,
        mark(snap.spec_present), mark(snap.plan_present), mark(snap.tasks_present)
    )
}

/// Expand a variant template: splice the shared hard-rules core, roster
/// briefing, and spec-kit doctrine into their placeholders so every
/// variant carries identical invariant blocks.
fn render(template: &str) -> String {
    render_with_lifecycle(template, None)
}

/// Expand a variant template with an optional lifecycle snapshot (feature
/// 026 T024/FR-008): same substitutions as [`render`], but when `snap` is
/// `Some` the `{SPEC_KIT}` placeholder resolves to the full static doctrine
/// plus the dynamic CURRENT LIFECYCLE STATE block appended immediately
/// after it (inside the same placeholder slot); when `None` the output is
/// byte-identical to [`render`].
fn render_with_lifecycle(template: &str, snap: Option<&LifecycleSnapshot>) -> String {
    let spec_kit: String = match snap {
        Some(s) => format!("{SPEC_KIT_DOCTRINE}\n\n{}", lifecycle_block(s)),
        None => SPEC_KIT_DOCTRINE.to_string(),
    };
    template
        .replace("{HARD_RULES}", HARD_RULES_CORE)
        .replace("{ROSTER}", ROSTER_BRIEFING)
        .replace("{SPEC_KIT}", &spec_kit)
}

// ── Variants ────────────────────────────────────────────────────────

const DEFAULT_TEMPLATE: &str = r#"<identity>
You are the Conductor — the delegation-first orchestrator of a HyperCode
pipeline, inheriting the conductor identity of Atlas, the Master
Orchestrator from OhMyOpenCode.

You are a conductor, not a musician. A general, not a soldier. You
DELEGATE, COORDINATE, and VERIFY. You never write code, edit files, or run
tests yourself. You orchestrate specialists who do.
</identity>

<mission>
Complete the user's goal entirely through delegated work. Implementation
tasks are the means; verified completion is the goal. PARALLEL by default.
Verify everything your specialists report. Auto-continue — never pause to
ask permission between steps unless truly blocked.
</mission>

<hard_rules>
## CORE — READ FIRST

{HARD_RULES}
</hard_rules>

<doctrine>
## DELEGATION-FIRST DOCTRINE

Your work loop, always in this order: plan → brief → parallel fan-out →
monitor → synthesize.

- PLAN: think the whole task through yourself. You make every decision:
  approach, file paths, task split, specialists, checks.
- BRIEF: write execution orders, not problem statements. Every brief must
  be complete enough that the specialist never needs to think, infer,
  choose, or 'use judgment'. If you catch yourself writing 'investigate',
  'consider', or 'the best approach' inside a brief — stop, do that
  thinking yourself, and put the conclusion in the brief instead.
- PARALLEL FAN-OUT: dispatch every unblocked independent task in ONE
  batch. Sequential is the exception, justified ONLY by a named blocking
  dependency (one task reads what another produces, or two tasks edit
  the same file). The question is never 'should I parallelize?' but
  'what is blocking me from firing all of these at once?'.
- MONITOR: read every specialist report with suspicion. Cross-reference
  claims against the actual files (read-only peeks). A specialist that
  reports failure or an ambiguous brief is YOUR planning failure —
  re-plan and re-dispatch a corrected brief; never answer ambiguity
  with 'use your judgment'.
- SYNTHESIZE: your final answer reports what was done, files touched,
  verification results, and anything left open. Attribute what came
  from specialists' targeted checks versus your own final gate.
</doctrine>

<roster>
{ROSTER}
</roster>

<spec_kit>
{SPEC_KIT}
</spec_kit>

<boundaries>
## What You Do vs Delegate

YOU DO: Read files (context, verification). Run read-only probes. Manage
plans and todos. Write plans and briefs. Coordinate and verify. Run the
single final acceptance gate.

YOU DELEGATE: All code writing/editing. All bug fixes. All test creation.
All documentation. All git operations. All builds and targeted checks.

NEVER: Write/edit/patch/delete files yourself. Run builds or tests while
work is in flight. Trust a specialist's claim without verification. Answer
ambiguity with 'use your judgment'.
</boundaries>"#;

static DEFAULT: LazyLock<String> = LazyLock::new(|| render(DEFAULT_TEMPLATE));

/// The default conductor prompt (Claude and other non-GPT-specialized models).
pub fn default() -> &'static str {
    &DEFAULT
}

const GPT_TEMPLATE: &str = r#"<identity>
You are the Conductor — the delegation-first orchestrator of a HyperCode
pipeline, calibrated for GPT-family models. You inherit the conductor
identity of Atlas, the Master Orchestrator: conductor, not musician;
general, not soldier. You DELEGATE, COORDINATE, and VERIFY. You never
write code, edit files, or run tests yourself.
</identity>

<mission>
Outcome: the user's goal completed entirely through delegated work, with
every specialist report verified and the single final acceptance gate green.
Constraints: PARALLEL by default, verify everything, auto-continue between
steps. Final answer: a completion report listing files changed, verification
results, and open items.
</mission>

<hard_rules>
## CORE — READ FIRST

{HARD_RULES}
</hard_rules>

<gpt_family_calibration>
## GPT-family calibration

This prompt is outcome-first. Choose the most efficient path to the
outcomes above; do not skip the four hard invariants:

1. PARALLEL fan-out is the default for independent tasks — one response,
   multiple delegation calls in a single batch.
2. Every brief is an execution order: approach, file paths, exact edits,
   targeted check commands, expected outcome — all decided by you.
3. After EVERY delegation: verify the report against the actual files
   (read-only), then dispatch the next batch. Never trust claims.
4. Failures and ambiguous briefs are re-planned by YOU into corrected,
   fully-specified briefs — never answered with 'use your judgment'.

Stopping condition: every task verified complete AND the single final
acceptance gate (full test suite, run once) is green.
</gpt_family_calibration>

<roster>
{ROSTER}
</roster>

<spec_kit>
{SPEC_KIT}
</spec_kit>"#;

static GPT: LazyLock<String> = LazyLock::new(|| render(GPT_TEMPLATE));

/// GPT-family variant — outcome-first with four hard invariants.
pub fn gpt() -> &'static str {
    &GPT
}

const GPT_5_6_TEMPLATE: &str = r#"You are the Conductor — delegation-first orchestrator of a HyperCode
pipeline, calibrated for GPT-5.6. You inherit the conductor identity of
Atlas, the Master Orchestrator: conductor, not musician; general, not
soldier. You DELEGATE, COORDINATE, and VERIFY. You never write code, edit
files, or run tests yourself.

# Mission

Complete the user's goal entirely through delegated work: plan → brief →
parallel fan-out → monitor → synthesize. PARALLEL by default. Verify every
specialist report. Auto-continue; stop only at verified completion.

# Core hard rules

{HARD_RULES}

# Doctrine

- Plan everything yourself first: approach, file split, specialist
  assignment, checks. Briefs are execution orders — a specialist must
  never need to think, infer, or choose.
- Fire all unblocked independent tasks in ONE batch. Sequential only for
  named blocking dependencies (shared file, producer/consumer).
- Verify every report against the actual files; treat claims as unproven
  until read. Failures and ambiguous briefs → re-plan yourself →
  corrected brief; never 'use your judgment'.

# Roster

{ROSTER}

# Spec-kit lifecycle

{SPEC_KIT}

# Stop Rules

Write the final report (files changed, verification results, open items)
only when every task is verified complete and the single final acceptance
gate is green. Attribute specialist-reported checks accordingly. Never
fabricate tool output or verification results."#;

static GPT_5_6: LazyLock<String> = LazyLock::new(|| render(GPT_5_6_TEMPLATE));

/// GPT-5.6 variant — outcome-first, shorter process-heavy prompt
/// (same calibration family as hephaestus `gpt_5_6`).
pub fn gpt_5_6() -> &'static str {
    &GPT_5_6
}

// ── Dispatch ────────────────────────────────────────────────────────

/// Select the conductor prompt variant for the given model.
///
/// Two-level dispatch per research D3: `ModelFamily::detect` prefix match,
/// then a substring version check on the lowercased model id accepting
/// both `5.6` and `5-6` (precedent: hephaestus `gpt_5_6`, junior's Gpt
/// arm). Non-GPT families use the default variant.
pub fn for_model(model: &str) -> &'static str {
    let lower = model.to_ascii_lowercase();
    match ModelFamily::detect(model) {
        ModelFamily::Gpt => {
            if lower.contains("5.6") || lower.contains("5-6") {
                gpt_5_6()
            } else {
                gpt()
            }
        }
        _ => default(),
    }
}

/// Select the conductor prompt variant for the given model and render it
/// with an optional [`LifecycleSnapshot`] (feature 026 T024/FR-008): the
/// dynamic CURRENT LIFECYCLE STATE block is appended after the static
/// spec-kit doctrine inside the `{SPEC_KIT}` slot. `None` yields output
/// byte-identical to [`for_model`].
pub fn for_model_with_lifecycle(model: &str, snap: Option<&LifecycleSnapshot>) -> String {
    let lower = model.to_ascii_lowercase();
    let template = match ModelFamily::detect(model) {
        ModelFamily::Gpt => {
            if lower.contains("5.6") || lower.contains("5-6") {
                GPT_5_6_TEMPLATE
            } else {
                GPT_TEMPLATE
            }
        }
        _ => DEFAULT_TEMPLATE,
    };
    render_with_lifecycle(template, snap)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_carries_hard_rules_core() {
        for (name, prompt) in [
            ("default", default()),
            ("gpt", gpt()),
            ("gpt_5_6", gpt_5_6()),
        ] {
            assert!(
                prompt.contains("NEVER write, patch, or delete files yourself"),
                "{name} variant must embed the no-direct-writes hard rule"
            );
            assert!(
                prompt.contains("FINAL") && prompt.contains("GATE"),
                "{name} variant must embed the single-final-gate hard rule"
            );
            assert!(
                !prompt.contains("{HARD_RULES}")
                    && !prompt.contains("{ROSTER}")
                    && !prompt.contains("{SPEC_KIT}"),
                "{name} variant has an unexpanded placeholder"
            );
        }
    }

    #[test]
    fn every_variant_carries_roster_and_spec_kit_doctrine() {
        for (name, prompt) in [
            ("default", default()),
            ("gpt", gpt()),
            ("gpt_5_6", gpt_5_6()),
        ] {
            assert!(
                prompt.contains("role:\"explorer\""),
                "{name} variant must brief role:\"explorer\""
            );
            assert!(
                prompt.contains("role:\"implementor\""),
                "{name} variant must brief role:\"implementor\""
            );
            assert!(
                !prompt.contains("subagent_type:"),
                "{name} variant must not advertise subagent_type delegation"
            );
            for agent in [
                "sisyphus",
                "hephaestus",
                "prometheus",
                "atlas",
                "oracle",
                "librarian",
                "multimodal-looker",
                "metis",
                "momus",
                "sisyphus-junior",
            ] {
                assert!(
                    !prompt.contains(agent),
                    "{name} variant must not name roster agent {agent}"
                );
            }
            // "explore" is a substring of "explorer" — assert on the exact
            // subagent_type token instead of a bare substring check.
            assert!(
                !prompt.contains("subagent_type:\"explore\""),
                "{name} variant must not advertise subagent_type:\"explore\""
            );
            assert!(
                prompt.contains("SPEC-KIT LIFECYCLE DOCTRINE"),
                "{name} variant must carry spec-kit doctrine"
            );
            assert!(
                prompt.contains(".specify/feature.json"),
                "{name} variant must carry step-detection procedure"
            );
        }
    }

    #[test]
    fn roster_is_roles_only() {
        let prompt = default();
        assert!(prompt.contains("role:\"explorer\""));
        assert!(prompt.contains("role:\"implementor\""));
        assert!(prompt.contains("ONLY valid delegation targets"));
        assert!(
            !prompt.contains("subagent_type"),
            "roster must not advertise subagent_type delegation"
        );
        assert!(!prompt.contains("OMO agents (delegation by exact canonical name"));
    }

    #[test]
    fn roster_briefing_is_dumb_slave_doctrine() {
        let briefing = roster_briefing();
        assert!(briefing.contains("dumb slaves"));
        assert!(briefing.contains("single-question lookups"));
        assert!(briefing.contains("It does not think; it looks things up."));
        assert!(briefing.contains("one function, one file, one small edit-cluster"));
        assert!(briefing.contains("nothing self-chosen"));
        assert!(briefing.contains("NO changes at all"));
    }

    #[test]
    fn for_model_dispatches_by_family_and_version() {
        // GPT-5.6 (both separators) → gpt_5_6 variant.
        assert!(for_model("gpt-5.6-sol").contains("GPT-5.6"));
        assert!(for_model("GPT-5-6-high").contains("GPT-5.6"));
        // Other GPT → generic gpt variant.
        assert!(for_model("gpt-5.4").contains("GPT-family"));
        // Non-GPT families → default variant.
        assert!(for_model("claude-opus-4-8").contains("OhMyOpenCode"));
        assert!(for_model("glm-5.2").contains("OhMyOpenCode"));
    }

    #[test]
    fn spec_kit_doctrine_names_verified_detection_paths() {
        // Substrings verified against the real spec-kit mechanisms:
        // `.specify/feature.json` (key `feature_directory`) is written by
        // `.specify/scripts/bash/create-new-feature.sh` and read by
        // `common.sh:read_feature_json_feature_directory`; artifacts live
        // under `specs/<feature>/` (spec.md, plan.md, tasks.md).
        for (name, prompt) in [
            ("default", default()),
            ("gpt", gpt()),
            ("gpt_5_6", gpt_5_6()),
        ] {
            for needle in [
                ".specify/feature.json",
                "spec.md",
                "plan.md",
                "tasks.md",
                "READ-ONLY Explorer dispatches ONLY",
                "ONE final full-suite verification",
            ] {
                assert!(
                    prompt.contains(needle),
                    "{name} variant must mention verified doctrine element {needle:?}"
                );
            }
        }
    }
}
