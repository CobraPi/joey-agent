# Contract: Agent Registry

**Feature**: 003-omo-orchestration

## Public API (joey-omo crate)

### AgentRegistry

```rust
/// The registry of all 11 built-in OMO agents, resolved against available providers.
pub struct AgentRegistry {
    agents: Vec<OmoAgent>,
    categories: Vec<CategoryConfig>,
}

impl AgentRegistry {
    /// Build the registry, resolving each agent's model via fallback chains.
    /// Agents whose models are all unavailable are marked skipped (not dropped).
    pub fn build(available_models: &AvailableModelSet, config: &Config) -> Self;

    /// All 11 agents (including skipped ones).
    pub fn all(&self) -> &[OmoAgent];

    /// Only primary agents available for Tab selection (model resolved).
    pub fn available_primary(&self) -> Vec<&OmoAgent>;

    /// Canonical Tab order: Default → Sisyphus → Hephaestus → Prometheus → Atlas.
    /// "Default" is the existing joey-agent agent (not an OmoAgent) — it is
    /// prepended by the caller (joey-cli/joey-tui) which owns the default.
    pub fn tab_order(&self) -> Vec<&OmoAgent>;

    /// Look up an agent by canonical name.
    pub fn get(&self, name: &str) -> Option<&OmoAgent>;

    /// All 11 categories.
    pub fn categories(&self) -> &[CategoryConfig];
}
```

### OmoAgent

```rust
pub struct OmoAgent {
    pub name: String,           // "sisyphus", "oracle", etc.
    pub display_name: String,   // "Sisyphus", "Oracle", etc.
    pub mode: AgentMode,        // Primary or Subagent
    pub color: String,          // "#10B981"
    pub description: String,
    pub model_requirement: ModelRequirement,
    pub resolved_model: Option<String>,  // None = skipped (unavailable)
    pub resolved_variant: Option<String>,
    pub temperature: f64,
    pub max_tokens: Option<u32>,
    pub tool_permissions: ToolPermissions,
}

impl OmoAgent {
    /// Build the system prompt for this agent, selecting the model-family variant.
    pub fn system_prompt(&self, model: &str) -> String;

    /// Whether this agent is available (model resolved).
    pub fn is_available(&self) -> bool;
}
```

## Canonical Agent List (11)

| Name | Display Name | Mode | Color | Notes |
|------|-------------|------|-------|-------|
| sisyphus | Sisyphus | Primary | (config) | Default OMO orchestrator |
| hephaestus | Hephaestus | Primary | #D97706 | GPT-only; requires OpenAI-class provider |
| prometheus | Prometheus | Primary | (config) | Read-only planner |
| atlas | Atlas | Primary | #10B981 | Conductor; never writes code |
| oracle | Oracle | Subagent | (config) | Architecture consultant |
| librarian | Librarian | Subagent | (config) | Docs/OSS search |
| explore | Explore | Subagent | (config) | Fast codebase grep |
| multimodal-looker | Multimodal-Looker | Subagent | (config) | Vision/screenshot analysis |
| metis | Metis | Subagent | (config) | Gap analyzer |
| momus | Momus | Subagent | (config) | Plan reviewer |
| sisyphus-junior | Sisyphus-Junior | Subagent | #20B2AA | Task executor; blocks `task` tool |

## Behavioral Contracts

- **BC-001**: `build()` MUST produce exactly 11 agents with the canonical
  names. No more, no less.
- **BC-002**: `available_primary()` MUST return only agents where
  `resolved_model` is `Some` and `mode == Primary`.
- **BC-003**: Hephaestus MUST be skipped if no OpenAI-class provider is
  connected (`requiresProvider` constraint).
- **BC-004**: `system_prompt(model)` MUST select the model-family-specific
  variant. If no specific variant exists, the default variant is used.
- **BC-005**: Sisyphus-Junior's `tool_permissions` MUST deny `task` and allow
  `call_omo_agent`.
