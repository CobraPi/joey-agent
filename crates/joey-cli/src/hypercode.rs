//! HyperCode: Parallel task optimization that decomposes work into the maximum
//! number of independent workstreams the system can support.
//!
//! HyperCode uses two specialized subagent roles:
//! - Explorer: Rapid codebase discovery, context gathering, and dependency analysis
//! - Implementor: Focused implementation of specific components
//!
//! Both can be configured with different models, thinking settings, and context windows
//! per provider, with the main model handling orchestration.

use std::collections::HashMap;

use joey_orchestration::{DelegationRequest, SubagentRole};

/// Configuration for HyperCode parallel optimization.
#[derive(Debug, Clone)]
pub struct HyperCodeConfig {
    /// Whether HyperCode mode is enabled (visual indicator in TUI).
    pub enabled: bool,
    /// Provider-specific model and settings for Explorer subagents.
    pub explorer_configs: HashMap<String, ExplorerConfig>,
    /// Provider-specific model and settings for Implementor subagents.
    pub implementor_configs: HashMap<String, ImplementorConfig>,
}

/// Configuration for Explorer subagents - specialized in codebase discovery.
#[derive(Debug, Clone)]
pub struct ExplorerConfig {
    /// Model to use for this provider (e.g., "gpt-4o", "claude-sonnet-4-20250514").
    pub model: String,
    /// Max context window in tokens (0 = use model default).
    pub max_tokens: usize,
    /// Max turns per subagent before summary.
    pub max_turns: usize,
    /// Reasoning level: "none", "low", "medium", "high".
    pub reasoning_level: String,
}

/// Configuration for Implementor subagents - specialized in focused implementation.
#[derive(Debug, Clone)]
pub struct ImplementorConfig {
    /// Model to use for this provider.
    pub model: String,
    /// Max context window in tokens (0 = use model default).
    pub max_tokens: usize,
    /// Max turns per subagent before summary.
    pub max_turns: usize,
    /// Reasoning level: "none", "low", "medium", "high".
    pub reasoning_level: String,
}

impl Default for HyperCodeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            explorer_configs: HashMap::new(),
            implementor_configs: HashMap::new(),
        }
    }
}

impl HyperCodeConfig {
    /// Get the explorer config for the given provider, or a sensible default.
    pub fn get_explorer_config(&self, provider: &str) -> ExplorerConfig {
        self.explorer_configs
            .get(provider)
            .cloned()
            .unwrap_or_else(|| ExplorerConfig {
                model: "gpt-4o".to_string(), // Balanced speed/capability
                max_tokens: 0,
                max_turns: 8,
                reasoning_level: "medium".to_string(), // Balanced reasoning for exploration
            })
    }

    /// Get the implementor config for the given provider, or a sensible default.
    pub fn get_implementor_config(&self, provider: &str) -> ImplementorConfig {
        self.implementor_configs
            .get(provider)
            .cloned()
            .unwrap_or_else(|| ImplementorConfig {
                model: "gpt-4o-mini".to_string(), // Fast, focused implementation
                max_tokens: 0,
                max_turns: 12,
                reasoning_level: "low".to_string(), // Lighter reasoning for implementation
            })
    }
    /// Set the explorer config for a specific provider.
    #[allow(dead_code)]
    pub fn set_explorer_config(&mut self, provider: String, config: ExplorerConfig) {
        self.explorer_configs.insert(provider, config);
    }

    /// Set the implementor config for a specific provider.
    #[allow(dead_code)]
    pub fn set_implementor_config(&mut self, provider: String, config: ImplementorConfig) {
        self.implementor_configs.insert(provider, config);
    }
}

/// Result type for HyperCode operations (shared between CLI and TUI).
#[derive(Debug, Clone)]
pub enum HyperCodeOutput {
    /// Multi-line output (status displays, errors, etc.)
    Text(Vec<String>),
    /// Toggle operation that returns the new enabled state.
    Toggle(bool),
    /// Configuration operation that returns success message.
    Configured(String),
}

/// HyperCode decomposes a task into parallel workstreams.
///
/// Strategy:
/// 1. Analyze the goal to identify independent workstreams
/// 2. Spawn parallel Explorer agents for disjoint code areas
/// 3. Based on Explorer results, spawn parallel Implementor agents
/// 4. Collect and merge results into a cohesive summary
#[allow(dead_code)]
pub struct HyperCode {
    config: HyperCodeConfig,
    provider: String,
}

impl HyperCodeConfig {
    /// Load HyperCode configuration from the joey Config.
    pub fn from_config(config: &joey_core::Config) -> Self {
        let mut hc = Self::default();
        
        // Load enabled state
        hc.enabled = config.get_bool("hypercode.enabled", false);
        
        // Load explorer configs per provider
        if let Some(explorer) = config.get("hypercode.explorer") {
            if let Some(mapping) = explorer.as_mapping() {
                for (provider, value) in mapping {
                    if let (Some(provider_str), Some(map)) = (provider.as_str(), value.as_mapping()) {
                        let mut ec = ExplorerConfig::default();
                        
                        if let Some(v) = map.get(&serde_yaml::Value::String("model".to_string())) {
                            if let Some(s) = v.as_str() {
                                ec.model = s.to_string();
                            }
                        }
                        if let Some(v) = map.get(&serde_yaml::Value::String("max_tokens".to_string())) {
                            if let Some(n) = v.as_u64() {
                                ec.max_tokens = n as usize;
                            }
                        }
                        if let Some(v) = map.get(&serde_yaml::Value::String("max_turns".to_string())) {
                            if let Some(n) = v.as_u64() {
                                ec.max_turns = n as usize;
                            }
                        }
                        if let Some(v) = map.get(&serde_yaml::Value::String("reasoning_level".to_string())) {
                            if let Some(s) = v.as_str() {
                                ec.reasoning_level = s.to_string();
                            }
                        }
                        
                        hc.explorer_configs.insert(provider_str.to_string(), ec);
                    }
                }
            }
        }
        
        // Load implementor configs per provider
        if let Some(implementor) = config.get("hypercode.implementor") {
            if let Some(mapping) = implementor.as_mapping() {
                for (provider, value) in mapping {
                    if let (Some(provider_str), Some(map)) = (provider.as_str(), value.as_mapping()) {
                        let mut ic = ImplementorConfig::default();
                        
                        if let Some(v) = map.get(&serde_yaml::Value::String("model".to_string())) {
                            if let Some(s) = v.as_str() {
                                ic.model = s.to_string();
                            }
                        }
                        if let Some(v) = map.get(&serde_yaml::Value::String("max_tokens".to_string())) {
                            if let Some(n) = v.as_u64() {
                                ic.max_tokens = n as usize;
                            }
                        }
                        if let Some(v) = map.get(&serde_yaml::Value::String("max_turns".to_string())) {
                            if let Some(n) = v.as_u64() {
                                ic.max_turns = n as usize;
                            }
                        }
                        if let Some(v) = map.get(&serde_yaml::Value::String("reasoning_level".to_string())) {
                            if let Some(s) = v.as_str() {
                                ic.reasoning_level = s.to_string();
                            }
                        }
                        
                        hc.implementor_configs.insert(provider_str.to_string(), ic);
                    }
                }
            }
        }
        
        hc
    }
    
    /// Persist the enabled state to config.
    pub fn save_enabled(enabled: bool) -> Result<(), String> {
        let mut config = joey_core::Config::load()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        config.set_and_save("hypercode.enabled", if enabled { "true" } else { "false" })
            .map_err(|e| format!("Failed to save config: {}", e))
    }
    
    /// Save explorer config for a provider to config.
    pub fn save_explorer_config(provider: &str, config: &ExplorerConfig) -> Result<(), String> {
        let mut cfg = joey_core::Config::load()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        
        // Save each setting individually
        let base_key = format!("hypercode.explorer.{}", provider);
        cfg.set_and_save(&format!("{}.model", base_key), &config.model)
            .map_err(|e| format!("Failed to save config: {}", e))?;
        cfg.set_and_save(&format!("{}.max_tokens", base_key), &config.max_tokens.to_string())
            .map_err(|e| format!("Failed to save config: {}", e))?;
        cfg.set_and_save(&format!("{}.max_turns", base_key), &config.max_turns.to_string())
            .map_err(|e| format!("Failed to save config: {}", e))?;
        cfg.set_and_save(&format!("{}.reasoning_level", base_key), &config.reasoning_level)
            .map_err(|e| format!("Failed to save config: {}", e))?;
        
        Ok(())
    }
    
    /// Save implementor config for a provider to config.
    pub fn save_implementor_config(provider: &str, config: &ImplementorConfig) -> Result<(), String> {
        let mut cfg = joey_core::Config::load()
            .map_err(|e| format!("Failed to load config: {}", e))?;
        
        // Save each setting individually
        let base_key = format!("hypercode.implementor.{}", provider);
        cfg.set_and_save(&format!("{}.model", base_key), &config.model)
            .map_err(|e| format!("Failed to save config: {}", e))?;
        cfg.set_and_save(&format!("{}.max_tokens", base_key), &config.max_tokens.to_string())
            .map_err(|e| format!("Failed to save config: {}", e))?;
        cfg.set_and_save(&format!("{}.max_turns", base_key), &config.max_turns.to_string())
            .map_err(|e| format!("Failed to save config: {}", e))?;
        cfg.set_and_save(&format!("{}.reasoning_level", base_key), &config.reasoning_level)
            .map_err(|e| format!("Failed to save config: {}", e))?;
        
        Ok(())
    }
}

#[allow(dead_code)]
impl HyperCode {
    /// Create a new HyperCode instance.
    pub fn new(config: HyperCodeConfig, provider: String) -> Self {
        Self { config, provider }
    }

    /// Decompose a task into parallel DelegationRequests.
    ///
    /// Returns a tuple: (explorer_tasks, implementor_tasks)
    /// where implementor_tasks are empty until explorer results are in.
    pub fn decompose(&self, goal: &str, context: Option<&str>) -> HyperCodePlan {
        // Use the main model to analyze and decompose.
        // For now, we use a heuristic-based decomposition.
        // In the future, this could be enhanced with an LLM call.
        
        let workstreams = self.identify_workstreams(goal, context);
        
        let explorer_requests = self.build_explorer_requests(&workstreams);
        
        HyperCodePlan {
            explorer_requests,
            implementor_requests: Vec::new(),
            workstreams,
        }
    }

    /// Identify independent workstreams from the goal.
    fn identify_workstreams(&self, goal: &str, context: Option<&str>) -> Vec<Workstream> {
        // Heuristic decomposition based on goal analysis.
        // This is a simplified approach; a production system would use
        // the main LLM to identify truly independent workstreams.
        
        let mut workstreams = Vec::new();
        
        // Check for common patterns that suggest parallelism.
        let goal_lower = goal.to_lowercase();
        
        if goal_lower.contains("multiple") || goal_lower.contains("several") {
            // Look for lists or numbered items
            if let Some(ctx) = context {
                if let Some(start) = ctx.find("1.") {
                    // Likely a numbered list of tasks
                    workstreams = self.parse_numbered_tasks(ctx, start);
                } else if let Some(start) = ctx.find("- ") {
                    // Bullet points
                    workstreams = self.parse_bulleted_tasks(ctx, start);
                }
            }
        }
        
        // If no clear decomposition found, create a single workstream.
        if workstreams.is_empty() {
            workstreams.push(Workstream {
                id: 0,
                focus: goal.to_string(),
                context: context.map(|s| s.to_string()),
                dependencies: Vec::new(),
            });
        }
        
        workstreams
    }

    fn parse_numbered_tasks(&self, text: &str, start: usize) -> Vec<Workstream> {
        let mut workstreams = Vec::new();
        let mut lines = text[start..].lines();
        
        while let Some(line) = lines.next() {
            let trimmed = line.trim();
            if trimmed.starts_with(|c: char| c.is_digit(10)) {
                // Extract the task description
                if let Some(pos) = trimmed.find(|c: char| c == '.' || c == ')') {
                    let task = trimmed[pos + 1..].trim();
                    if !task.is_empty() {
                        workstreams.push(Workstream {
                            id: workstreams.len(),
                            focus: task.to_string(),
                            context: None,
                            dependencies: Vec::new(),
                        });
                    }
                }
            }
        }
        
        workstreams
    }

    fn parse_bulleted_tasks(&self, text: &str, start: usize) -> Vec<Workstream> {
        let mut workstreams = Vec::new();
        let mut lines = text[start..].lines();
        
        while let Some(line) = lines.next() {
            let trimmed = line.trim();
            if trimmed.starts_with("- ") || trimmed.starts_with("* ") {
                let task = trimmed[2..].trim();
                if !task.is_empty() {
                    workstreams.push(Workstream {
                        id: workstreams.len(),
                        focus: task.to_string(),
                        context: None,
                        dependencies: Vec::new(),
                    });
                }
            }
        }
        
        workstreams
    }

    fn build_explorer_requests(&self, workstreams: &[Workstream]) -> Vec<DelegationRequest> {
        let explorer_cfg = self.config.get_explorer_config(&self.provider);
        
        workstreams
            .iter()
            .map(|ws| DelegationRequest {
                goal: format!(
                    "Explore the codebase to understand the scope and context for: {}",
                    ws.focus
                ),
                context: ws.context.clone(),
                tasks: Vec::new(),
                model: Some(explorer_cfg.model.clone()),
                toolsets: vec!["file".to_string(), "web".to_string()],
                max_turns: Some(explorer_cfg.max_turns),
                persist: false,
                role: SubagentRole::Orchestrator,
                workdir: None,
                category: None,
                subagent_type: None,
                load_skills: Vec::new(),
                prompt_append: Some(
                    "You are an Explorer agent. Your job is to:\n\
                     1. Locate relevant code, tests, and documentation\n\
                     2. Identify dependencies and relationships\n\
                     3. Surface any gotchas or edge cases\n\
                     4. Provide a clear summary of what needs to be done\n\
                     Keep your summary concise and actionable."
                        .to_string(),
                ),
            })
            .collect()
    }

    /// Generate implementor tasks based on explorer results.
    pub fn generate_implementor_tasks(
        &self,
        explorer_results: &[(usize, String)],
    ) -> Vec<DelegationRequest> {
        let impl_cfg = self.config.get_implementor_config(&self.provider);
        
        explorer_results
            .iter()
            .map(|(_ws_id, summary)| DelegationRequest {
                goal: format!("Implement based on exploration results",),
                context: Some(format!(
                    "Exploration summary:\n\
                     {}\n\
                     \n\
                     Implement the identified changes cleanly.",
                    summary
                )),
                tasks: Vec::new(),
                model: Some(impl_cfg.model.clone()),
                toolsets: vec![
                    "file".to_string(),
                    "terminal".to_string(),
                    "coding".to_string(),
                ],
                max_turns: Some(impl_cfg.max_turns),
                persist: false,
                role: SubagentRole::Leaf,
                workdir: None,
                category: None,
                subagent_type: None,
                load_skills: Vec::new(),
                prompt_append: Some(
                    "You are an Implementor agent. Your job is to:\n\
                     1. Write clean, correct code\n\
                     2. Ensure tests pass\n\
                     3. Follow the project's conventions\n\
                     4. Report back with what was done\n\
                     Focus on implementation, not exploration."
                        .to_string(),
                ),
            })
            .collect()
    }
}

/// A workstream represents an independent unit of work.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Workstream {
    pub id: usize,
    pub focus: String,
    pub context: Option<String>,
    pub dependencies: Vec<usize>,
}

/// The execution plan for a HyperCode task.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct HyperCodePlan {
    pub explorer_requests: Vec<DelegationRequest>,
    pub implementor_requests: Vec<DelegationRequest>,
    pub workstreams: Vec<Workstream>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hypercode_config_defaults() {
        let config = HyperCodeConfig::default();
        let explorer = config.get_explorer_config("unknown-provider");
        assert_eq!(explorer.model, "gpt-4o");
        assert_eq!(explorer.reasoning_level, "medium");
        
        let impl_cfg = config.get_implementor_config("unknown-provider");
        assert_eq!(impl_cfg.model, "gpt-4o-mini");
        assert_eq!(impl_cfg.reasoning_level, "low");
    }

    #[test]
    fn test_config_per_provider() {
        let mut config = HyperCodeConfig::default();
        config.set_explorer_config(
            "test-provider".to_string(),
            ExplorerConfig {
                model: "custom-explorer".to_string(),
                max_tokens: 8000,
                max_turns: 5,
                reasoning_level: "high".to_string(),
            },
        );
        
        let explorer = config.get_explorer_config("test-provider");
        assert_eq!(explorer.model, "custom-explorer");
        assert_eq!(explorer.max_tokens, 8000);
        assert_eq!(explorer.reasoning_level, "high");
    }
}