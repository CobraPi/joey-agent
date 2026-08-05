//! `ModuleId` — a distinct LLM call site in the compound system (T005).
//!
//! Seeded with the three real intercept points and extensible additively via
//! the `Custom` variant so new call sites can be added without breaking the
//! on-disk schema (Constitution VII).

use serde::{Deserialize, Serialize};

/// A distinct LLM call site ("module") in the agent's compound system.
///
/// Only three real call sites exist today (research.md §2): the main reasoning
/// turn, history compression, and subagent dispatch. The `Custom` variant lets
/// future call sites be added additively without a schema bump.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ModuleId {
    /// The main agent reasoning turn (agent.rs `build_request` intercept).
    MainTurn,
    /// History compression side-LLM (summary.rs intercept).
    Compression,
    /// A delegated subagent goal (orchestration `resolve_model` intercept).
    Subagent,
    /// A named call site added after initial release (additive; Constitution VII).
    /// Persisted as `{"custom":"<name>"}`; new variants don't break old maps.
    Custom(String),
}

impl ModuleId {
    /// Validate that a Custom name matches `^[a-z][a-z0-9_]{0,31}$`.
    pub fn validate_custom_name(name: &str) -> Result<(), String> {
        if name.is_empty() {
            return Err("custom module name must not be empty".to_string());
        }
        if name.len() > 32 {
            return Err(format!(
                "custom module name '{}' exceeds 32 chars",
                name
            ));
        }
        let mut chars = name.chars();
        let first = chars.next().unwrap();
        if !first.is_ascii_lowercase() {
            return Err(format!(
                "custom module name '{}' must start with lowercase a-z",
                name
            ));
        }
        for c in chars {
            if !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
                return Err(format!(
                    "custom module name '{}' contains invalid char '{}'; allowed: [a-z0-9_]",
                    name, c
                ));
            }
        }
        Ok(())
    }

    /// Stable string identifier used for display and debugging.
    pub fn as_str(&self) -> &str {
        match self {
            ModuleId::MainTurn => "main_turn",
            ModuleId::Compression => "compression",
            ModuleId::Subagent => "subagent",
            ModuleId::Custom(n) => n.as_str(),
        }
    }

    /// Parse a module id from its string form.
    ///
    /// Accepts the snake_case enum ids (`main_turn`, `compression`, `subagent`)
    /// or `custom:<name>` for an additive module. Returns `Err` with a helpful
    /// message for unknown modules or invalid custom names (contracts/llm-selector-command.md
    /// "Module argument grammar").
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "main_turn" => Ok(ModuleId::MainTurn),
            "compression" => Ok(ModuleId::Compression),
            "subagent" => Ok(ModuleId::Subagent),
            _ => {
                if let Some(name) = s.strip_prefix("custom:") {
                    ModuleId::validate_custom_name(name)?;
                    Ok(ModuleId::Custom(name.to_string()))
                } else {
                    Err(format!(
                        "unknown module '{}'. Valid: main_turn, compression, subagent, custom:<name>",
                        s
                    ))
                }
            }
        }
    }
}

impl std::fmt::Display for ModuleId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serde_snake_case() {
        let json = serde_json::to_string(&ModuleId::MainTurn).unwrap();
        assert_eq!(json, "\"main_turn\"");
        let json = serde_json::to_string(&ModuleId::Compression).unwrap();
        assert_eq!(json, "\"compression\"");
        let json = serde_json::to_string(&ModuleId::Subagent).unwrap();
        assert_eq!(json, "\"subagent\"");
    }

    #[test]
    fn test_serde_custom() {
        let id = ModuleId::Custom("vision_check".to_string());
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "{\"custom\":\"vision_check\"}");
        let back: ModuleId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn test_roundtrip_all_variants() {
        for id in [
            ModuleId::MainTurn,
            ModuleId::Compression,
            ModuleId::Subagent,
            ModuleId::Custom("title_gen".to_string()),
        ] {
            let json = serde_json::to_string(&id).unwrap();
            let back: ModuleId = serde_json::from_str(&json).unwrap();
            assert_eq!(id, back);
        }
    }

    #[test]
    fn test_validate_custom_name_ok() {
        assert!(ModuleId::validate_custom_name("a").is_ok());
        assert!(ModuleId::validate_custom_name("abc").is_ok());
        assert!(ModuleId::validate_custom_name("a_b_c").is_ok());
        assert!(ModuleId::validate_custom_name("a1b2").is_ok());
        assert!(ModuleId::validate_custom_name(&"a".repeat(32)).is_ok());
    }

    #[test]
    fn test_validate_custom_name_bad() {
        assert!(ModuleId::validate_custom_name("").is_err());
        assert!(ModuleId::validate_custom_name("A").is_err()); // uppercase
        assert!(ModuleId::validate_custom_name("1abc").is_err()); // digit first
        assert!(ModuleId::validate_custom_name("a-b").is_err()); // hyphen
        assert!(ModuleId::validate_custom_name(&"a".repeat(33)).is_err()); // too long
    }
}
