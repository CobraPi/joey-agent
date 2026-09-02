//! SC-009 failure-recurrence A/B test (spec 023, task T034, US7).
//!
//! Deterministic fake-repair harness: a worker with the KNOWN failure
//! signature `SIG-RC-42` fails its task on the first attempt UNLESS its
//! consulted guidance contains a verified lesson matching that signature.
//! One cycle = one task attempt; a RECURRENCE is a cycle where the worker
//! failed (the failure recurs because no verified lesson guided it).
//!
//! Arm A (baseline, consultation off): 8 cycles with no store => 8
//! recurrences. Arm B (consultation on): one recorded [`VerifiedOutcome`]
//! whose lesson matches SIG-RC-42 => 8 cycles, 0 recurrences. SC-009
//! requires `guided_recurrences * 2 <= baseline_recurrences` (>= 50%
//! reduction).
//!
//! Harness note: `OutcomeStore::consult_by_signature` matches rows by
//! `task_signature` (exact match — see `src/memory/outcomes.rs`), and the
//! verified lesson under test is filed under `task_signature =
//! "task-fixed-thing"`. The worker therefore consults THAT signature and
//! considers itself guided only when a returned lesson's
//! `failure_signature` matches its known failure `SIG-RC-42` — i.e. the
//! consulted guidance contains a verified lesson matching the signature.

use joey_neurocode::memory::outcomes::{OutcomeStore, VerifiedOutcome};

/// The worker's known failure signature.
const FAILURE_SIGNATURE: &str = "SIG-RC-42";
/// Task signature under which the verified lesson is filed.
const LESSON_TASK_SIGNATURE: &str = "task-fixed-thing";
/// Cycles per arm.
const CYCLES: usize = 8;

/// One task attempt. Returns `true` when the worker succeeds on the first
/// try: with a store it consults the lessons filed for the task and
/// succeeds only when the guidance contains a verified lesson matching its
/// known failure signature.
fn attempt(store: Option<&OutcomeStore>) -> bool {
    if let Some(st) = store {
        let lessons = st.consult_by_signature(LESSON_TASK_SIGNATURE).unwrap();
        if lessons
            .iter()
            .any(|lesson| lesson.failure_signature.as_deref() == Some(FAILURE_SIGNATURE))
        {
            return true;
        }
    }
    false
}

/// Recurrences (failed cycles) across `cycles` attempts of one arm.
fn count_recurrences(store: Option<&OutcomeStore>, cycles: usize) -> usize {
    (0..cycles).filter(|_| !attempt(store)).count()
}

/// The single verified outcome feeding arm B (full provenance, FR-025).
fn fixed_thing_outcome() -> VerifiedOutcome {
    VerifiedOutcome {
        task_signature: LESSON_TASK_SIGNATURE.to_string(),
        repository_revision: "rev-1".to_string(),
        artifact_ids: vec![7],
        policy_ids: vec![],
        failure_signature: Some(FAILURE_SIGNATURE.to_string()),
        resolution: Some("guard the lookup before unwrap".to_string()),
        evidence_ids: vec!["ev-1".to_string()],
        confidence: 90,
    }
}

#[test]
fn sc009_verified_lesson_guidance_cuts_recurrence_at_least_in_half() {
    // Arm A: baseline, consultation off — the worker has no guidance and
    // fails every cycle, so the failure recurs 8 times.
    let baseline_recurrences = count_recurrences(None, CYCLES);
    assert_eq!(baseline_recurrences, 8);

    // Arm B: consultation on, one verified lesson recorded.
    let store = OutcomeStore::open_in_memory().unwrap();
    store.record(&fixed_thing_outcome()).unwrap();
    let guided_recurrences = count_recurrences(Some(&store), CYCLES);
    assert_eq!(guided_recurrences, 0);

    // SC-009: at least a 50% reduction in failure recurrence.
    assert!(guided_recurrences * 2 <= baseline_recurrences);

    // hit_count == 8 after the B arm: bumped once per consult, one consult
    // per cycle (`all_rows` inspects WITHOUT bumping).
    let rows = store.all_rows().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].hit_count, CYCLES as u32);

    // The recorded lesson round-trips with full provenance (this consult is
    // bump number CYCLES + 1, so the returned row is post-bump).
    let lessons = store.consult_by_signature(LESSON_TASK_SIGNATURE).unwrap();
    assert_eq!(lessons.len(), 1);
    let lesson = &lessons[0];
    assert_eq!(lesson.task_signature, "task-fixed-thing");
    assert_eq!(lesson.repository_revision, "rev-1");
    assert_eq!(lesson.artifact_ids, vec![7]);
    assert!(lesson.policy_ids.is_empty());
    assert_eq!(lesson.failure_signature.as_deref(), Some("SIG-RC-42"));
    assert_eq!(
        lesson.resolution.as_deref(),
        Some("guard the lookup before unwrap")
    );
    assert_eq!(lesson.evidence_ids, vec!["ev-1"]);
    assert_eq!(lesson.confidence, 90);
    assert_eq!(lesson.hit_count, CYCLES as u32 + 1);
}

#[test]
fn unverified_tasks_yield_no_guidance_so_failure_recurs() {
    // Fresh store, nothing recorded: no verified outcome exists, so no
    // guidance exists for ANY signature (SC-008: only verified outcomes
    // can be recorded).
    let store = OutcomeStore::open_in_memory().unwrap();
    assert!(store.consult_by_signature("SIG-RC-42").unwrap().is_empty());
    assert!(store
        .consult_by_signature(LESSON_TASK_SIGNATURE)
        .unwrap()
        .is_empty());

    // attempt() against the empty store behaves like consultation-off: the
    // worker fails, i.e. the failure recurs — guidance only exists for
    // VERIFIED outcomes.
    assert!(!attempt(Some(&store)));
    assert_eq!(count_recurrences(Some(&store), 1), 1);
}
