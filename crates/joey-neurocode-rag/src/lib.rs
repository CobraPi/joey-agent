//! # joey-neurocode-rag
//!
//! NeuroCode RAG: local-first semantic code retrieval layered over the existing
//! NeuroCode typed code graph (feature 021, `specs/021-please-enhance-neurocode`).
//!
//! Natural-language code search, hybrid dense+BM25 ranking fused client-side via
//! Reciprocal Rank Fusion, fast incremental background re-indexing with atomic
//! snapshot swaps, context-expanded results, and relationship-aware retrieval.
//! The semantic engine is a fully-local, in-process ONNX embedder (`ort` with a
//! runtime-loaded ONNX Runtime dylib; no daemon, no network by default), with
//! optional OpenAI-compatible and Ollama HTTP backends behind per-project
//! recorded, revocable consent, plus a provider-following GitHub Copilot
//! embeddings backend (Joey-native extension). The whole feature is default-off
//! (`neurocode.rag.enabled = false`) and byte-identical to pre-enhancement
//! behavior while disabled (FR-009/SC-005).
//!
//! Module map (full scaffold declared up front so later tasks add
//! implementation, never module declarations):
//!
//! - [`config`] — all 18 `neurocode.rag.*` config keys (T002)
//! - [`consent`] — per-project `consent.json` state machine (T007)
//! - [`parity`] — byte-identical-when-disabled guard (T035)
//! - [`embed`] — embedding backends: model profiles, artifact integrity,
//!   local ONNX (primary), OpenAI-compatible and Ollama HTTP (secondary)
//! - [`index`] — chunker, incremental change detection, refresh worker
//! - [`search`] — hybrid dense+FTS5 retrieval, RRF fusion, context expansion
//! - [`vector`] — BLOB vector store, in-memory scan, int8 quantization

pub mod config;
pub mod consent;
pub mod memory_search;
pub mod parity;

// T010: `embed` grew a real mod.rs (trait + registry + auto resolution) —
// the inline declaration is replaced by the file module; submodule paths
// (`crate::embed::profiles`, …) are unchanged.
pub mod embed;

pub mod index {
    pub mod chunker;
    pub mod incremental;
    pub mod refresh_worker;
}

pub mod search {
    pub mod expand;
    pub mod hybrid;
    pub mod rrf;
}

pub mod vector {
    pub mod quantize;
    pub mod scan;
    pub mod store;
}
