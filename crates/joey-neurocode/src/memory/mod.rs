//! Learned patterns, anti-patterns, and domain knowledge (FR-011, FR-013/014).

pub mod distill;
pub mod domain;
pub mod episodes;
pub mod outcomes;
pub mod patterns;
pub mod preferences;

pub use distill::{DistilledPreference, HeuristicDistiller, MemoryDistiller};
pub use domain::{DomainKnowledge, KnowledgeCategory, KnowledgeSource};
pub use episodes::{EpisodeKind, EpisodeOutcome, EpisodeSource, EpisodeStore, MemoryEpisode, MemoryItemKind, MemoryQuantization, MemoryVectorRecord};
pub use outcomes::{OutcomeMemory, OutcomeMemoryBuffer, VerifiedOutcome};
pub use patterns::{LearnedAntiPattern, LearnedPattern};
pub use preferences::{MemoryPreference, PreferenceOrigin, PreferenceStatus, PreferenceStore, UpsertOutcome};
