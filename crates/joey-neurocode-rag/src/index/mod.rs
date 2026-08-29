//! Indexing pipeline.
//!
//! Chunk construction from parse spans + fallback, incremental change
//! detection, and the atomic background refresh worker.

pub mod chunker;
pub mod incremental;
pub mod refresh_worker;
