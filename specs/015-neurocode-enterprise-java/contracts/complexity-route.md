# Contract: ComplexityRoute + ComplexityTier Types

**Spec**: [spec.md](../spec.md) | **Data model**: [data-model.md](../data-model.md) Entities 1–2 | **Research**: [research.md](../research.md) §5

## ComplexityTier

```rust
/// The model tier a coding request is routed to (FR-001).
///
/// `#[non_exhaustive]`: future tiers (e.g. `MidTier`) may be added without
/// breaking the trait, the on-disk config, or the SQLite schema (Constitution
/// VII — additive).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
#[serde(rename_all = "lowercase")]
pub enum ComplexityTier {
    /// Suited to boilerplate, unit-test generation, simple refactoring.
    Economical,
    /// Suited to architectural changes, multi-file refactoring, concurrency
    /// debugging, legacy comprehension.
    Frontier,
    /// The defined default when the classifier cannot decide (FR-001
    /// acceptance 3). Resolves to `Economical` (cheaper; developer can
    /// escalate — spec edge case "router/developer disagree").
    AmbiguousDefault,
}
```

## ComplexityRoute

```rust
/// The result of classifying a coding request (spec Key Entity).
#[derive(Debug, Clone)]
pub struct ComplexityRoute {
    /// The resolved tier.
    pub tier: ComplexityTier,
    /// Human-readable classification reasoning (FR-002, SC-002).
    pub reasoning: String,
    /// True if the developer overrode the automatic classification (FR-002).
    pub overridden: bool,
    /// The developer-chosen tier when `overridden` is true.
    pub override_tier: Option<ComplexityTier>,
    /// The deterministic signals that fired (for diagnostics).
    pub signals: Vec<ClassificationSignal>,
}

/// A single deterministic classification signal (research.md §5).
#[derive(Debug, Clone)]
pub struct ClassificationSignal {
    pub kind: SignalKind,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalKind {
    /// A keyword match ("refactor", "test", "architecture", ...).
    Keyword,
    /// Scope fan-out (number of artifacts referenced).
    ScopeFanOut,
    /// Structural-graph locality (request touches a hub type).
    GraphHub,
}
```

## Classification rules (deterministic, non-async)

The `ComplexityClassifier` evaluates signals in priority order
(research.md §5). The rules are configurable via `config.yaml` but ship with
sensible defaults:

| Signal | Economical lean | Frontier lean |
|---|---|---|
| Keyword | "test", "getter", "setter", "boilerplate", "implement method" | "refactor", "architecture", "concurrency", "redesign", "migrate", "debug" |
| Scope fan-out | ≤ 2 artifacts referenced | > 4 artifacts referenced |
| Graph hub | target is a leaf method | target is a hub type (many in/out edges) |

- Conflicting signals → `AmbiguousDefault` (resolves to `Economical`).
- Developer override (FR-002) always wins over automatic classification.
- No LLM call (research.md §5 — avoids FR-017 hot-path cost).

## Override behavior (FR-002)

- The developer pins a tier via `/neurocode tier <economical|frontier>` for
  the next task, or per-session via `/neurocode tier pin <tier>`.
- `/neurocode tier auto` reverts to automatic classification.
- The override is recorded in `ComplexityRoute.overridden` + `override_tier`
  for transparency (SC-002).
