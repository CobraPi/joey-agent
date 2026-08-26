# Research: NeuroCode — Enterprise Java & Pega Rule System Coding Agent

**Branch**: `015-neurocode-enterprise-java` | **Date**: 2026-08-13 | **Spec**: [spec.md](./spec.md)

This document resolves every technical unknown and records each dependency
decision against the joey-agent constitution (`.specify/memory/constitution.md`
v1.1.0). The constitution (Principles I, VI, VIII) mandates: workspace-first
Rust crates, acyclic coupling behind narrow traits, and every new dependency
justified by a concrete, measurable benefit with weight recorded against
alternatives.

## §1 — Crate topology and DAG placement

**Decision**: One new crate, `joey-neurocode`, placed at the same workspace
layer as `joey-llm-selector` (depends on `joey-core` + `joey-tools`), consumed
upward by `joey-agent-core` via a narrow trait and by `joey-cli` for the
`/neurocode` command.

**Rationale**: The feature bundles three concerns — (a) graph-aware code
retrieval, (b) complexity-tier routing that composes with the 011 selector,
and (c) a build/verify feedback loop. All three share the same structural
index and the same config namespace, so splitting them into multiple crates
would create circular dependencies (the feedback loop calls the router; the
router reads the index). A single crate with internal modules keeps the DAG
acyclic and matches how the workspace already factors cross-cutting concerns
(`joey-mcp`, `joey-cron`, `joey-llm-selector` are each one crate behind a
small surface).

**Workspace DAG (verified acyclic)**:
```
joey-core
  └─ joey-tools          (Tool trait, registry)
  └─ joey-neurocode      (NEW: index, router, graph, feedback loop)
       └─ consumes joey-llm-selector's ModelAllocator trait (already on this layer)
  └─ joey-providers
       └─ joey-llm-selector
  └─ joey-agent-core     (consumes joey-neurocode's NeuroCodeEngine trait)
  └─ joey-orchestration  (consumes joey-neurocode config via parent_config_tree — no upward dep)
  └─ joey-cli            (wires /neurocode + passes engine to agent-core/orchestration)
```
`joey-neurocode` depends only downward on `joey-core`, `joey-tools`, and the
`ModelAllocator` trait (from `joey-llm-selector`, which is itself at this
layer). No new edge is introduced between existing crates. A change inside the
NeuroCode engine never forces an edit to `joey-agent-core` beyond the trait
(Constitution VI).

**Alternatives considered**:
- *Three crates* (`joey-codegraph`, `joey-tier-router`, `joey-verify-loop`):
  rejected — the three concerns share the index and config; splitting creates
  inter-crate cycles or forces a fourth "shared types" crate, adding compile
  surface for no decoupling benefit.
- *Add logic into `joey-agent-core`*: rejected — violates Constitution VI
  (threads feature logic through shared core paths) and Constitution I
  (not a dedicated crate).

## §2 — Structural store: SQLite + FTS5 (NOT a vector DB)

**Decision**: Use the workspace's **existing bundled SQLite** (rusqlite
`bundled`, `SCHEMA_VERSION = 22`) with **FTS5** for the structural knowledge
graph. A per-project NeuroCode database lives at
`~/.joey/neurocode/<project-hash>/graph.db`.

**Rationale**: The source plan proposed Qdrant (a separate vector database
server). This is rejected for this workspace:

1. **Constitution VIII (lean deps / justified weight).** Qdrant is a
   *separate server process* (or a heavy embedded Rust crate). It adds a
   runtime dependency, a second storage engine, deployment complexity, and
   binary size. The workspace already bundles SQLite with FTS5 (verified:
   `joey-core::state` probes and uses FTS5, `sqlite_supports_fts5()` at
   state.rs:538). Reusing it is zero new dependency.

2. **FTS5 with BM25 ranking is a proven code-retrieval baseline.** Symbol-
   aware tokenization (splitting on camelCase/snake_case boundaries) plus
   FTS5 BM25 ranking over the structural metadata fields (class name,
   package, annotations, dependency names) achieves effective retrieval for
   the graph-expansion step (FR-007) without embedding vectors. The graph
   edges (implements/injects/references) are exact-match lookups, not
   similarity queries — a vector store adds no value for them.

3. **The graph edges are the point, not embedding similarity.** FR-007's
   graph expansion ("pull in the interface, the repository, the DTO") is a
   deterministic traversal of typed edges, not a nearest-neighbor search.
   SQLite tables with indexed foreign keys do this optimally.

4. **Constitution VII (non-regression).** SQLite is already a pinned,
   versioned on-disk format the workspace maintains (`SCHEMA_VERSION`).
   Adding Qdrant introduces an unpinned external format with its own
   migration story.

**Embedding model (deferred / optional, out of scope for v1)**: The source
plan names `voyage-code-2` / `bge-m3`. Semantic-similarity retrieval is a
*future enhancement* layered behind the same trait (see §6); v1 ships with
FTS5 + graph traversal, which satisfies every FR in the spec. Adding an
embedding model later requires a provider call or a local model dependency —
deferred until a concrete retrieval-quality gap is measured.

**Alternatives considered**:
- *Qdrant (embedded `qdrant-lib` or server)*: rejected — server adds
  deployment complexity and a runtime dependency the constitution discourages;
  embedded crate is heavy. No benefit over FTS5+graph for the typed-edge
  traversal that is the core of FR-007.
- *LanceDB / sqlite-vec*: deferred — interesting future option for optional
  semantic retrieval, but v1's typed-edge graph doesn't need vectors.
- *In-memory only (no persistence)*: rejected — FR-016 (cold/un-indexed
  detection) and the feedback loop's learned patterns (FR-011) require
  persistence across sessions.

## §3 — Code parsing: tree-sitter-java

**Decision**: Add `tree-sitter = "0.26"` + `tree-sitter-java = "0.23"` as the
**one** new external dependency for this feature. These are the official Rust
bindings for the Tree-sitter parsing framework and the maintained Java
grammar.

**Rationale**: FR-006 mandates deterministic, syntax-aware parsing of Java
(type/method/field boundaries, annotations, imports, injection points) that
does NOT rely on the LLM to guess structure. Tree-sitter is the standard
choice: it is incremental, deterministic, produces a concrete syntax tree,
and the Java grammar is mature and maintained. Regex-based parsing of Java is
brittle (generics, nested classes, records, annotations with arguments) and
would fail the "deterministic, no LLM guessing" requirement.

**Weight justification (Constitution VIII)**:
- `tree-sitter` core: ~120KB compiled (a C library with Rust bindings). No
  transitive runtime deps (it bundles its C source via `cc`).
- `tree-sitter-java`: ~150KB (the grammar's C source). No transitive deps.
- Compile-time cost: moderate (C compilation via `cc`); cached after first
  build. Acceptable for a parsing capability that is the core of FR-006.
- The alternative (a hand-written Java parser in Rust) would be thousands of
  lines and still be less correct than the maintained grammar. The "no new
  dep" alternative (regex heuristics) fails FR-006's determinism requirement.

This is the **only** new external dependency the feature introduces. It is
recorded here per Constitution VIII and referenced in the plan's Primary
Dependencies.

**Alternatives considered**:
- *Regex heuristics*: rejected — brittle on generics/annotations/nested
  classes; fails FR-006 determinism.
- *Hand-written Rust Java parser*: rejected — enormous effort, less correct,
  violates VIII's "simplest algorithm that meets the requirement" (tree-sitter
  already exists and is correct).
- *java-parser via an LLM*: rejected — FR-006 explicitly forbids relying on
  the LLM to guess structure.

## §4 — Pega rule-system integration (version-adaptive, per Clarification Q4)

**Decision**: Pega rule-type awareness is implemented as (a) a tree-sitter
query layer over the indexed Java that recognizes Pega rule class patterns
(`Rule-Obj-*`, `Data-*`, `Work-*` class hierarchies, directed-inheritance
declarations, rule-reference patterns) and (b) an ingestion path for Pega
rule-type metadata sourced from the installed Pega Platform version's
documentation/metadata (detected from the project's build artifacts — e.g.
`prweb`/`prpc` version markers, Gradle/Maven Pega BOM entries, or a
`pegaversion` file).

**Version detection mechanism**: The agent probes the project for a Pega
version marker in priority order:
1. A `pega.version` key in the project's NeuroCode config (explicit override).
2. Pega-version-bearing build entries (Gradle dependency on
   `com.pega:prpub`/`prweb` BOM, Maven `pega-platform` dependency version).
3. In-source markers (package `com.pega.*`, class names matching
   `PR*` patterns, `Rule-*` class-pattern files).

If no version is detected, the agent operates in generic-Java mode and
informs the developer that Pega-specific rule awareness is inactive (graceful
fallback, FR-015). The supported version floor is the latest two major
Infinity releases (e.g., Infinity '24 and '23 at time of writing); older
versions fall back to generic Java + observed patterns without version-matched
metadata. This is documented as a plan-level decision per Clarification Q4.

**Live Pega integration is out of scope** (Clarification Q1) — no connection
to a running Pega instance, no DX API calls at generation time. The rule-type
metadata is ingested as static domain knowledge (FR-013), not queried live.

**Alternatives considered**:
- *Hardcode to one Pega version*: rejected (Clarification Q4 resolved as
  version-adaptive).
- *Live Pega validation*: rejected (Clarification Q1, Option B explicitly
  excludes live validation).
- *Ignore Pega entirely*: rejected (the spec's FR-009 and User Story 3 make
  Pega a P1 requirement).

## §5 — Tier routing: composing with spec 011's ModelAllocator

**Decision**: NeuroCode does NOT implement a parallel model router. It
implements a `ComplexityClassifier` that maps a coding request to a
`ComplexityTier` (Economical | Frontier | AmbiguousDefault), and a
`TierModelResolver` that maps each tier to a concrete model id from config.
The composition with spec 011 (Clarification Q2) works as follows:

- **Both NeuroCode and 011 enabled**: NeuroCode classifies complexity →
  produces a `ComplexityTier` → this tier is passed as a *constraint hint* to
  011's `ModelAllocator::resolve()` via a new optional field on the
  `ModuleId` context (additive, non-breaking). 011 still owns the allocation
  map, learning loop, and diagnostics. NeuroCode does not duplicate any of
  that machinery (FR-018).
- **NeuroCode enabled, 011 not enabled**: NeuroCode's `TierModelResolver`
  reads the configured model for the chosen tier directly from
  `config.yaml` (`neurocode.tier.economical.model`,
  `neurocode.tier.frontier.model`).
- **NeuroCode disabled**: byte-identical to today (FR-020, SC-008).

**ComplexityClassifier implementation**: a lightweight, deterministic rule-
based classifier (NOT an LLM call) operating on request heuristics — keyword
signals ("refactor", "architecture", "concurrency", "redesign" → Frontier;
"test", "getter", "boilerplate", "implement method" → Economical), scope
signals (number of files/artifacts referenced), and the structural-graph
fan-out (a request touching > N related artifacts leans Frontier). This is
non-async, O(1) on the hot path (FR-017), and avoids spending a model call on
classification. The classifier is configurable and the developer can always
override the tier (FR-002).

**Why not a small-LLM classifier**: The source plan suggests "a lightweight
classifier (or a small LLM prompt)" for routing. Using an LLM call for
classification on every request violates FR-017 (no blocking on the hot path)
and adds latency + cost before the actual task even starts. A deterministic
rule-based classifier is faster, cheaper, debuggable, and sufficient — the
developer override (FR-002) covers the edge cases the rules get wrong.

**Alternatives considered**:
- *LLM-based router*: rejected (FR-017 hot-path cost; adds a model call before
  every task).
- *NeuroCode implements its own full allocator*: rejected (duplicates 011,
  violates FR-018 and Constitution VI).

## §6 — Trait surface (the narrow boundary joey-agent-core consumes)

**Decision**: `joey-agent-core` consumes NeuroCode through a single trait,
`NeuroCodeEngine`, with three methods:

```rust
pub trait NeuroCodeEngine: Send + Sync {
    /// Classify a coding request's complexity and resolve the tier.
    /// Non-async, O(1) — hot path (FR-017).
    fn classify(&self, request: &CodingRequest) -> ComplexityRoute;

    /// Assemble the dependency-aware context graph for a request, formatted
    /// for the resolved tier's context budget (FR-007, FR-008).
    /// Reads from the structural index (no network).
    fn assemble_context(&self, request: &CodingRequest, tier: ComplexityTier)
        -> AssembledContext;

    /// Whether NeuroCode is enabled for the current session.
    fn is_active(&self) -> bool;
}
```

The structural index, ingestion pipeline, graph store, Pega metadata, domain
knowledge, learned patterns, and feedback loop are all private to
`joey-neurocode`. The agent turn loop calls `classify` and `assemble_context`
before dispatching to the model; the feedback loop runs asynchronously via a
detached `tokio::spawn` (like 011's diagnoser) and is invoked after a
generation completes.

**Tools surface**: NeuroCode also exposes its capabilities as tools the agent
can call directly (registered in `joey_tools::builtins::register_all` per the
workspace convention):
- `neurocode_index` — trigger/re-index a project (async, off hot path).
- `neurocode_query` — query the structural graph (what implements X, what
  injects Y).
- `neurocode_status` — report index state, tier config, Pega version.
- `neurocode_ingest` — ingest a domain-knowledge source (docs/entity/
  postmortem).

This gives the model agentic access to the index (it can decide to re-index
or query the graph mid-task) while the engine trait gives the turn loop the
automatic pre-generation context assembly.

**Alternatives considered**:
- *Expose the full engine to agent-core*: rejected (Constitution VI — narrow
  trait only).
- *Tools-only (no trait)*: rejected — FR-007/FR-008 require *automatic*
  context assembly before every generation, not just when the model decides
  to call a tool.

## §7 — Build/verify feedback loop

**Decision**: The feedback loop runs as a detached `tokio::spawn` task after
a generation is written to disk. It executes the configured verification
steps (static-analysis, compile, test) via the existing `terminal` tool's
shell-execution path (reusing `joey-tools`'s subprocess machinery — no second
process runner). On failure, it feeds the error output back by injecting a
structured result that the next agent turn consumes; on success, it records
the pattern. It never blocks the interactive turn (FR-017, FR-010's async
requirement).

**Verification step configuration** (per project, via `config.yaml`):
```yaml
neurocode:
  verify:
    steps:
      - name: checkstyle
        command: "mvn checkstyle:check"
        parse: checkstyle_xml
        timeout_sec: 120
      - name: compile
        command: "mvn compile -q"
        timeout_sec: 300
      - name: test
        command: "mvn test -Dtest={target_class}"
        timeout_sec: 300
    max_fix_iterations: 3
```

The feedback loop reuses the existing `terminal` subprocess execution (no new
dependency). Error parsing (Checkstyle XML, compiler output) is a small,
self-contained module. The learned patterns and anti-patterns are persisted
in the per-project SQLite store via `atomic_json_write` metadata blobs.

**Alternatives considered**:
- *Synchronous loop in the turn*: rejected (FR-017 — blocks the developer).
- *Separate CI/CD integration (GitHub Actions etc.)*: rejected (out of scope;
  the spec describes an agentic feedback loop, not a CI integration).

## §8 — On-disk layout and format stability (Constitution VII)

**Decision**: NeuroCode state lives under `~/.joey/neurocode/` (honouring
`JOEY_HOME`, resolved via `process_joey_home()`):

```
~/.joey/neurocode/
├── config.json                    # resolved NeuroCode config (cache)
├── projects/
│   └── <project-hash>/            # one per indexed project (hash of repo root path)
│       ├── graph.db               # SQLite: structural index + FTS5 + learned patterns
│       ├── meta.json              # project metadata (pega version, framework versions, last-indexed)
│       └── domain/                # ingested domain knowledge
│           ├── frameworks/
│           ├── entities/
│           └── postmortems/
```

The `graph.db` SQLite schema is a **new, versioned** on-disk format
(`neurocode_schema_version: 1`). It is additive to the workspace — the
existing session-store schema (`SCHEMA_VERSION = 22` in `joey-core::state`) is
untouched. NeuroCode uses its own SQLite connection (a separate file), never
the session DB. Any future breaking change to the NeuroCode schema requires a
documented migration, satisfying Constitution VII.

**Why SQLite, not JSON files**: The structural index is queried frequently
(FTS5 search + graph-edge traversal on every `assemble_context` call) and
updated on ingestion. JSON files would require loading the entire graph into
memory on every query — unacceptable for a large enterprise codebase. SQLite
with indexed tables is the lean choice that the workspace already depends on.

**Alternatives considered**:
- *Reuse the session DB*: rejected — different lifecycle (project-scoped, not
  session-scoped), different access pattern, would pollute the session schema.
- *JSON files*: rejected (performance on large indexes — Constitution VIII).

## §9 — Regression safety (Constitution VII)

The feature is strictly additive and default-off (`neurocode.enabled = false`).
When disabled:
- `NeuroCodeEngine::is_active()` returns false.
- `classify` and `assemble_context` are never called by the turn loop (the
  engine is wrapped in `Option<Arc<dyn NeuroCodeEngine>>` — `None` when
  disabled, identical to today's code path).
- No new messages are injected into conversation history.
- The system prompt is byte-stable (NeuroCode does not touch
  `build_system_prompt`).
- The four NeuroCode tools' `check()` methods return false when disabled, so
  they are not offered to the model.

Regression coverage (FR-020) asserts all of the above in tests.

## §10 — Summary of new dependencies (Constitution VIII audit)

| Dependency | Version | Why | Weight | Alternative rejected |
|---|---|---|---|---|
| `tree-sitter` | 0.26 | Deterministic Java parsing (FR-006) | ~120KB compiled, C via `cc`, no runtime deps | Regex (brittle); hand-written (huge, less correct) |
| `tree-sitter-java` | 0.23 | Java grammar for tree-sitter | ~150KB, no transitive deps | (same as above) |

**Total new external dependencies: 2** (both are the official Tree-sitter
Rust bindings + grammar). No vector database, no embedding model, no separate
server, no second storage engine. All other capabilities reuse existing
workspace crates (`joey-core` SQLite/atomic-write/config, `joey-tools`
Tool/subprocess, `joey-llm-selector` ModelAllocator trait).
