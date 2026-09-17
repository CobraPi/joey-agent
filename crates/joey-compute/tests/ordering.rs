//! T003 — deadline-heap entry ordering (TDD, specs/033 contracts §2).
//!
//! BinaryHeap is a MAX-heap: to pop the earliest deadline first, Entry's
//! `Ord` is INVERTED — the "greatest" entry is the most urgent (earliest
//! deadline; among equal deadlines, the smaller sequence number = FIFO).

use joey_compute::Entry;
use std::time::{Duration, Instant};

fn entry_at(now: Instant, offset_ms: u64, seq: u64, value: u32) -> Entry<u32> {
    Entry {
        deadline: now + Duration::from_millis(offset_ms),
        seq,
        value,
    }
}

#[test]
fn earliest_deadline_pops_first() {
    let now = Instant::now();
    // Spec cases at scale 2s: w1 → +2000ms, w5 → +400ms, w80 → +25ms
    // (2000/80; the contract formula deadline = now + scale/weight governs).
    let mut heap = std::collections::BinaryHeap::new();
    heap.push(entry_at(now, 2000, 1, 1));
    heap.push(entry_at(now, 25, 2, 2));
    heap.push(entry_at(now, 400, 3, 3));
    assert_eq!(heap.pop().unwrap().value, 2); // w80 first
    assert_eq!(heap.pop().unwrap().value, 3); // w5 next
    assert_eq!(heap.pop().unwrap().value, 1); // w1 last
}

#[test]
fn equal_deadlines_pop_fifo_by_sequence() {
    let now = Instant::now();
    let mut heap = std::collections::BinaryHeap::new();
    heap.push(entry_at(now, 500, 3, 30));
    heap.push(entry_at(now, 500, 1, 10));
    heap.push(entry_at(now, 500, 2, 20));
    assert_eq!(heap.pop().unwrap().value, 10);
    assert_eq!(heap.pop().unwrap().value, 20);
    assert_eq!(heap.pop().unwrap().value, 30);
}

#[test]
fn inverted_ord_greatest_is_most_urgent() {
    let now = Instant::now();
    use std::cmp::Ordering;
    let urgent = entry_at(now, 10, 1, 0);
    let late = entry_at(now, 10_000, 2, 0);
    assert_eq!(urgent.cmp(&late), Ordering::Greater);
    assert_eq!(late.cmp(&urgent), Ordering::Less);
    let first_seq = entry_at(now, 10, 1, 0);
    let later_seq = entry_at(now, 10, 2, 0);
    assert_eq!(first_seq.cmp(&later_seq), Ordering::Greater);
}
