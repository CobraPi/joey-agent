//! Distillation of semantic memories from episodic evidence (feature 027).
//! Fully automatic + continuous (clarified Q1/Q3). This module is
//! deliberately provider-free (crate DAG): the [`MemoryDistiller`] trait is
//! implemented synchronously by [`HeuristicDistiller`] (pattern-based
//! explicit-statement detection + recurrence confidence math); the
//! provider-backed implementation lives in joey-cli wiring. Embedding-based
//! recurrence matching (cosine >= 0.92) is orchestrated by the wiring layer,
//! which has both stores and the embedding backend.

use crate::memory::episodes::{sanitize_text, MemoryEpisode};

/// A preference candidate distilled from a user message or an episode.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DistilledPreference {
    pub category: String,
    pub statement: String,
}

/// Synchronous distillation seam: the heuristic implementation runs in-crate
/// with zero model cost; the provider-backed pass is wired in joey-cli
/// (crate DAG forbids a provider dependency here).
pub trait MemoryDistiller: Send + Sync {
    /// Detect explicit preference statements in raw user text.
    fn detect_explicit(&self, user_text: &str) -> Vec<DistilledPreference>;
    /// Distill preference candidates from a completed episode.
    fn distill_episode(&self, episode: &MemoryEpisode) -> Vec<DistilledPreference>;
}

/// Pattern-based distiller: explicit-statement detection via fixed
/// prefixes/patterns, deterministic keyword category derivation, and a
/// failed-episode fallback that surfaces lessons with zero model cost.
pub struct HeuristicDistiller;

impl MemoryDistiller for HeuristicDistiller {
    fn detect_explicit(&self, user_text: &str) -> Vec<DistilledPreference> {
        let mut out: Vec<DistilledPreference> = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for line in user_text.lines() {
            let trimmed = line.trim();
            if !line_matches(trimmed) {
                continue;
            }
            let statement = sanitize_text(
                strip_trailing_punctuation(trimmed),
                crate::memory::preferences::STATEMENT_CAP,
            );
            if statement.trim().is_empty() {
                continue; // dropped: nothing survived sanitization
            }
            let category = derive_category(&statement);
            // Dedupe by (category, statement): statements compare
            // case-insensitively with whitespace normalized (trailing
            // punctuation is already stripped above). First occurrence
            // wins and keeps its original casing.
            let key = format!(
                "{}\u{1}{}",
                category,
                statement
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_lowercase()
            );
            if !seen.contains(&key) {
                seen.push(key);
                out.push(DistilledPreference { category, statement });
            }
        }
        out
    }

    fn distill_episode(&self, episode: &MemoryEpisode) -> Vec<DistilledPreference> {
        if episode.outcome == crate::memory::episodes::EpisodeOutcome::Failure
            && !episode.lessons.trim().is_empty()
        {
            let first_sentence = first_sentence(&episode.lessons);
            let statement =
                sanitize_text(&format!("Avoid: {first_sentence}"), crate::memory::preferences::STATEMENT_CAP);
            if !statement.trim().is_empty() {
                return vec![DistilledPreference {
                    category: "structure".to_string(),
                    statement,
                }];
            }
        }
        Vec::new()
    }
}

/// Does the (trimmed, case-insensitive) line match one of the
/// explicit-statement prefixes/patterns?
fn line_matches(trimmed: &str) -> bool {
    let lower = trimmed.to_lowercase();
    const PREFIXES: [&str; 8] = [
        "i prefer ",
        "i always ",
        "i never ",
        "i don't want ",
        "always ",
        "never ",
        "please always ",
        "please never ",
    ];
    PREFIXES.iter().any(|p| lower.starts_with(p))
        || lower.contains(" prefer ")
        || lower.contains(" instead of ")
}

/// Strip trailing punctuation (`.`/`!`/`?`/`;`/`,`) from a trimmed line.
fn strip_trailing_punctuation(trimmed: &str) -> &str {
    trimmed.trim_end_matches(['.', '!', '?', ';', ','])
}

/// Deterministic keyword map: statement text → category slug.
fn derive_category(statement: &str) -> String {
    let lower = statement.to_lowercase();
    let has_any = |keys: &[&str]| keys.iter().any(|k| lower.contains(k));
    if has_any(&["name", "naming", "variable", "function name"]) {
        "naming"
    } else if has_any(&["error", "exception", "panic"]) {
        "error-handling"
    } else if has_any(&["test"]) {
        "testing"
    } else if has_any(&["library", "crate", "dependency", "framework"]) {
        "libraries"
    } else if has_any(&["format", "indent"]) {
        "formatting"
    } else {
        "structure"
    }
    .to_string()
}

/// First sentence of `text` (up to the first `.`/`!`/`?`, else the whole
/// trimmed text).
fn first_sentence(text: &str) -> &str {
    let trimmed = text.trim();
    match trimmed.find(['.', '!', '?']) {
        Some(idx) => &trimmed[..=idx],
        None => trimmed,
    }
}

/// Cosine similarity of two f32 slices: dot / (||a|| * ||b||); 0.0 on
/// length mismatch or zero norm.
pub fn cosine_f32(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

/// Recurrence threshold: embedding cosine >= this treats two statements as
/// the same preference (strengthen instead of insert).
pub const RECURRENCE_COSINE: f32 = 0.92;

/// Confidence bump on recurrence (saturating at 100).
pub fn bump_confidence(current: u8) -> u8 {
    current.saturating_add(10).min(100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::episodes::{EpisodeKind, EpisodeOutcome, EpisodeSource};

    fn episode(outcome: EpisodeOutcome, lessons: &str) -> MemoryEpisode {
        MemoryEpisode {
            id: "ep-1".to_string(),
            kind: EpisodeKind::Task,
            title: "t".to_string(),
            task: "task".to_string(),
            context: String::new(),
            approach: String::new(),
            outcome,
            lessons: lessons.to_string(),
            source: EpisodeSource::Interactive,
            origin_run: String::new(),
            evidence_ids: vec![],
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn detect_explicit_matches_prefixes_and_patterns() {
        let d = HeuristicDistiller;
        let hits = d.detect_explicit(
            "I prefer snake_case names.\nSome other line.\nAlways run tests instead of skipping them",
        );
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].category, "naming");
        assert_eq!(hits[0].statement, "I prefer snake_case names");
        assert_eq!(hits[1].category, "testing");
    }

    #[test]
    fn detect_explicit_dedupes_and_drops_empty() {
        let d = HeuristicDistiller;
        let hits = d.detect_explicit("never commit secrets\nNever commit secrets!");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].statement, "never commit secrets");
        assert!(d.detect_explicit("nothing interesting here").is_empty());
    }

    #[test]
    fn distill_episode_failure_lessons_only() {
        let d = HeuristicDistiller;
        let hits = d.distill_episode(&episode(EpisodeOutcome::Failure, "The build broke. Fix deps next."));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].category, "structure");
        assert_eq!(hits[0].statement, "Avoid: The build broke.");
        // Success or empty lessons → nothing.
        assert!(d.distill_episode(&episode(EpisodeOutcome::Success, "lesson")).is_empty());
        assert!(d.distill_episode(&episode(EpisodeOutcome::Failure, "   ")).is_empty());
    }

    #[test]
    fn cosine_math_and_threshold() {
        assert!((cosine_f32(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert_eq!(cosine_f32(&[1.0], &[1.0, 2.0]), 0.0); // length mismatch
        assert_eq!(cosine_f32(&[0.0], &[0.0]), 0.0); // zero norm
        assert_eq!(RECURRENCE_COSINE, 0.92);
    }

    #[test]
    fn bump_confidence_saturates() {
        assert_eq!(bump_confidence(50), 60);
        assert_eq!(bump_confidence(95), 100);
        assert_eq!(bump_confidence(255), 100);
    }
}
