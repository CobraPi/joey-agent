//! joey-llm-selector — dynamic per-module LLM model allocator.
//!
//! Engages when the user selects the `auto` model on a catalog-exposing
//! provider. Treats the agent as a compound AI system (Chen et al.,
//! arXiv:2502.14815): assigns each distinct LLM call site ("module") the
//! best-suited model drawn from the active provider's live `/models` catalog,
//! learns better allocations asynchronously via an LLM diagnoser triggered
//! only by observable failure, and persists a global allocation map at
//! `~/.joey/llm-selector/allocations.json`.
//!
//! `joey-agent-core` consumes only the narrow [`model_allocator::ModelAllocator`]
//! trait; the engine, diagnoser, scorer, and map internals are private to this
//! crate (Constitution VI — Modularity and Decoupling).

pub mod allocator;
pub mod candidate;
pub mod diagnoser;
pub mod map;
pub mod model_allocator;
pub mod module;
pub mod query;
pub mod scorer;

pub use allocator::{SelectorConfig, SelectorEngine};
pub use candidate::{CandidateModel, CandidateModelPool, CapabilityTier, Cost};
pub use map::{AllocationEntry, AllocationMap, DiagnosticRecord, FailureSignal, MapError};
pub use model_allocator::{Allocation, AllocationSource, ModelAllocator};
pub use module::ModuleId;
pub use query::{SelectorQuery, StatusReport, render_status};
pub use scorer::{ColdStartScorer, ModuleRequirements};
