//! `SelectorQuery` — query/control API for the `/llm-selector` command (T017).
//!
//! Returns text-renderable structs the command handler formats into the
//! output shapes defined in contracts/llm-selector-command.md.

use crate::allocator::{SelectorConfig, SelectorEngine};
use crate::candidate::CatalogSource;
use crate::model_allocator::ModelAllocator;

/// Status report (FR-001): enabled/disabled state, pool size, diagnoser model.
#[derive(Debug, Clone)]
pub struct StatusReport {
    pub active: bool,
    pub enabled: bool,
    pub configured_model: String,
    pub pool_size: usize,
    pub pool_source: CatalogSource,
    /// True when the pool has exactly one eligible model (Edge Case: the
    /// selector is a no-op pass-through; no cross-module diversity is possible).
    pub pool_is_single_model: bool,
    pub diagnoser_model: String,
    pub learning_budget: u32,
    pub budget_used: u32,
    pub entries: Vec<AllocationRow>,
}

/// One row of the allocation map (FR-011).
#[derive(Debug, Clone)]
pub struct AllocationRow {
    pub module: String,
    pub model_id: String,
    pub pinned: bool,
    pub implicit_pin: bool,
    pub reason: String,
    pub estimated_performance: Option<f64>,
    pub updated_at: Option<String>,
}

/// One candidate in the pool (FR-003).
#[derive(Debug, Clone)]
pub struct CandidateRow {
    pub id: String,
    pub provider: String,
    pub tier: String,
    pub context_window: u64,
    pub supports_tools: bool,
    pub supports_vision: bool,
}

/// A diagnoser judgment (FR-018).
#[derive(Debug, Clone)]
pub struct DiagnosticRow {
    pub at: String,
    pub module: String,
    pub signal: String,
    pub implicated_model: String,
    pub rationale: String,
}

/// The query API: read/write the selector state through the engine.
pub struct SelectorQuery<'a> {
    engine: &'a SelectorEngine,
}

impl<'a> SelectorQuery<'a> {
    pub fn new(engine: &'a SelectorEngine) -> Self {
        Self { engine }
    }

    /// Build a status report (FR-001).
    pub fn status(&self) -> StatusReport {
        let map = self.engine.map_snapshot();
        let pool = self.engine.pool();
        let entries = map
            .entries
            .iter()
            .map(|e| AllocationRow {
                module: e.module.to_string(),
                model_id: e.model_id.clone(),
                pinned: e.pinned,
                implicit_pin: e.implicit_pin,
                reason: e.reason.clone(),
                estimated_performance: e.estimated_performance,
                updated_at: e.updated_at.clone(),
            })
            .collect();
        StatusReport {
            active: self.engine.is_active(),
            enabled: map.enabled,
            configured_model: self.engine.configured_model(),
            pool_size: pool.len(),
            pool_source: pool.source,
            pool_is_single_model: pool.len() == 1,
            diagnoser_model: map.diagnoser_model,
            learning_budget: map.learning_budget,
            budget_used: map.budget_used_this_cycle,
            entries,
        }
    }

    /// List the candidate pool (FR-003).
    pub fn pool(&self) -> Vec<CandidateRow> {
        self.engine
            .pool()
            .models
            .iter()
            .map(|m| CandidateRow {
                id: m.id.clone(),
                provider: m.provider.clone(),
                tier: format!("{:?}", m.tier).to_ascii_lowercase(),
                context_window: m.context_window,
                supports_tools: m.supports_tools,
                supports_vision: m.supports_vision,
            })
            .collect()
    }

    /// List recent diagnostics (FR-018).
    pub fn diagnostics(&self, limit: usize) -> Vec<DiagnosticRow> {
        let map = self.engine.map_snapshot();
        map.diagnostics
            .iter()
            .rev()
            .take(limit)
            .map(|d| DiagnosticRow {
                at: d.at.clone(),
                module: d.module.to_string(),
                signal: format!("{:?}", d.signal).to_ascii_lowercase(),
                implicated_model: d.implicated_model.clone(),
                rationale: d.rationale.clone(),
            })
            .collect()
    }

    /// Enable the selector (FR-002).
    pub fn enable(&self) {
        let mut cfg = self.engine.config_snapshot();
        cfg.enabled = true;
        self.engine.update_config(cfg);
    }

    /// Disable the selector (FR-002). Falls back to the configured model.
    pub fn disable(&self) {
        let mut cfg = self.engine.config_snapshot();
        cfg.enabled = false;
        self.engine.update_config(cfg);
    }

    /// Set the learning budget (FR-009).
    pub fn set_budget(&self, budget: u32) {
        let mut cfg = self.engine.config_snapshot();
        cfg.learning_budget = budget;
        self.engine.update_config(cfg);
    }

    /// Set the diagnoser model (FR-008). Validates versatile-tier eligibility.
    /// Returns Err(reason) on rejection (caller exits non-zero).
    pub fn set_diagnoser_model(&self, model_id: &str) -> Result<(), String> {
        self.engine.set_diagnoser_model(model_id)
    }

    /// Pin a module to a model (FR-012).
    pub fn pin(&self, module: crate::ModuleId, model_id: String) -> Result<(), String> {
        self.engine.pin_module(module, model_id)
    }

    /// Unpin a module (FR-012).
    pub fn unpin(&self, module: &crate::ModuleId) -> Result<(), String> {
        self.engine.unpin_module(module)
    }
}

// ── Engine accessors needed by the query API ───────────────────────────────

impl SelectorEngine {
    /// Get the configured model id.
    pub fn configured_model(&self) -> String {
        self.config.read().unwrap().configured_model.clone()
    }

    /// Get a snapshot of the config.
    pub fn config_snapshot(&self) -> SelectorConfig {
        self.config.read().unwrap().clone()
    }
}

/// Render a status report as plain text (contracts/llm-selector-command.md).
pub fn render_status(report: &StatusReport) -> String {
    let mut out = String::new();
    if report.active {
        out.push_str("LLM Selector: enabled\n");
        out.push_str(&format!(
            "Active model: {} (dynamic allocation engaged)\n",
            report.configured_model
        ));
        out.push_str(&format!(
            "Candidate pool: {} chat-capable models (source: {:?})\n",
            report.pool_size, report.pool_source
        ));
        // Edge Case (spec.md line 126): single eligible model → no-op pass-through.
        if report.pool_is_single_model {
            out.push_str(
                "Note: only one eligible model in the pool — selector is a no-op pass-through.\n",
            );
        }
        out.push_str(&format!(
            "Diagnoser model: {}\n",
            if report.diagnoser_model.is_empty() {
                "(unset)"
            } else {
                &report.diagnoser_model
            }
        ));
        out.push_str(&format!(
            "Learning budget: {} ({} used this cycle)\n",
            report.learning_budget, report.budget_used
        ));
        if !report.entries.is_empty() {
            out.push_str("Allocations:\n");
            for e in &report.entries {
                let pin = if e.pinned {
                    " (pinned)"
                } else if e.implicit_pin {
                    " (implicit pin)"
                } else {
                    ""
                };
                out.push_str(&format!(
                    "  {:<14} -> {:<20}{}\n",
                    e.module, e.model_id, pin
                ));
            }
        }
    } else if !report.enabled {
        out.push_str("LLM Selector: disabled\n");
        out.push_str(&format!(
            "Active model: {} (concrete model; selector inactive)\n",
            report.configured_model
        ));
        out.push_str("Enable by selecting the `auto` model, then /llm-selector enable.\n");
    } else {
        // Enabled but not active (pool empty or model not auto).
        if report.pool_size == 0 {
            out.push_str("LLM Selector: unavailable\n");
            out.push_str("The active provider does not expose a live model catalog.\n");
            out.push_str("Dynamic selection requires a catalog-exposing provider (copilot/openrouter).\n");
        } else {
            out.push_str(&format!(
                "LLM Selector: enabled but inactive (model is '{}', not 'auto')\n",
                report.configured_model
            ));
        }
    }
    out.push_str("Run /llm-selector help for the full command list.\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_render_disabled() {
        let report = StatusReport {
            active: false,
            enabled: false,
            configured_model: "gpt-4o".to_string(),
            pool_size: 0,
            pool_source: CatalogSource::Empty,
            pool_is_single_model: false,
            diagnoser_model: String::new(),
            learning_budget: 8,
            budget_used: 0,
            entries: vec![],
        };
        let text = render_status(&report);
        assert!(text.contains("LLM Selector: disabled"));
        assert!(text.contains("gpt-4o"));
    }

    #[test]
    fn test_render_no_catalog() {
        let report = StatusReport {
            active: false,
            enabled: true,
            configured_model: "auto".to_string(),
            pool_size: 0,
            pool_source: CatalogSource::Empty,
            pool_is_single_model: false,
            diagnoser_model: String::new(),
            learning_budget: 8,
            budget_used: 0,
            entries: vec![],
        };
        let text = render_status(&report);
        assert!(text.contains("unavailable"));
    }

    #[test]
    fn test_render_active() {
        let report = StatusReport {
            active: true,
            enabled: true,
            configured_model: "auto".to_string(),
            pool_size: 12,
            pool_source: CatalogSource::Copilot,
            pool_is_single_model: false,
            diagnoser_model: "gpt-4.1".to_string(),
            learning_budget: 8,
            budget_used: 0,
            entries: vec![AllocationRow {
                module: "main_turn".to_string(),
                model_id: "gpt-4.1".to_string(),
                pinned: false,
                implicit_pin: false,
                reason: "cold-start".to_string(),
                estimated_performance: None,
                updated_at: None,
            }],
        };
        let text = render_status(&report);
        assert!(text.contains("enabled"));
        assert!(text.contains("12 chat-capable"));
        assert!(text.contains("main_turn"));
    }
}
