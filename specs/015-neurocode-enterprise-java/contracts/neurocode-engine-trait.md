# Contract: NeuroCodeEngine Trait

**Spec**: [spec.md](../spec.md) | **Plan**: [plan.md](../plan.md) | **Research**: [research.md](../research.md) §6

The narrow trait `joey-agent-core` consumes. This is the ONLY public surface
the turn loop depends on (Constitution VI). Everything else in `joey-neurocode`
is private to the engine.

## Trait definition

```rust
use crate::context::AssembledContext;
use crate::classifier::{ComplexityRoute, ComplexityTier};

/// The input to classification and context assembly.
/// Constructed by the turn-loop intercept from the user's request.
pub struct CodingRequest {
    pub text: String,
    pub active_file: Option<String>,
    pub active_symbols: Vec<String>,
    pub project_root: std::path::PathBuf,
    pub token_budget_hint: u64,
}

/// Narrow interface the agent turn loop consumes to classify a coding
/// request's complexity and assemble a dependency-aware context graph.
///
/// Hot-path methods (`classify`, `assemble_context`) are non-async and
/// run off cached/indexed state — no network, no blocking (FR-017).
pub trait NeuroCodeEngine: Send + Sync {
    /// Classify a coding request's complexity and resolve the tier (FR-001).
    /// Non-async, O(1) — deterministic rule evaluation (research.md §5).
    fn classify(&self, request: &CodingRequest) -> ComplexityRoute;

    /// Assemble the dependency-aware context graph for a request, formatted
    /// for the resolved tier's context budget (FR-007, FR-008).
    /// Reads from the local structural index (no network).
    fn assemble_context(
        &self,
        request: &CodingRequest,
        tier: ComplexityTier,
    ) -> AssembledContext;

    /// Whether NeuroCode is enabled for the current session (FR-003).
    /// When false, the turn loop MUST NOT call classify/assemble_context
    /// and behavior is byte-identical to today (FR-020, SC-008).
    fn is_active(&self) -> bool;
}
```

## Consumption pattern in joey-agent-core

The engine is injected as `Option<Arc<dyn NeuroCodeEngine>>`:

- **`None`** (feature disabled, `neurocode.enabled = false`, or not compiled
  in): the turn loop takes today's exact code path. No classification, no
  context assembly, no messages injected. Byte-identical (FR-020).
- **`Some(engine)` where `engine.is_active() == false`**: same as `None` —
  the intercept is a no-op. This covers the case where the engine is wired
  but the feature is disabled at runtime for the current profile.
- **`Some(engine)` where `engine.is_active() == true`**: before model
  dispatch, the turn loop calls `engine.classify(&request)` to get the
  `ComplexityRoute`, then `engine.assemble_context(&request, route.tier)` to
  get the `AssembledContext`. The context is prepended to the user's request
  as structured context; the tier feeds the model-id resolution (via 011
  composition or direct config lookup — see tier-routing-composition.md).

## Invariants

- `classify` and `assemble_context` are **non-async** and **never block**
  (FR-017). They read from the local SQLite index and in-memory classifier
  state only.
- Calling `classify` or `assemble_context` when `is_active() == false` is a
  no-op contract violation (the turn loop must not do it); the methods return
  safe empty defaults defensively.
- The trait is `#[non_exhaustive]`-friendly: adding a method with a default
  impl is a non-breaking change (Constitution VII). Removing or changing a
  method signature is a breaking change requiring a MAJOR bump.

## Registration (joey-cli wiring)

`joey-cli` constructs the engine (or `None`) at startup from `config.yaml`
and injects it into `AgentConfig`, which `joey-agent-core` reads. The engine
is `None` unless `neurocode.enabled = true` AND the target project has been
indexed (or is eligible for cold-start — FR-016).
