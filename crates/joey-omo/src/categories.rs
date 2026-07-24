//! Category-based delegation system.
//!
//! 11 built-in categories, each with a model fallback chain. Categories route
//! to Sisyphus-Junior with the category's resolved model and prompt append.
//!
//! 1-to-1 port of OMO's `category-model-requirements.ts`.

use crate::agents::registry::AgentRegistry;
use crate::models::{resolve_model, AvailableModelSet, FallbackEntry, ModelRequirement};

// ── CategoryConfig ──────────────────────────────────────────────────

/// A semantic delegation target. Port of data-model.md `CategoryConfig`.
#[derive(Debug, Clone)]
pub struct CategoryConfig {
    /// Category name (e.g. "visual-engineering", "quick").
    pub name: String,
    /// What this category is for.
    pub description: String,
    /// Fallback chain for this category.
    pub model_requirement: ModelRequirement,
    /// Override temperature (default 0.5 for categories).
    pub temperature: Option<f64>,
    /// Optional text appended to Junior's prompt.
    pub prompt_append: Option<String>,
}

// ── Category Model Requirement Constants (T013) ─────────────────────
//
// 1-to-1 port of OMO's `category-model-requirements.ts`.

use crate::models::FallbackEntry as FE;

fn visual_engineering_req() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("gemini-3.1-pro", Some("high"), &["google", "github-copilot", "opencode", "vercel"]),
            FE::new("glm-5", None, &["zai-coding-plan", "opencode", "bailian-coding-plan", "vercel"]),
            FE::new("claude-opus-4-8", Some("max"), &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("glm-5.2", None, &["opencode-go", "vercel"]),
            FE::new("kimi-k3", None, &["kimi-for-coding"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

fn ultrabrain_req() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("gpt-5.6-sol", Some("xhigh"), &["openai", "vercel"]),
            FE::new("gpt-5.6-sol", Some("high"), &["github-copilot"]),
            FE::new("gpt-5.6-sol", Some("xhigh"), &["openai", "opencode", "vercel"]),
            FE::new("gemini-3.1-pro", Some("high"), &["google", "github-copilot", "opencode", "vercel"]),
            FE::new("claude-opus-4-8", Some("max"), &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("glm-5.2", None, &["opencode-go", "vercel"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

fn deep_req() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("gpt-5.6-terra", Some("xhigh"), &["openai", "vercel"]),
            FE::new("gpt-5.6-terra", Some("high"), &["github-copilot"]),
            FE::new("gpt-5.6-sol", Some("high"), &["openai", "github-copilot", "vercel"]),
            FE::new("gpt-5.6-sol", Some("medium"), &["openai", "github-copilot", "opencode", "vercel"]),
            FE::new("claude-opus-4-8", Some("max"), &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("gemini-3.1-pro", Some("high"), &["google", "github-copilot", "opencode", "vercel"]),
            FE::new("kimi-k3", None, &["opencode-go", "vercel"]),
            FE::new("glm-5.2", None, &["opencode-go", "vercel"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

fn artistry_req() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("gemini-3.1-pro", Some("high"), &["google", "github-copilot", "opencode", "vercel"]),
            FE::new("claude-opus-4-8", Some("max"), &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("gpt-5.6-sol", Some("high"), &["openai", "github-copilot", "opencode", "vercel"]),
            FE::new("kimi-k3", None, &["opencode-go", "vercel"]),
            FE::new("glm-5.2", None, &["opencode-go", "vercel"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

fn quick_req() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("gpt-5.4-mini", None, &["openai", "github-copilot", "opencode", "vercel"]),
            FE::new("claude-haiku-4-5", None, &["anthropic", "github-copilot", "vercel"]),
            FE::new("gemini-3-flash", None, &["google", "github-copilot", "opencode", "vercel"]),
            FE::new("minimax-m3", None, &["opencode-go", "vercel"]),
            FE::new("MiniMax-M3", None, &["minimax-coding-plan", "minimax-cn-coding-plan"]),
            FE::new("minimax-m2.7", None, &["opencode-go", "vercel"]),
            FE::new("gpt-5-nano", None, &["opencode", "vercel"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

fn unspecified_low_req() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("gpt-5.6-luna", Some("xhigh"), &["openai", "vercel"]),
            FE::new("gpt-5.6-luna", Some("high"), &["github-copilot"]),
            FE::new("claude-sonnet-4-6", None, &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("gpt-5.6-sol", Some("medium"), &["openai", "opencode", "vercel"]),
            FE::new("kimi-k3", None, &["opencode-go", "vercel"]),
            FE::new("gemini-3-flash", None, &["google", "github-copilot", "opencode", "vercel"]),
            FE::new("minimax-m3", None, &["opencode-go", "vercel"]),
            FE::new("MiniMax-M3", None, &["minimax-coding-plan", "minimax-cn-coding-plan"]),
            FE::new("minimax-m2.7", None, &["opencode-go", "vercel"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

fn unspecified_high_req() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("claude-opus-4-8", Some("max"), &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("gpt-5.6-sol", Some("high"), &["openai", "github-copilot", "opencode", "vercel"]),
            FE::new("glm-5", None, &["zai-coding-plan", "opencode", "bailian-coding-plan", "vercel"]),
            FE::new("kimi-k3", None, &["kimi-for-coding"]),
            FE::new("glm-5.2", None, &["opencode-go", "vercel"]),
            FE::new("kimi-k3", None, &["opencode", "bailian-coding-plan", "vercel", "moonshotai", "moonshotai-cn", "firmware", "ollama-cloud", "aihubmix"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

fn writing_req() -> ModelRequirement {
    ModelRequirement {
        fallback_chain: vec![
            FE::new("gemini-3-flash", None, &["google", "github-copilot", "opencode", "vercel"]),
            FE::new("kimi-k3", None, &["opencode-go", "vercel"]),
            FE::new("claude-sonnet-4-6", None, &["anthropic", "github-copilot", "opencode", "vercel"]),
            FE::new("minimax-m3", None, &["opencode-go", "vercel"]),
            FE::new("MiniMax-M3", None, &["minimax-coding-plan", "minimax-cn-coding-plan"]),
            FE::new("minimax-m2.7", None, &["opencode-go", "vercel"]),
        ],
        requires_any_model: false,
        requires_provider: None,
    }
}

/// The 11 built-in category definitions. 1-to-1 with OMO source.
pub fn builtin_categories() -> Vec<CategoryConfig> {
    vec![
        CategoryConfig {
            name: "visual-engineering".into(),
            description: "Frontend/UI work, design, visual implementation".into(),
            model_requirement: visual_engineering_req(),
            temperature: Some(0.5),
            prompt_append: Some("You are working on a visual-engineering task. Focus on frontend, UI, design fidelity, and visual implementation.".into()),
        },
        CategoryConfig {
            name: "ultrabrain".into(),
            description: "Hard logic, strategic thinking, complex reasoning".into(),
            model_requirement: ultrabrain_req(),
            temperature: Some(0.3),
            prompt_append: Some("You are working on an ultrabrain task. This requires maximum reasoning depth, careful analysis, and strategic thinking.".into()),
        },
        CategoryConfig {
            name: "deep".into(),
            description: "Autonomous research and execution, deep work".into(),
            model_requirement: deep_req(),
            temperature: Some(0.4),
            prompt_append: Some("You are working on a deep task. Conduct thorough research and autonomous execution with maximum diligence.".into()),
        },
        CategoryConfig {
            name: "artistry".into(),
            description: "Creative and design work, aesthetics".into(),
            model_requirement: artistry_req(),
            temperature: Some(0.7),
            prompt_append: Some("You are working on an artistry task. Prioritize creativity, design quality, and aesthetic excellence.".into()),
        },
        CategoryConfig {
            name: "quick".into(),
            description: "Fast, cheap tasks — minimal tokens, quick turnaround".into(),
            model_requirement: quick_req(),
            temperature: Some(0.2),
            prompt_append: Some("You are working on a quick task. Be fast, efficient, and direct. Minimize tokens while delivering correct results.".into()),
        },
        CategoryConfig {
            name: "unspecified-low".into(),
            description: "Low-effort fallback category".into(),
            model_requirement: unspecified_low_req(),
            temperature: Some(0.3),
            prompt_append: None,
        },
        CategoryConfig {
            name: "unspecified-high".into(),
            description: "High-effort fallback category".into(),
            model_requirement: unspecified_high_req(),
            temperature: Some(0.4),
            prompt_append: None,
        },
        CategoryConfig {
            name: "writing".into(),
            description: "Prose and documentation, content creation".into(),
            model_requirement: writing_req(),
            temperature: Some(0.5),
            prompt_append: Some("You are working on a writing task. Focus on clear, well-structured prose and documentation.".into()),
        },
        CategoryConfig {
            name: "quick-rust".into(),
            description: "Quick Rust-specific tasks".into(),
            model_requirement: quick_req(),
            temperature: Some(0.2),
            prompt_append: Some("You are working on a quick Rust task. Apply Rust best practices: ownership, lifetimes, error handling with anyhow/thiserror.".into()),
        },
        CategoryConfig {
            name: "quick-zig".into(),
            description: "Quick Zig-specific tasks".into(),
            model_requirement: quick_req(),
            temperature: Some(0.2),
            prompt_append: Some("You are working on a quick Zig task. Apply Zig best practices: explicit memory management, error sets, comptime.".into()),
        },
        CategoryConfig {
            name: "git".into(),
            description: "Git operations and version control".into(),
            model_requirement: quick_req(),
            temperature: Some(0.1),
            prompt_append: Some("You are working on a git task. Handle version control operations carefully: commits, branches, merges, rebases.".into()),
        },
    ]
}

// ── Category Resolution ─────────────────────────────────────────────

/// The resolved category delegation: model + config.
pub struct ResolvedCategory {
    pub model: String,
    pub variant: Option<String>,
    pub config: CategoryConfig,
}

/// Resolve a category by name against available models (T055).
///
/// Returns None if the category name is unknown or its model chain
/// doesn't resolve.
pub fn resolve_category(
    name: &str,
    registry: &AgentRegistry,
) -> Option<ResolvedCategory> {
    let config = registry.categories().iter().find(|c| c.name == name)?;
    let available = registry.available_models();
    let (model, variant) = resolve_model(&config.model_requirement, available)?;
    Some(ResolvedCategory {
        model,
        variant,
        config: config.clone(),
    })
}

/// Validate that category and subagent_type are not both specified (T056).
/// BC-011: mutual exclusivity. Returns an error message if both are present.
pub fn validate_delegation(
    category: Option<&str>,
    subagent_type: Option<&str>,
) -> Result<(), String> {
    match (category, subagent_type) {
        (Some(_), Some(_)) => {
            Err("Cannot specify both 'category' and 'subagent_type' — they are mutually exclusive (BC-011).".into())
        }
        (None, None) => {
            Err("Must specify at least one of 'category' or 'subagent_type' (BC-012).".into())
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exactly_eleven_categories() {
        let cats = builtin_categories();
        assert_eq!(cats.len(), 11, "exactly 11 categories");
    }

    #[test]
    fn canonical_category_names() {
        let cats = builtin_categories();
        let names: Vec<&str> = cats.iter().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"visual-engineering"));
        assert!(names.contains(&"ultrabrain"));
        assert!(names.contains(&"deep"));
        assert!(names.contains(&"artistry"));
        assert!(names.contains(&"quick"));
        assert!(names.contains(&"unspecified-low"));
        assert!(names.contains(&"unspecified-high"));
        assert!(names.contains(&"writing"));
        assert!(names.contains(&"quick-rust"));
        assert!(names.contains(&"quick-zig"));
        assert!(names.contains(&"git"));
    }

    #[test]
    fn validate_delegation_mutual_exclusivity() {
        // T062: both specified → error
        assert!(validate_delegation(Some("quick"), Some("oracle")).is_err());
        // Only category → Ok
        assert!(validate_delegation(Some("quick"), None).is_ok());
        // Only subagent_type → Ok
        assert!(validate_delegation(None, Some("oracle")).is_ok());
        // Neither → error (BC-012)
        assert!(validate_delegation(None, None).is_err());
    }

    #[test]
    fn quick_category_resolves_with_gpt_mini() {
        // T061: resolve_category("quick") with only Gpt-mini available returns the quick model
        let mut available = AvailableModelSet::new();
        available.add_model("gpt-5.4-mini".into());
        // Test the chain directly since resolve_category needs a full registry
        let cats = builtin_categories();
        let quick = cats.iter().find(|c| c.name == "quick").unwrap();
        let (model, _) = resolve_model(&quick.model_requirement, &available).unwrap();
        assert_eq!(model, "gpt-5.4-mini");
    }
}
