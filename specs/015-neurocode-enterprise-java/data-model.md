# Data Model: NeuroCode — Enterprise Java & Pega Rule System Coding Agent

**Branch**: `015-neurocode-enterprise-java` | **Date**: 2026-08-13 | **Spec**: [spec.md](./spec.md) | **Plan**: [plan.md](./plan.md)

This document defines the entities, fields, relationships, and on-disk
representation for NeuroCode. Types are described at the design level (Rust
type names + field semantics); the SQLite schema is specified in
[contracts/graph-store-schema.md](./contracts/graph-store-schema.md).

## Entity 1 — ComplexityTier (enum)

The model tier a coding request is routed to.

```
ComplexityTier ::= Economical | Frontier | AmbiguousDefault
                  (#[non_exhaustive] — future tiers, e.g. MidTier, may be added
                   without breaking the trait or the on-disk config)
```

- **Economical**: suited to boilerplate, unit-test generation, simple refactoring (FR-001).
- **Frontier**: suited to architectural changes, multi-file refactoring, concurrency debugging, legacy comprehension (FR-001).
- **AmbiguousDefault**: the defined default when the classifier cannot decide (FR-001 acceptance 3). Defaults to `Economical` (cheaper, developer can escalate — edge case "router/developer disagree").

**Validation**: the tier must map to a model id resolvable either via config (`neurocode.tier.<tier>.model`) or via the 011 allocator (FR-018). If unresolvable, the system falls back to the agent's configured default model and records the fallback.

**Persistence**: stored as a string tag in `ComplexityRoute` and in `config.yaml` tier definitions. The `#[non_exhaustive]` attribute means adding a tier is a non-breaking change (additive — Constitution VII).

## Entity 2 — ComplexityRoute (struct)

The result of classifying a coding request — the routing artifact the developer inspects (spec Key Entity).

| Field | Type | Semantics |
|---|---|---|
| `tier` | `ComplexityTier` | The resolved tier. |
| `reasoning` | `String` | Human-readable classification reasoning (FR-002, SC-002 transparency). E.g. "keyword 'refactor' + 4 artifacts in scope → Frontier". |
| `overridden` | `bool` | True if the developer overrode the automatic classification (FR-002). |
| `override_tier` | `Option<ComplexityTier>` | The developer-chosen tier when `overridden` is true. |
| `signals` | `Vec<ClassificationSignal>` | The deterministic signals that fired (keyword, scope-fan-out, etc.) — for diagnostics. |

**Lifecycle**: created per-request by `ComplexityClassifier::classify()`. Not persisted as a standalone entity (it is transient), but `signals` and the resolved tier are logged for `/neurocode` diagnostics and feed the feedback loop.

## Entity 3 — CodeArtifactNode (struct, persisted in graph.db)

A unit of parsed code stored in the structural knowledge graph (spec Key Entity). One row per parsed Java type/method/field or Pega rule.

| Field | Type | Semantics |
|---|---|---|
| `id` | `ArtifactId` (u64, SQLite rowid) | Internal primary key. |
| `kind` | `ArtifactKind` | `Class \| Interface \| Enum \| Method \| Field \| PegaRule`. |
| `fqcn` | `String` | Fully-qualified canonical name (e.g. `com.enterprise.auth.service.UserServiceImpl`). |
| `enclosing_type` | `Option<String>` | Enclosing type name for methods/fields (FR-005). |
| `package` | `String` | Package/namespace (FR-005). |
| `implemented_interfaces` | `Vec<String>` | Interfaces this type implements (FR-005). |
| `annotations` | `Vec<String>` | Framework annotations/declarations (FR-005): e.g. `["Service", "Transactional"]`. |
| `declared_dependencies` | `Vec<String>` | Injected/declared dependencies (FR-005): e.g. `["UserRepository", "AuditLogger"]`. |
| `source_path` | `String` | Relative path to the source file. |
| `source_span` | `Option<(u32,u32)>` | Byte range in the source file (tree-sitter span). |
| `pega_metadata` | `Option<PegaMetadata>` | Present only for Pega artifacts (FR-005, FR-009). See Entity 8. |
| `framework_version` | `Option<String>` | Detected framework version (e.g. `Spring Boot 3.2`) for domain-aware generation. |

**Identity/uniqueness**: `(fqcn, kind, source_path)` is unique. Re-ingestion of the same source updates the existing node (upsert), preserving learned edges/patterns attached to it.

**Lifecycle**: `Created` (on ingestion) → `Updated` (on re-ingestion after source change) → `Stale` (source deleted; edges retained for reference but marked) → `Deleted` (explicit purge). Stale nodes are surfaced by `/neurocode status` and excluded from context assembly unless explicitly requested.

## Entity 4 — DependencyGraphEdge (struct, persisted in graph.db)

A typed relationship between two CodeArtifactNodes (spec Key Entity). Drives graph expansion during context assembly (FR-007).

| Field | Type | Semantics |
|---|---|---|
| `from_id` | `ArtifactId` | Source node. |
| `to_id` | `ArtifactId` | Target node. |
| `edge_kind` | `EdgeKind` | The typed relationship (see below). |

```
EdgeKind ::= Implements           // from implements to (interface)
           | IsImplementedBy      // inverse of Implements
           | Injects              // from injects/depends-on to (the declared type)
           | ExchangesType        // from exchanges a DTO/type with to
           | ReferencesRule       // Pega: from references/delegates to to (rule-to-rule)
           | InheritsRule         // Pega: directed inheritance (rule class hierarchy)
```

**Identity/uniqueness**: `(from_id, to_id, edge_kind)` is unique. Re-ingestion is idempotent.

**Validation**: both endpoints must reference existing nodes (FK constraint). A broken reference (target node deleted) is surfaced as a finding (spec edge case "cyclic/broken graph"), not silently dropped.

## Entity 5 — ContextGraph (transient struct)

The per-task assembly of CodeArtifactNodes (directly retrieved + graph-expanded) formatted for a specific tier's budget (spec Key Entity). What the model actually sees.

| Field | Type | Semantics |
|---|---|---|
| `primary_nodes` | `Vec<ArtifactId>` | The directly-retrieved artifacts (the request target). |
| `expanded_nodes` | `Vec<(ArtifactId, ExpansionReason)>` | Graph-expanded artifacts, each tagged with why it was pulled in (FR-007). `ExpansionReason ::= ImplementsInterface \| InjectedByTarget \| ExchangesTypeWithTarget \| ReferencesRule`. |
| `formatted_context` | `String` | The final text formatted for the tier (method+interface slice for Economical; full graph for Frontier — FR-008). |
| `tier` | `ComplexityTier` | The tier this graph was formatted for. |
| `token_estimate` | `usize` | Estimated tokens in `formatted_context` (must be within the tier's budget). |

**Lifecycle**: created per-request by `ContextAssembler::assemble()`, consumed by the turn loop, not persisted (transient). The `expanded_nodes` + reasons are logged for the developer's "inspect what was sent" capability (User Story 2 acceptance 5).

## Entity 6 — LearnedPattern (struct, persisted in graph.db)

A recorded successful generation (spec Key Entity). Stored in the `patterns` table.

| Field | Type | Semantics |
|---|---|---|
| `id` | `PatternId` (u64) | Primary key. |
| `prompt_signature` | `String` | A normalized signature of the generation prompt (for matching similar future tasks). |
| `generation_summary` | `String` | Summary of the generated code (not full code — for retrieval matching). |
| `verify_result` | `VerifyResult` | The verification outcome that passed (which steps ran, duration). |
| `artifact_ids` | `Vec<ArtifactId>` | The codebase artifacts involved (for "similar task" matching by graph locality). |
| `tier` | `ComplexityTier` | Which tier produced this pattern. |
| `created_at` | `chrono::DateTime<chrono::Utc>` | When recorded. |

## Entity 7 — LearnedAntiPattern (struct, persisted in graph.db)

A recorded failure with its fix (spec Key Entity). Stored in the `anti_patterns` table. Surfaced as a warning when the same area is edited again (FR-011).

| Field | Type | Semantics |
|---|---|---|
| `id` | `AntiPatternId` (u64) | Primary key. |
| `error_signature` | `String` | Normalized error signature (e.g. `BeanCreationException:UserServiceImpl`). |
| `error_output` | `String` | The relevant failure output (truncated). |
| `resolution` | `String` | The fix that resolved the failure. |
| `artifact_ids` | `Vec<ArtifactId>` | The codebase area this anti-pattern is attached to (FR-011 "same area"). |
| `created_at` | `chrono::DateTime<chrono::Utc>` | When recorded. |
| `hit_count` | `u32` | How many times this anti-pattern has been surfaced (for `/neurocode` diagnostics). |

**Lifecycle**: `Created` (on verify failure + successful fix) → `Surfaced` (incremented `hit_count` when the same area is edited and the warning fires) → `Resolved` (developer dismisses the anti-pattern via `/neurocode` once the underlying issue is permanently addressed).

## Entity 8 — PegaMetadata (embedded in CodeArtifactNode)

Structural metadata specific to Pega Platform artifacts (spec Key Entity "Pega Rule Artifact"). Present only when `kind == PegaRule` or the Java type matches Pega rule patterns.

| Field | Type | Semantics |
|---|---|---|
| `rule_class_family` | `PegaRuleFamily` | `RuleObj \| Data \| Work \| Other` (mapped from `Rule-Obj-*`/`Data-*`/`Work-*` patterns). |
| `rule_name` | `String` | The Pega rule instance name. |
| `references_rules` | `Vec<String>` | Other rules this rule references/delegates to (drives `ReferencesRule` edges). |
| `inherits_from` | `Option<String>` | Directed-inheritance parent rule class (drives `InheritsRule` edges). |
| `pega_version` | `String` | The detected Pega version this artifact's metadata is grounded in (Clarification Q4). |

## Entity 9 — DomainKnowledgeSource (struct, persisted in graph.db + files)

An ingested body of knowledge (spec Key Entity). The retrieval graph draws on it during generation.

| Field | Type | Semantics |
|---|---|---|
| `id` | `KnowledgeId` (u64) | Primary key. |
| `category` | `KnowledgeCategory` | `FrameworkDocs \| EntityCatalog \| Postmortem` (FR-013). |
| `source_path` | `String` | Path to the ingested file under `~/.joey/neurocode/projects/<hash>/domain/`. |
| `version_tag` | `Option<String>` | For framework docs: the version they apply to (FR-013a). For entities: the schema version. |
| `provenance` | `String` | Human-readable source identifier (which doc, which entity set, which postmortem — FR-014). |
| `ingested_at` | `chrono::DateTime<chrono::Utc>` | When ingested. |
| `fts_indexed` | `bool` | Whether the content is in the FTS5 index (for retrieval). |

**Conflict resolution** (spec edge case "conflicting sources"): when two sources with the same `category` + overlapping `version_tag` are ingested, the most recently-ingested one wins for retrieval, the conflict is flagged in `/neurocode status`, and the developer can resolve via `/neurocode domain remove <id>`.

## Entity 10 — CodingRequest (input struct, transient)

The input to `NeuroCodeEngine::classify` and `assemble_context`. Constructed by the turn-loop intercept from the user's request + current workspace context.

| Field | Type | Semantics |
|---|---|---|
| `text` | `String` | The developer's request text. |
| `active_file` | `Option<String>` | The file currently open/active (for scope). |
| `active_symbols` | `Vec<String>` | Symbols in the active selection/cursor position (for graph expansion seeding). |
| `project_root` | `PathBuf` | The target project root (determines which `graph.db` to use). |
| `token_budget_hint` | `u64` | The available context budget for the resolved tier (from the allocator). |

## Entity relationships (ER summary)

```
CodeArtifactNode 1───* DependencyGraphEdge *───1 CodeArtifactNode
      (from_id)                              (to_id)

CodeArtifactNode 1───* LearnedPattern        (artifact_ids)
CodeArtifactNode 1───* LearnedAntiPattern    (artifact_ids)

CodeArtifactNode 1───0..1 PegaMetadata       (embedded, Pega artifacts only)

DomainKnowledgeSource 0───* (FTS5 index entries)   (retrieval, not a hard FK)

CodingRequest  ──classify()──▶  ComplexityRoute
CodingRequest  ──assemble_context()──▶  ContextGraph  (references CodeArtifactNodes)
```

## On-disk representation

- **CodeArtifactNode, DependencyGraphEdge, LearnedPattern, LearnedAntiPattern, DomainKnowledgeSource**: SQLite tables in `graph.db` (schema in [contracts/graph-store-schema.md](./contracts/graph-store-schema.md)). FTS5 virtual table indexes `fqcn`, `enclosing_type`, `annotations`, `declared_dependencies`, and domain-knowledge content for BM25 retrieval.
- **PegaMetadata**: embedded JSON column on the `code_artifacts` table (deserialized via serde).
- **Transient entities** (ComplexityRoute, ContextGraph, CodingRequest): in-memory only, never persisted.
- **Config**: `neurocode.*` dotted keys in `config.yaml` (see [contracts/neurocode-command.md](./contracts/neurocode-command.md) for the full key list).
