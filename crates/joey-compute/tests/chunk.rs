//! T019 — cooperative chunked cancellation (feature 033, US5).
//! Cancellation is checked BETWEEN chunks only; partial results plus a
//! completed-all flag are returned.

use joey_compute::{run_chunked, CancelToken};

#[test]
fn cancel_after_three_chunks_returns_partial_and_incomplete() {
    let items: Vec<u32> = (0..100).collect();
    let token = CancelToken::new();
    let token2 = token.clone();
    let mut chunks_seen = 0;
    let (results, completed) = run_chunked(&items, &token, 10, |chunk| {
        chunks_seen += 1;
        if chunks_seen == 3 {
            token2.set(); // cancel takes effect BEFORE the next chunk
        }
        chunk.iter().map(|x| x * 2).collect()
    });
    assert_eq!(results.len(), 30, "exactly 3 chunks of 10 processed");
    assert_eq!(results[29], 58);
    assert!(!completed, "cancelled run must report not-completed");
}

#[test]
fn full_run_without_cancel_completes_all() {
    let items: Vec<u32> = (0..100).collect();
    let token = CancelToken::new();
    let (results, completed) = run_chunked(&items, &token, 10, |chunk| {
        chunk.iter().map(|x| x + 1).collect()
    });
    assert_eq!(results.len(), 100);
    assert_eq!(results[99], 100);
    assert!(completed);
}

#[test]
fn pre_cancelled_token_processes_nothing() {
    let items: Vec<u32> = (0..50).collect();
    let token = CancelToken::new();
    token.set();
    let (results, completed) = run_chunked(&items, &token, 10, |chunk| {
        chunk.iter().map(|x| x * 2).collect()
    });
    assert!(results.is_empty());
    assert!(!completed);
}
