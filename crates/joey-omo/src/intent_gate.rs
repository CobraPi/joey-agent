//! IntentGate: keyword detection for ultrawork, hyperplan, and team modes.
//!
//! Port of OMO's keyword-detector constants. Detects `ultrawork`/`ulw`,
//! `hyperplan`, and `team` keywords in user messages.

use crate::agents::registry::AgentRegistry;

/// The type of keyword detected in a user message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeywordType {
    /// `ultrawork` or `ulw` — activates ultrawork mode.
    Ultrawork,
    /// `hyperplan` — activates the adversarial hyperplan workflow.
    Hyperplan,
    /// Both `hyperplan` and `ultrawork` in the same message.
    HyperplanUltraworkCombo,
    /// `team` — activates team mode.
    Team,
}

impl KeywordType {
    /// The first-response message the agent must emit (BC-024).
    pub fn first_response(self) -> Option<&'static str> {
        match self {
            Self::Ultrawork | Self::HyperplanUltraworkCombo => {
                Some("ULTRAWORK MODE ENABLED!")
            }
            Self::Hyperplan => Some("HYPERPLAN MODE ENABLED!"),
            Self::Team => Some("TEAM MODE ENABLED!"),
        }
    }
}

/// Detect keywords in a user message (T103).
///
/// Returns the detected keyword type, if any. Scans for:
/// - `ultrawork` or `ulw` as standalone words
/// - `hyperplan` as a standalone word
/// - combo: both `hyperplan` and `ultrawork`/`ulw`
/// - `team` as a standalone word
///
/// Priority: combo > individual (if both present, returns combo).
pub fn detect_keyword(message: &str) -> Option<KeywordType> {
    let lower = message.to_ascii_lowercase();

    let has_ultrawork = contains_word(&lower, "ultrawork") || contains_word(&lower, "ulw");
    let has_hyperplan = contains_word(&lower, "hyperplan");
    let has_team = contains_word(&lower, "team");

    if has_hyperplan && has_ultrawork {
        return Some(KeywordType::HyperplanUltraworkCombo);
    }
    if has_ultrawork {
        return Some(KeywordType::Ultrawork);
    }
    if has_hyperplan {
        return Some(KeywordType::Hyperplan);
    }
    if has_team {
        return Some(KeywordType::Team);
    }
    None
}

/// Check if a word appears as a standalone word (not a substring of another word).
fn contains_word(haystack: &str, needle: &str) -> bool {
    for word in haystack.split_whitespace() {
        let cleaned: String = word.chars().filter(|c| c.is_alphanumeric() || *c == '-').collect();
        if cleaned == needle {
            return true;
        }
    }
    false
}

/// Check if ultrawork activation is valid for the given agent (T106).
///
/// Valid on: Default, Sisyphus, Hephaestus, Atlas.
/// IGNORED on: Prometheus (read-only planner incompatible) — FR-022, Q2.
pub fn ultrawork_valid_for_agent(agent_name: &str) -> bool {
    // Default agent is represented as "" or "default" by the caller.
    matches!(
        agent_name,
        "" | "default" | "sisyphus" | "hephaestus" | "atlas"
    )
}

/// Check if ultrawork should activate given the keyword and active agent.
/// Returns Some(first_response_message) if it should activate, None if ignored.
pub fn check_ultrawork_activation(
    keyword: KeywordType,
    agent_name: &str,
) -> Option<&'static str> {
    match keyword {
        KeywordType::Ultrawork | KeywordType::HyperplanUltraworkCombo => {
            if ultrawork_valid_for_agent(agent_name) {
                keyword.first_response()
            } else {
                None // Silently ignored on Prometheus (BC-025)
            }
        }
        _ => keyword.first_response(),
    }
}

/// Check ultrawork activation using the agent registry (overload that takes
/// a registry for future extensibility). Currently delegates to the name-based
/// check.
pub fn check_ultrawork_activation_registry(
    keyword: KeywordType,
    _registry: &AgentRegistry,
    agent_name: &str,
) -> Option<&'static str> {
    check_ultrawork_activation(keyword, agent_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T111: detect ultrawork and ulw in messages.
    #[test]
    fn detect_ultrawork_keywords() {
        assert_eq!(detect_keyword("ultrawork implement a CLI"), Some(KeywordType::Ultrawork));
        assert_eq!(detect_keyword("ulw implement a CLI"), Some(KeywordType::Ultrawork));
        assert_eq!(detect_keyword("let's ultrawork this"), Some(KeywordType::Ultrawork));
    }

    /// T111: detect hyperplan.
    #[test]
    fn detect_hyperplan_keyword() {
        assert_eq!(detect_keyword("hyperplan the architecture"), Some(KeywordType::Hyperplan));
    }

    /// T111: detect combo "hyperplan ultrawork".
    #[test]
    fn detect_hyperplan_ultrawork_combo() {
        assert_eq!(
            detect_keyword("hyperplan ultrawork the whole thing"),
            Some(KeywordType::HyperplanUltraworkCombo)
        );
    }

    /// T111: detect team keyword.
    #[test]
    fn detect_team_keyword() {
        assert_eq!(detect_keyword("team mode go"), Some(KeywordType::Team));
    }

    /// No keyword in normal messages.
    #[test]
    fn no_keyword_in_normal_message() {
        assert!(detect_keyword("help me write a function").is_none());
        assert!(detect_keyword("what is the status?").is_none());
    }

    /// T112: ultrawork activation on Sisyphus returns Some(message); on
    /// Prometheus returns None (ignored).
    #[test]
    fn ultrawork_activation_agent_validation() {
        // On Sisyphus → activates
        assert_eq!(
            check_ultrawork_activation(KeywordType::Ultrawork, "sisyphus"),
            Some("ULTRAWORK MODE ENABLED!")
        );
        // On Default → activates
        assert_eq!(
            check_ultrawork_activation(KeywordType::Ultrawork, "default"),
            Some("ULTRAWORK MODE ENABLED!")
        );
        // On Prometheus → ignored (None)
        assert_eq!(
            check_ultrawork_activation(KeywordType::Ultrawork, "prometheus"),
            None
        );
    }

    /// Word boundary: "ultrawork" inside another word doesn't trigger.
    #[test]
    fn word_boundary_matching() {
        // "ultraworking" should NOT match "ultrawork" or "ulw"
        assert!(detect_keyword("I'm ultraworking on this").is_none());
        // But "ultrawork" as a standalone word should
        assert_eq!(
            detect_keyword("start ultrawork now"),
            Some(KeywordType::Ultrawork)
        );
    }
}
