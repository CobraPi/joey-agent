//! Hierarchical policy combining (spec 023, FR-002).

use std::collections::HashSet;
use std::path::PathBuf;

use super::sources::glob_match;
use super::{PolicyBinding, PolicyLayer};

/// Layers allowed to carry an unrestricted (`["**"]`) binding (FR-002).
const UNRESTRICTED_LAYERS: &[PolicyLayer] = &[
    PolicyLayer::Organization,
    PolicyLayer::Repository,
    PolicyLayer::TaskContract,
];

/// Generic words that never identify a policy subject.
const STOPWORDS: &[&str] = &[
    "always",
    "never",
    "with",
    "from",
    "that",
    "this",
    "when",
    "must",
    "should",
    "code",
    "tests",
];

/// Markers that flip a directive's polarity to negated (case-insensitive).
const NEGATION_MARKERS: &[&str] = &["never", "do not", "don't", "must not", "avoid", "forbidden"];

/// Precedence rank of a policy layer: Organization=0 (broadest) through
/// TaskContract=4 (narrowest). `combine` orders bindings low -> high.
pub fn layer_order(layer: &PolicyLayer) -> u8 {
    match layer {
        PolicyLayer::Organization => 0,
        PolicyLayer::Repository => 1,
        PolicyLayer::Module => 2,
        PolicyLayer::ScopedRule => 3,
        PolicyLayer::TaskContract => 4,
    }
}

/// A polarity clash between two applicable bindings from different layers.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PolicyConflict {
    pub directive_a: String,
    pub directive_b: String,
    pub layer_a: PolicyLayer,
    pub layer_b: PolicyLayer,
    pub source_a: PathBuf,
    pub source_b: PathBuf,
    /// Task paths the conflicting pair both applied to (sorted, deduped).
    pub paths: Vec<String>,
}

/// The result of combining every applicable binding across policy layers.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, Default)]
pub struct CombinedPolicy {
    /// All applicable, valid bindings sorted by
    /// (layer_order, source_path, directive) ascending. Nothing applicable
    /// is ever dropped: conflicts are surfaced, never shadowed away.
    pub bindings: Vec<PolicyBinding>,
    /// Cross-layer polarity clashes detected while combining.
    pub conflicts: Vec<PolicyConflict>,
}

/// Combine all applicable, valid bindings across the policy hierarchy.
///
/// FR-002 rules implemented:
///
/// * Applicability: a binding applies iff it declares at least one glob in
///   `applies_to` and any of those globs matches any path in `task_paths`.
///   Bindings with an empty `applies_to` are invalid and skipped entirely
///   (never effective, never conflicting).
/// * Restriction: an unrestricted binding (`applies_to == ["**"]`) is only
///   valid from the unrestricted layers (Organization, Repository,
///   TaskContract); an unrestricted Module/ScopedRule binding is invalid
///   and skipped.
/// * No first-found-wins: every applicable valid binding is kept and
///   sorted by (layer_order, source_path, directive) ascending; conflicts
///   between layers are surfaced, never silently dropped.
///
/// Conflict detection heuristic (deterministic): for each pair of
/// applicable bindings from *different* layers (in sorted order, so side A
/// is always the broader layer), extract the subject: the longest word
/// shared by both directives, lowercased alphanumeric, length >= 4,
/// excluding the stopwords `always`/`never`/`with`/`from`/`that`/`this`/
/// `when`/`must`/`should`/`code`/`tests` (ties break lexicographically;
/// only its existence affects the outcome). A directive's polarity is
/// negated iff it contains any of `never`, `do not`, `don't`, `must not`,
/// `avoid`, `forbidden` (case-insensitive), otherwise affirmative. If the
/// pair shares a subject and the polarities differ, a [`PolicyConflict`]
/// is recorded with `paths` = task paths sorted and deduped, and each
/// returned binding's `conflicts_with` gains the counterpart directive.
pub fn combine(all: &[PolicyBinding], task_paths: &[&str]) -> CombinedPolicy {
    let mut bindings: Vec<PolicyBinding> = all
        .iter()
        .filter(|binding| valid(binding) && applicable(binding, task_paths))
        .cloned()
        .collect();

    // Deterministic order: broader layers first, then source path, then
    // directive text. This also fixes the pair order used below
    // (side A = the broader layer).
    bindings.sort_by(|a, b| {
        layer_order(&a.layer)
            .cmp(&layer_order(&b.layer))
            .then_with(|| a.source_path.cmp(&b.source_path))
            .then_with(|| a.directive.cmp(&b.directive))
    });

    let mut conflicts: Vec<PolicyConflict> = Vec::new();
    let mut conflict_pairs: Vec<(usize, usize)> = Vec::new();
    for i in 0..bindings.len() {
        for j in (i + 1)..bindings.len() {
            let (a, b) = (&bindings[i], &bindings[j]);
            if a.layer == b.layer {
                continue;
            }
            if shared_subject(&a.directive, &b.directive).is_none() {
                continue;
            }
            if negated(&a.directive) == negated(&b.directive) {
                continue;
            }
            conflict_pairs.push((i, j));
            conflicts.push(PolicyConflict {
                directive_a: a.directive.clone(),
                directive_b: b.directive.clone(),
                layer_a: a.layer.clone(),
                layer_b: b.layer.clone(),
                source_a: a.source_path.clone(),
                source_b: b.source_path.clone(),
                paths: sorted_paths(task_paths),
            });
        }
    }

    // Record the counterpart directive on each side of every conflict (in
    // the returned copies) without dropping either binding.
    for &(i, j) in &conflict_pairs {
        let for_i = bindings[j].directive.clone();
        let for_j = bindings[i].directive.clone();
        if !bindings[i].conflicts_with.contains(&for_i) {
            bindings[i].conflicts_with.push(for_i);
        }
        if !bindings[j].conflicts_with.contains(&for_j) {
            bindings[j].conflicts_with.push(for_j);
        }
    }

    CombinedPolicy {
        bindings,
        conflicts,
    }
}

/// A binding is well-formed: `applies_to` is non-empty, and unrestricted
/// scope is only permitted from the unrestricted layers.
fn valid(binding: &PolicyBinding) -> bool {
    if binding.applies_to.is_empty() {
        return false;
    }
    if is_unrestricted(binding) && !UNRESTRICTED_LAYERS.contains(&binding.layer) {
        return false;
    }
    true
}

/// `applies_to == ["**"]`.
fn is_unrestricted(binding: &PolicyBinding) -> bool {
    binding.applies_to.len() == 1 && binding.applies_to[0] == "**"
}

/// Any declared glob matches any task path.
fn applicable(binding: &PolicyBinding, task_paths: &[&str]) -> bool {
    binding
        .applies_to
        .iter()
        .any(|glob| task_paths.iter().any(|path| glob_match(glob, path)))
}

/// Directive polarity: negated iff it contains a negation marker.
fn negated(directive: &str) -> bool {
    let lower = directive.to_lowercase();
    NEGATION_MARKERS.iter().any(|m| lower.contains(*m))
}

/// Lowercased alphanumeric words of length >= 4, in order of appearance.
fn words(directive: &str) -> Vec<String> {
    directive
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| word.chars().count() >= 4)
        .map(str::to_string)
        .collect()
}

/// The subject shared by two directives: the longest word (length >= 4,
/// not a stopword) present in both. Ties break lexicographically.
fn shared_subject(a: &str, b: &str) -> Option<String> {
    let words_b: HashSet<String> = words(b).into_iter().collect();
    words(a)
        .into_iter()
        .filter(|word| !STOPWORDS.contains(&word.as_str()))
        .filter(|word| words_b.contains(word))
        .max_by(|x, y| x.len().cmp(&y.len()).then_with(|| x.cmp(y)))
}

/// Task paths sorted and deduped.
fn sorted_paths(task_paths: &[&str]) -> Vec<String> {
    let mut paths: Vec<String> = task_paths.iter().map(|p| p.to_string()).collect();
    paths.sort();
    paths.dedup();
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(layer: PolicyLayer, applies_to: &[&str], directive: &str) -> PolicyBinding {
        PolicyBinding {
            layer,
            source_path: PathBuf::from("mem"),
            applies_to: applies_to.iter().map(|s| s.to_string()).collect(),
            directive: directive.to_string(),
            conflicts_with: Vec::new(),
        }
    }

    #[test]
    fn layering_keeps_all_layers_sorted_by_layer_order() {
        let bindings = vec![
            b(PolicyLayer::TaskContract, &["**"], "task contract directive"),
            b(PolicyLayer::Module, &["src/**/*.rs"], "module directive alpha"),
            b(PolicyLayer::Organization, &["**"], "organization directive"),
            b(PolicyLayer::ScopedRule, &["src/**/*.rs"], "scoped directive"),
            b(PolicyLayer::Repository, &["**"], "repository directive"),
        ];
        let combined = combine(&bindings, &["src/x/a.rs"]);
        assert_eq!(combined.bindings.len(), 5);
        let orders: Vec<u8> = combined
            .bindings
            .iter()
            .map(|x| layer_order(&x.layer))
            .collect();
        assert_eq!(orders, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn glob_scoping_filters_by_task_paths() {
        let bindings = vec![b(
            PolicyLayer::Module,
            &["src/**/*.rs"],
            "prefer explicit error types",
        )];
        let excluded = combine(&bindings, &["lib/a.rs"]);
        assert!(excluded.bindings.is_empty());

        let included = combine(&bindings, &["src/x/a.rs"]);
        assert_eq!(included.bindings.len(), 1);
    }

    #[test]
    fn invalid_bindings_are_skipped() {
        let bindings = vec![
            // Empty applies_to is invalid: never effective.
            b(PolicyLayer::Module, &[], "module with empty scope"),
            // Unrestricted Module/ScopedRule bindings are invalid.
            b(PolicyLayer::ScopedRule, &["**"], "scoped rule unrestricted"),
            // Unrestricted Repository bindings are allowed.
            b(PolicyLayer::Repository, &["**"], "repository unrestricted"),
        ];
        let combined = combine(&bindings, &["src/a.rs"]);
        assert_eq!(combined.bindings.len(), 1);
        assert_eq!(combined.bindings[0].layer, PolicyLayer::Repository);
        assert!(combined.conflicts.is_empty());
    }

    #[test]
    fn conflicts_are_surfaced_not_dropped() {
        let repo = b(
            PolicyLayer::Repository,
            &["src/**/*.rs"],
            "Never use unwrap in production parsing paths",
        );
        let module = b(
            PolicyLayer::Module,
            &["src/**/*.rs"],
            "Always validate unwrap usage in parsing helpers",
        );
        let combined = combine(&[repo, module], &["src/x/a.rs"]);

        // FR-002: no first-found-wins — neither binding is dropped.
        assert_eq!(combined.bindings.len(), 2);

        assert_eq!(combined.conflicts.len(), 1);
        let c = &combined.conflicts[0];
        assert_eq!(c.directive_a, "Never use unwrap in production parsing paths");
        assert_eq!(
            c.directive_b,
            "Always validate unwrap usage in parsing helpers"
        );
        assert_eq!(c.layer_a, PolicyLayer::Repository);
        assert_eq!(c.layer_b, PolicyLayer::Module);
        assert_eq!(c.source_a, PathBuf::from("mem"));
        assert_eq!(c.source_b, PathBuf::from("mem"));
        assert_eq!(c.paths, vec!["src/x/a.rs".to_string()]);

        // conflicts_with is populated on both returned copies.
        let repo_out = combined
            .bindings
            .iter()
            .find(|x| x.layer == PolicyLayer::Repository)
            .unwrap();
        let module_out = combined
            .bindings
            .iter()
            .find(|x| x.layer == PolicyLayer::Module)
            .unwrap();
        assert_eq!(
            repo_out.conflicts_with,
            vec!["Always validate unwrap usage in parsing helpers".to_string()]
        );
        assert_eq!(
            module_out.conflicts_with,
            vec!["Never use unwrap in production parsing paths".to_string()]
        );
    }

    #[test]
    fn no_conflict_same_polarity_no_subject_or_same_layer() {
        // Shared subject, different layers, but same (affirmative) polarity.
        let same_polarity = combine(
            &[
                b(
                    PolicyLayer::Repository,
                    &["src/**/*.rs"],
                    "Always document parsing helpers",
                ),
                b(
                    PolicyLayer::Module,
                    &["src/**/*.rs"],
                    "Always cover parsing helpers with tests",
                ),
            ],
            &["src/x/a.rs"],
        );
        assert!(same_polarity.conflicts.is_empty());
        assert_eq!(same_polarity.bindings.len(), 2);
        assert!(same_polarity.bindings.iter().all(|x| x.conflicts_with.is_empty()));

        // Opposite polarity, different layers, but no shared subject.
        let no_subject = combine(
            &[
                b(
                    PolicyLayer::Repository,
                    &["src/**/*.rs"],
                    "Never commit generated bindings",
                ),
                b(
                    PolicyLayer::Module,
                    &["src/**/*.rs"],
                    "Always format macros before review",
                ),
            ],
            &["src/x/a.rs"],
        );
        assert!(no_subject.conflicts.is_empty());
        assert_eq!(no_subject.bindings.len(), 2);

        // Opposite polarity, shared subject, but the SAME layer.
        let same_layer = combine(
            &[
                b(
                    PolicyLayer::Module,
                    &["src/**/*.rs"],
                    "Never commit generated bindings",
                ),
                b(
                    PolicyLayer::Module,
                    &["src/**/*.rs"],
                    "Always review generated bindings",
                ),
            ],
            &["src/x/a.rs"],
        );
        assert!(same_layer.conflicts.is_empty());
        assert_eq!(same_layer.bindings.len(), 2);
    }

    #[test]
    fn serde_round_trips_combined_policy_and_conflict() {
        let conflict = PolicyConflict {
            directive_a: "Never use unwrap".to_string(),
            directive_b: "Always validate unwrap".to_string(),
            layer_a: PolicyLayer::Repository,
            layer_b: PolicyLayer::Module,
            source_a: PathBuf::from("mem"),
            source_b: PathBuf::from("mem"),
            paths: vec!["src/x/a.rs".to_string()],
        };
        let json = serde_json::to_string(&conflict).unwrap();
        assert!(json.contains("\"directive_a\""));
        assert!(json.contains("\"layer_b\""));
        assert!(json.contains("\"source_a\""));
        let back: PolicyConflict = serde_json::from_str(&json).unwrap();
        assert_eq!(back, conflict);

        let combined = CombinedPolicy {
            bindings: vec![PolicyBinding {
                layer: PolicyLayer::Repository,
                source_path: PathBuf::from("mem"),
                applies_to: vec!["src/**/*.rs".to_string()],
                directive: "Never use unwrap in production parsing paths".to_string(),
                conflicts_with: vec![
                    "Always validate unwrap usage in parsing helpers".to_string(),
                ],
            }],
            conflicts: vec![conflict],
        };
        let json = serde_json::to_string(&combined).unwrap();
        let back: CombinedPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(back, combined);
    }
}
