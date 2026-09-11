//! T008 — Store-level behavior suites for feature 027 (feature 027,
//! specs/027-please-enhance-neurocode, contracts/neurocode-memory-storage.md):
//!
//! * sanitization choke point [analyze U1] on the episode insert path,
//! * empty-after-sanitization skip (FR-011),
//! * delete cascade episode → vector,
//! * FIFO eviction beyond `max_episodes`,
//! * FR-008 supersede rank (explicit > inferred) + conflict surfacing,
//! * strengthen (recurrence) semantics,
//! * heuristic distiller detection categories + redaction, cosine math,
//!   confidence bump saturation.

use joey_neurocode::memory::distill::{
    bump_confidence, cosine_f32, DistilledPreference, HeuristicDistiller, MemoryDistiller,
};
use joey_neurocode::memory::episodes::{
    EpisodeKind, EpisodeOutcome, EpisodeSource, EpisodeStore, MemoryEpisode, MemoryItemKind,
    MemoryQuantization, MemoryVectorRecord, FIELD_CAP, TITLE_CAP,
};
use joey_neurocode::memory::preferences::{
    PreferenceOrigin, PreferenceStatus, PreferenceStore,
};

/// An AWS Access Key ID: matches the `AKIA[A-Z0-9]{16}` prefix pattern in
/// joey-core's redact.rs (one of the patterns its own unit tests use).
const AWS_KEY: &str = "AKIAABCDEFGHIJKLMNOP";

fn episode(task: &str) -> MemoryEpisode {
    MemoryEpisode {
        id: String::new(),
        kind: EpisodeKind::Task,
        title: "title".to_string(),
        task: task.to_string(),
        context: String::new(),
        approach: String::new(),
        outcome: EpisodeOutcome::Success,
        lessons: String::new(),
        source: EpisodeSource::Interactive,
        origin_run: String::new(),
        evidence_ids: vec![],
        created_at: String::new(),
        updated_at: String::new(),
    }
}

// ---------------------------------------------------------------------------
// Sanitization choke point [analyze U1].
// ---------------------------------------------------------------------------

#[test]
fn insert_sanitizes_and_caps() {
    let store = EpisodeStore::open_in_memory().unwrap();

    // Secret pattern joey-core redacts: an AWS-style Access Key ID must NOT
    // survive into the persisted row.
    let mut ep = episode(&format!("deploy with key {AWS_KEY} now"));
    ep.title = format!("deploy with {AWS_KEY}");
    let id = store.insert(&ep, None, 100).unwrap().unwrap();
    let fetched = store.get(&id).unwrap().unwrap();
    assert!(
        !fetched.task.contains(AWS_KEY),
        "AWS key leaked into stored task: {}",
        fetched.task
    );
    assert!(
        !fetched.title.contains(AWS_KEY),
        "AWS key leaked into stored title: {}",
        fetched.title
    );

    // A 10_000-char task is stored capped at FIELD_CAP chars.
    let long_task = "x".repeat(10_000);
    let id2 = store.insert(&episode(&long_task), None, 100).unwrap().unwrap();
    let fetched2 = store.get(&id2).unwrap().unwrap();
    assert_eq!(fetched2.task.chars().count(), FIELD_CAP);
    assert_eq!(fetched2.task, "x".repeat(FIELD_CAP));

    // Title capped at TITLE_CAP.
    let mut ep3 = episode("a real task");
    ep3.title = "t".repeat(10_000);
    let id3 = store.insert(&ep3, None, 100).unwrap().unwrap();
    let fetched3 = store.get(&id3).unwrap().unwrap();
    assert_eq!(fetched3.title.chars().count(), TITLE_CAP);
    assert_eq!(fetched3.title, "t".repeat(TITLE_CAP));
}

#[test]
fn insert_empty_task_returns_none() {
    let store = EpisodeStore::open_in_memory().unwrap();
    // Whitespace-only task: empty after sanitization → Ok(None), no write.
    let out = store.insert(&episode("   "), None, 100).unwrap();
    assert!(out.is_none());
    assert_eq!(store.count().unwrap(), 0);
    // A task that sanitizes to only the redaction sentinel is NOT empty —
    // only a sanitized-empty task skips (FR-011 rule on the raw text).
    assert!(store.insert(&episode(""), None, 100).unwrap().is_none());
    assert_eq!(store.count().unwrap(), 0);
}

// ---------------------------------------------------------------------------
// Delete cascade: episode row + its memory_vectors row.
// ---------------------------------------------------------------------------

#[test]
fn delete_cascades_vector() {
    // File-backed: cross-check the vector purge through the GraphStore
    // connection on the same file.
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("graph.db");
    let graph = joey_neurocode::graph::DependencyGraph::open(&db_path).unwrap();

    let store = EpisodeStore::open(&db_path).unwrap();
    // Explicit id so the vector record can key on it in the same insert.
    let mut ep = episode("task with vector");
    ep.id = "ep-del-1".to_string();
    store
        .insert(
            &ep,
            Some(MemoryVectorRecord {
                item_id: "ep-del-1".to_string(),
                item_kind: MemoryItemKind::Episode,
                dim: 2,
                quantization: MemoryQuantization::F32,
                blob: vec![1, 2, 3, 4],
            }),
            100,
        )
        .unwrap()
        .unwrap();
    assert!(store.get("ep-del-1").unwrap().is_some());
    assert!(store.get_vector("ep-del-1").unwrap().is_some());
    let conn = graph.store().conn();
    let vectors_before: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_vectors WHERE item_id = 'ep-del-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(vectors_before, 1);

    assert!(store.delete("ep-del-1").unwrap());
    assert!(store.get("ep-del-1").unwrap().is_none());
    assert!(store.get_vector("ep-del-1").unwrap().is_none());
    // GraphStore-conn cross-check: the memory_vectors row is gone.
    let vectors_after: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM memory_vectors WHERE item_id = 'ep-del-1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(vectors_after, 0);
    // Second delete: nothing left.
    assert!(!store.delete("ep-del-1").unwrap());
}

// ---------------------------------------------------------------------------
// FIFO eviction beyond max_episodes.
// ---------------------------------------------------------------------------

#[test]
fn fifo_eviction_respects_max_episodes() {
    let store = EpisodeStore::open_in_memory().unwrap();
    for i in 0..5 {
        let mut ep = episode(&format!("task {i}"));
        // Deterministic, strictly increasing created_at keeps FIFO order
        // stable even when inserts land in the same millisecond (the id
        // embeds timestamp_ms, so equal created_at would race on id order).
        ep.created_at = format!("2026-09-09T00:00:{:02}+00:00", 10 + i);
        ep.updated_at = ep.created_at.clone();
        store.insert(&ep, None, 3).unwrap();
    }
    assert_eq!(store.count().unwrap(), 3);
    let recent: Vec<String> = store
        .list_recent(10)
        .unwrap()
        .into_iter()
        .map(|e| e.task)
        .collect();
    // The two oldest (task 0, task 1) are gone; the newest three remain,
    // newest first.
    assert_eq!(recent, vec!["task 4", "task 3", "task 2"]);
    assert!(store.list_recent(10).unwrap().iter().all(|e| e.task != "task 0"));
    assert!(store.list_recent(10).unwrap().iter().all(|e| e.task != "task 1"));
}

// ---------------------------------------------------------------------------
// FR-008 supersede rank + conflict surfacing.
// ---------------------------------------------------------------------------

#[test]
fn preference_supersede_rank() {
    // (a) explicit target + incoming inferred with supersede_id → NOT
    //     superseded (rank refused), both stay active.
    let store = PreferenceStore::open_in_memory().unwrap();
    let explicit = store
        .upsert(
            "naming",
            "use tabs",
            PreferenceOrigin::Explicit,
            &[],
            None,
            None,
            None,
        )
        .unwrap();
    let inferred = store
        .upsert(
            "naming",
            "use spaces",
            PreferenceOrigin::Inferred,
            &[],
            None,
            Some(&explicit.id),
            None,
        )
        .unwrap();
    assert_eq!(inferred.superseded_id, None);
    let target = store.get(&explicit.id).unwrap().unwrap();
    assert_eq!(target.status, PreferenceStatus::Active);
    assert_eq!(store.count_active().unwrap(), 2);

    // (b) inferred target + incoming explicit → superseded, links set both
    //     directions: winner.supersedes = target id, target.superseded_by =
    //     winner id, target status superseded.
    let winner = store
        .upsert(
            "naming",
            "always use tabs",
            PreferenceOrigin::Explicit,
            &[],
            None,
            Some(&inferred.id),
            None,
        )
        .unwrap();
    assert_eq!(winner.superseded_id.as_deref(), Some(inferred.id.as_str()));
    let target = store.get(&inferred.id).unwrap().unwrap();
    assert_eq!(target.status, PreferenceStatus::Superseded);
    assert_eq!(target.superseded_by.as_deref(), Some(winner.id.as_str()));
    let winner_row = store.get(&winner.id).unwrap().unwrap();
    assert_eq!(winner_row.supersedes.as_deref(), Some(inferred.id.as_str()));
    // Explicit target from (a) was never superseded; active count:
    // explicit(a) + winner(b) = 2.
    assert_eq!(store.count_active().unwrap(), 2);

    // (c) explicit_conflict_categories lists a category with 2 active
    //     explicit rows.
    assert_eq!(
        store.explicit_conflict_categories().unwrap(),
        vec!["naming".to_string()]
    );
}

// ---------------------------------------------------------------------------
// Strengthen (recurrence) semantics.
// ---------------------------------------------------------------------------

#[test]
fn strengthen_bumps_confidence_no_duplicate() {
    let store = PreferenceStore::open_in_memory().unwrap();
    // Upsert a new preference (id X).
    let first = store
        .upsert(
            "naming",
            "prefer snake_case",
            PreferenceOrigin::Inferred,
            &["ev-1".to_string()],
            None,
            None,
            None,
        )
        .unwrap();
    assert!(!first.strengthened);
    assert_eq!(store.count_active().unwrap(), 1);
    assert_eq!(store.get(&first.id).unwrap().unwrap().confidence, 50);
    let created_updated_at = store.get(&first.id).unwrap().unwrap().updated_at.clone();

    // Strengthen with the same id + new evidence id: same row (count_active
    // unchanged), confidence +10, evidence deduped, updated_at refreshed.
    let second = store
        .upsert(
            "naming",
            "prefer snake_case still",
            PreferenceOrigin::Inferred,
            &["ev-2".to_string(), "ev-1".to_string()], // ev-1 again: dedup
            Some(&first.id),
            None,
            None,
        )
        .unwrap();
    assert!(second.strengthened);
    assert_eq!(second.id, first.id);
    assert_eq!(store.count_active().unwrap(), 1);

    let pref = store.get(&first.id).unwrap().unwrap();
    assert_eq!(pref.confidence, 60);
    assert_eq!(
        pref.evidence_ids,
        vec!["ev-1".to_string(), "ev-2".to_string()]
    );
    assert_eq!(pref.evidence_ids.len(), 2); // both ids, deduped
    assert_ne!(pref.updated_at, created_updated_at);

    // One more strengthen: 60 + 10 saturating at 100.
    let third = store
        .upsert(
            "naming",
            "prefer snake_case forever",
            PreferenceOrigin::Inferred,
            &["ev-3".to_string()],
            Some(&first.id),
            None,
            None,
        )
        .unwrap();
    assert!(third.strengthened);
    let pref = store.get(&first.id).unwrap().unwrap();
    assert_eq!(pref.confidence, 70);
    assert_eq!(
        pref.evidence_ids,
        vec!["ev-1".to_string(), "ev-2".to_string(), "ev-3".to_string()]
    );
}

// ---------------------------------------------------------------------------
// Heuristic distiller: detection categories, redaction, episode fallback,
// cosine math, confidence bump.
// ---------------------------------------------------------------------------

#[test]
fn heuristic_distiller_detection() {
    let d = HeuristicDistiller;

    // Two preference lines. Per distill.rs's implemented keyword map:
    // "constructor" is not a keyword → "structure"; "small functions" has
    // no keyword either ("function" alone is not; only "function name"
    // is) → "structure".
    let hits = d.detect_explicit("I prefer constructor injection\nalways use small functions");
    assert_eq!(hits.len(), 2);
    assert_eq!(
        hits,
        vec![
            DistilledPreference {
                category: "structure".to_string(),
                statement: "I prefer constructor injection".to_string(),
            },
            DistilledPreference {
                category: "structure".to_string(),
                statement: "always use small functions".to_string(),
            },
        ]
    );

    // A secret inside a preference line is redacted before the statement
    // is returned.
    let secret_hits = d.detect_explicit(&format!(
        "never commit the key {AWS_KEY} to the repo"
    ));
    assert_eq!(secret_hits.len(), 1);
    assert!(!secret_hits[0].statement.contains(AWS_KEY));

    // distill_episode on Failure + lessons yields the "Avoid: ..." preference.
    let mut ep = episode("task");
    ep.outcome = EpisodeOutcome::Failure;
    ep.lessons = "The build broke. Fix deps next.".to_string();
    let distilled = d.distill_episode(&ep);
    assert_eq!(distilled.len(), 1);
    assert_eq!(distilled[0].category, "structure");
    assert_eq!(distilled[0].statement, "Avoid: The build broke.");

    // On Success: none.
    let mut ok = episode("task");
    ok.outcome = EpisodeOutcome::Success;
    ok.lessons = "went fine".to_string();
    assert!(d.distill_episode(&ok).is_empty());

    // Cosine math: identical ≈ 1.0, orthogonal ≈ 0.0, mismatched len → 0.0.
    assert!((cosine_f32(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
    assert!((cosine_f32(&[0.5, 0.5], &[0.5, 0.5]) - 1.0).abs() < 1e-6);
    assert!(cosine_f32(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    assert_eq!(cosine_f32(&[1.0], &[1.0, 2.0]), 0.0);

    // Confidence bump saturates at 100: 95 + 10 = 105 → 100; 100 → 100.
    assert_eq!(bump_confidence(50), 60);
    assert_eq!(bump_confidence(95), 100);
    assert_eq!(bump_confidence(100), 100);
}

// ---------------------------------------------------------------------------
// T025 — budget/eviction/supersede hardening.
// ---------------------------------------------------------------------------

#[test]
fn fifo_eviction_exact_boundary() {
    let store = EpisodeStore::open_in_memory().unwrap();

    // Exactly max_episodes=3 rows: all retained, nothing evicted.
    for i in 0..3 {
        let mut ep = episode(&format!("task {i}"));
        // Distinct, strictly increasing created_at (same trick as the FIFO
        // test above) keeps eviction order deterministic.
        ep.created_at = format!("2026-09-10T00:00:{:02}+00:00", 10 + i);
        ep.updated_at = ep.created_at.clone();
        store.insert(&ep, None, 3).unwrap();
    }
    assert_eq!(store.count().unwrap(), 3);
    let all: Vec<String> = store
        .list_recent(10)
        .unwrap()
        .into_iter()
        .map(|e| e.task)
        .collect();
    assert_eq!(all, vec!["task 2", "task 1", "task 0"]);

    // A 4th insert evicts exactly the oldest (task 0); the 3 newest remain.
    let mut fourth = episode("task 3");
    fourth.created_at = "2026-09-10T00:00:13+00:00".to_string();
    fourth.updated_at = fourth.created_at.clone();
    store.insert(&fourth, None, 3).unwrap();
    assert_eq!(store.count().unwrap(), 3);
    let recent: Vec<String> = store
        .list_recent(10)
        .unwrap()
        .into_iter()
        .map(|e| e.task)
        .collect();
    assert_eq!(recent, vec!["task 3", "task 2", "task 1"]);
    assert!(recent.iter().all(|t| t != "task 0"));
}

#[test]
fn strengthen_refreshes_updated_at_without_duplication() {
    let store = PreferenceStore::open_in_memory().unwrap();
    // Upsert a new preference.
    let first = store
        .upsert(
            "naming",
            "prefer tabs",
            PreferenceOrigin::Inferred,
            &["ev-1".to_string()],
            None,
            None,
            None,
        )
        .unwrap();
    assert!(!first.strengthened);
    assert_eq!(store.count_active().unwrap(), 1);

    // Strengthen with a repeated evidence id: same row (no duplicate id),
    // count_active unchanged, confidence bumped, evidence deduped.
    // updated_at freshness is NOT asserted — the API stamps wall-clock time
    // and same-ms equality would make a changed-assertion flaky.
    let second = store
        .upsert(
            "naming",
            "prefer tabs",
            PreferenceOrigin::Inferred,
            &["ev-1".to_string(), "ev-2".to_string()], // ev-1 again: dedup
            Some(&first.id),
            None,
            None,
        )
        .unwrap();
    assert!(second.strengthened);
    assert_eq!(second.id, first.id);
    assert_eq!(store.count_active().unwrap(), 1);
    let pref = store.get(&first.id).unwrap().unwrap();
    assert_eq!(pref.id, first.id); // SAME id — no duplicate row
    assert_eq!(pref.confidence, 60);
    assert_eq!(
        pref.evidence_ids,
        vec!["ev-1".to_string(), "ev-2".to_string()]
    );

    // Evidence cap: 40 fresh ids in one strengthen → capped at 32.
    let many: Vec<String> = (0..40).map(|i| format!("ev-c-{i}")).collect();
    let third = store
        .upsert(
            "naming",
            "prefer tabs",
            PreferenceOrigin::Inferred,
            &many,
            Some(&first.id),
            None,
            None,
        )
        .unwrap();
    assert!(third.strengthened);
    assert_eq!(third.id, first.id);
    assert_eq!(store.count_active().unwrap(), 1);
    let pref = store.get(&first.id).unwrap().unwrap();
    assert_eq!(pref.evidence_ids.len(), 32);
    assert_eq!(pref.confidence, 70);
}

#[test]
fn supersede_links_consistent_both_directions() {
    let store = PreferenceStore::open_in_memory().unwrap();
    // Inferred target + explicit incoming (rank allows it).
    let target = store
        .upsert(
            "formatting",
            "2-space indent",
            PreferenceOrigin::Inferred,
            &[],
            None,
            None,
            None,
        )
        .unwrap();
    let winner = store
        .upsert(
            "formatting",
            "4-space indent",
            PreferenceOrigin::Explicit,
            &[],
            None,
            Some(&target.id),
            None,
        )
        .unwrap();
    assert_eq!(winner.superseded_id.as_deref(), Some(target.id.as_str()));

    // Links consistent in both directions + statuses flipped.
    let target_row = store.get(&target.id).unwrap().unwrap();
    assert_eq!(target_row.status, PreferenceStatus::Superseded);
    assert_eq!(target_row.superseded_by.as_deref(), Some(winner.id.as_str()));
    let winner_row = store.get(&winner.id).unwrap().unwrap();
    assert_eq!(winner_row.status, PreferenceStatus::Active);
    assert_eq!(winner_row.supersedes.as_deref(), Some(target.id.as_str()));

    // Deleting the winner: no resurrection — the target stays superseded
    // with its audit link intact, and the winner row is gone.
    assert!(store.delete(&winner.id).unwrap());
    assert!(store.get(&winner.id).unwrap().is_none());
    let target_after = store.get(&target.id).unwrap().unwrap();
    assert_eq!(target_after.status, PreferenceStatus::Superseded);
    assert_eq!(
        target_after.superseded_by.as_deref(),
        Some(winner.id.as_str())
    );
    assert_eq!(store.count_active().unwrap(), 0);
}
