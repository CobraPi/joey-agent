//! Conflict detection integration test — T014.
//!
//! A write based on a stale hash must be rejected (409-equivalent error)
//! and the file must remain completely unmodified on disk.

use joey_speckit_ui::conflict::content_hash;
use joey_speckit_ui::writer::{write_if_unchanged, WriteError};
use tempfile::tempdir;

#[test]
fn stale_hash_write_is_rejected_and_file_unmodified() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("tasks.md");
    let original = "- [ ] T001 Do the thing\n";
    std::fs::write(&path, original).unwrap();

    // Simulate another process/editor changing the file after we read it.
    let stale_hash = content_hash("- [ ] T001 Do a different thing\n");

    let result = write_if_unchanged(&path, "- [X] T001 Do the thing\n", &stale_hash);

    match result {
        Err(WriteError::Conflict(_)) => {}
        other => panic!("expected Conflict error, got {other:?}"),
    }

    // File must be byte-for-byte unchanged.
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, original);
}

#[test]
fn matching_hash_write_succeeds() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("tasks.md");
    let original = "- [ ] T001 Do the thing\n";
    std::fs::write(&path, original).unwrap();

    let correct_hash = content_hash(original);
    let new_hash = write_if_unchanged(&path, "- [X] T001 Do the thing\n", &correct_hash).unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, "- [X] T001 Do the thing\n");
    assert_eq!(new_hash, content_hash(&after));
}
