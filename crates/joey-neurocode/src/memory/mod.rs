//! Learned patterns, anti-patterns, and domain knowledge (FR-011, FR-013/014).

pub mod domain;
pub mod patterns;

pub use domain::{DomainKnowledge, KnowledgeCategory, KnowledgeSource};
pub use patterns::{LearnedAntiPattern, LearnedPattern};
