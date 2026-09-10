# joey-neurocode — enterprise code-analysis engine (Java/Pega)

`joey-neurocode` (spec 015, originally "Enterprise Java & Pega Rule System") is
the multi-language coding-agent engine behind Joey's code-analysis features. It
classifies incoming coding requests by complexity and routes them between an
economical and a frontier model tier, maintains a structural dependency graph of
the target codebase in SQLite+FTS5 (built via tree-sitter grammars for Java,
Python, JS/TS/TSX, Go, Rust and more, plus a heuristic fallback for the long
tail of languages), assembles a dependency-aware context graph per request,
understands the Pega Platform rule system in a version-adaptive way on Java
projects, runs an asynchronous build/verify feedback loop that records
successes as patterns and failures as anti-patterns, and ingests domain
knowledge. The whole subsystem is default-off (`neurocode.enabled: false`).
Tier-model routing additionally requires the HyperCode orchestrator-active
state (`hypercode.enabled: true` and `hypercode.orchestrator_mode: true`; see
the config table below); RAG and indexing do not.

> See also: [joey-neurocode-rag.md](joey-neurocode-rag.md) — the semantic RAG
> layer that lives in the same `graph.db`.

## Overview

- **Spec 015** ("please-ensure-neurocode" lineage): multi-language engine with
  an enterprise Java/Pega focus. Later specs layered on top: spec 021 added the
  RAG tables to this crate's schema (see
  [joey-neurocode-rag.md](joey-neurocode-rag.md)), and spec 023 added the
  enterprise analysis plane (`analysis.rs`, `policy/`, `risk.rs`,
  `verification_plan.rs`).
- **Consumers**: `joey-agent-core` consumes only the narrow
  [`NeuroCodeEngine`] trait (the graph store, ingestion pipeline, classifier
  internals, and feedback loop are private to this crate — Constitution VI).
  The NeuroCode engine is installed on the orchestrator/main session
  only; dispatched subagents receive no injected NeuroCode Context — the
  orchestrator includes any code-map facts a child needs directly in its
  brief (FR-021 cascade superseded 2026-09-08);
  `joey-tui` renders the context-graph snapshot;
  `joey-neurocode-rag` builds its chunk/vector store inside this crate's
  schema; `joey-cli` wires the `/neurocode` command and the auto-reindex
  turn-hook (see [joey-cli.md](joey-cli.md),
  [joey-agent-core.md](joey-agent-core.md),
  [joey-orchestration.md](joey-orchestration.md), [joey-tui.md](joey-tui.md)).
- **Orchestrator-only injection (revised 2026-09-08)**: The NeuroCode engine
  is installed on the orchestrator/main session only; dispatched subagents
  receive no injected NeuroCode Context — the orchestrator includes any
  code-map facts a child needs directly in its brief. At the engine layer,
  engines constructed for the same project root still lazily open the SAME
  per-project `graph.db` the parent built — no re-ingestion, no private index
  — and `Agent::neurocode_engine()` exposes the main session's
  `Arc<dyn NeuroCodeEngine>`.
- Everything is offline and deterministic: no network calls on the hot path
  (`classify` / `assemble_context` are non-async, O(1) on cached/indexed
  state — FR-017).

## Module map

69 files total (43 `src` + 23 `tests` + 2 `examples` + `Cargo.toml`):

| Group | Files | Role |
|---|---|---|
| core engine | `src/engine.rs` | `NeuroCodeEngine` + `NeuroCodeCommands` traits, `CodingRequest`, `DefaultEngine` |
| config | `src/config.rs` | `NeuroCodeConfig` from `neurocode.*` dotted keys |
| classifier | `src/classifier.rs` | deterministic tier classification (FR-001) |
| tier_resolver | `src/tier_resolver.rs` | tier → model id (Mode 2 config lookup) |
| risk | `src/risk.rs` | `RiskAssessment`, `RiskFactor`, `RiskLevel` (spec 023) |
| verification_plan | `src/verification_plan.rs` | `VerificationPlan`, `VerificationStep` (spec 023) |
| auto_index | `src/auto_index.rs` | edit tracking + re-index thresholds |
| analysis | `src/analysis.rs` | `EnterpriseTaskAnalyzer` unified analysis plane (spec 023) |
| `parse/` | 12 files: `mod.rs`, `registry.rs`, `grammars.rs`, `java.rs`, `python.rs`, `jsts.rs`, `golang.rs`, `rustlang.rs`, `heuristic.rs`, `pega.rs`, `extract.rs`, `spans.rs` | tree-sitter ingestion pipeline (FR-006) |
| `graph/` | `mod.rs`, `node.rs`, `edge.rs`, `store.rs` | typed dependency graph over SQLite+FTS5 |
| `context/` | `mod.rs`, `budget.rs`, `discovery.rs`, `snapshot.rs`, `tokens.rs` | graph-aware context assembly (FR-007/008) |
| `memory/` | `mod.rs`, `patterns.rs`, `domain.rs`, `outcomes.rs` | patterns, anti-patterns, domain knowledge, outcome memory |
| `pega/` | `mod.rs`, `metadata.rs`, `version.rs` | Pega rule metadata + version detection (FR-009) |
| `policy/` | `mod.rs`, `sources.rs`, `resolver.rs` | `PolicyBinding`, `PolicyLayer`, `CombinedPolicy`, `PolicyConflict` (spec 023) |
| `verify/` | `mod.rs`, `runner.rs`, `parse.rs` | build/verify feedback loop (FR-010/011/012) |
| tests | 23 files under `tests/` | see [Testing](#testing) |
| examples | `examples/ingest_frozen.rs`, `examples/ingest_perf.rs` | A/B and throughput harnesses |

`ingest_frozen` ingests a frozen copy of a HEAD tree (stable input for A/B
comparison) into a temp-home DB; `ingest_perf` measures pipeline throughput.
Both open a `DependencyGraph` directly and print `IngestionResult` counters.

## On-disk store

`NEUROCODE_SCHEMA_VERSION = 3` (v1 → v2 added the `signature` column on
`code_artifacts`; v2 → v3 added the RAG tables per spec 021 — both migrations
are additive and run in place on open).

Per-project location, resolved by `project_graph_db_path()`:

```text
~/.joey/neurocode/projects/<sha256-16-hex-of-canonical-root>/graph.db
```

The hash is the first 16 bytes (hex) of SHA-256 over the canonicalized project
root path; `JOEY_HOME` is honoured via `process_joey_home()`. The store opens
with `PRAGMA journal_mode=WAL`, `foreign_keys=ON`, and `busy_timeout=5000`
(a second process touching the same `graph.db` waits briefly instead of
failing with `SQLITE_BUSY`).

| Table | Purpose |
|---|---|
| `code_artifacts` | one row per parsed type/method/field/Pega rule; `UNIQUE(fqcn, kind, source_path)`; v2 added `signature` (declaration headers for methods/fields; existing rows keep NULL until re-indexed) |
| `graph_edges` | typed edges `(from_id, to_id, edge_kind)`, PK over all three; FKs to `code_artifacts` |
| `code_artifacts_fts` | FTS5 virtual table (`unicode61` tokenizer, external-content pattern over `code_artifacts`) |
| `code_artifacts_ai` / `_ad` / `_au` | 3 sync triggers keeping FTS in step (insert/delete/update) |
| `patterns` | `LearnedPattern` rows (`prompt_signature`, `generation_summary`, `verify_result`, `artifact_ids`, `tier`) |
| `anti_patterns` | `LearnedAntiPattern` rows (`error_signature`, `error_output`, `resolution`, `hit_count`, `status`) |
| `domain_knowledge` + `domain_knowledge_fts` | ingested knowledge-source registry + standalone FTS5 content index |
| `rag_chunks`, `rag_vectors`, `rag_index_meta`, `rag_model_artifacts`, `rag_chunk_edges` | v3 RAG tables (spec 021) — owned by `joey-neurocode-rag` |
| `schema_meta` | key/value schema bookkeeping |

A tombstone pass marks rows whose `source_path` no longer exists on disk as
`Deleted` (see [Ingest pipeline](#ingest-pipeline)); re-ingesting a path
reactivates its rows (upsert overwrites status).

## Public API

Re-exported from `lib.rs`:

| Item | Notes |
|---|---|
| `NeuroCodeEngine` trait | the narrow hot-path interface `joey-agent-core` consumes: `classify`, `assemble_context`, `assemble_context_with_progress` (streaming stage callbacks), `is_active`, `resolve_tier_model`, `record_file_edit`, `should_reindex`, `reindex_now`, `auto_index_progress` |
| `NeuroCodeCommands` trait | `/neurocode` command surface: `status_text`, `index_text(force)`, `query_text(type, symbol)`, `tier_text(action, tier)`, `patterns_text`, `anti_patterns_text`, `domain_list_text`, `domain_remove_text(id)`, `ingest_text(category, path, version, provenance)` — plain-text in/out |
| `DefaultEngine` | the implementation; `set_provider()` scopes tier-model resolution; `with_graph()` snapshots the persisted graph so a fresh engine reads an index built by a previous one |
| `CodingRequest` | `{ text, active_file, active_symbols, project_root, token_budget_hint }` |
| `ArtifactKind` | `Class`, `Interface`, `Enum`, `Method`, `Field`, `PegaRule` |
| `ArtifactStatus` | `Active`, `Stale`, `Deleted` |
| `EdgeKind` | `Implements`, `IsImplementedBy`, `Injects`, `ExchangesType`, `MemberOf`, `ReferencesRule`, `InheritsRule` (`Implements`/`IsImplementedBy` are inverses) |
| `ComplexityTier` | `Economical`, `Frontier`, `AmbiguousDefault` (`#[non_exhaustive]`; AmbiguousDefault resolves to the configured default tier) |
| `ComplexityRoute` | `{ tier, reasoning, overridden, override_tier, signals }` |
| `TierBudget` | per-tier context caps (below) |
| `GRAPH_HUB_DEPENDENTS_THRESHOLD` | `4` — a target with ≥ 4 dependents (or outgoing non-`MemberOf` edges) is a structural hub |
| `PegaMetadata` / `PegaRuleFamily` | `{ rule_class_family, rule_name, references_rules, inherits_from, pega_version }`; family = `RuleObj`, `Data`, `Work`, `Other` |
| `project_graph_db_path` | root → `graph.db` path |
| `LearnedPattern` / `LearnedAntiPattern` | memory rows; anti-patterns carry `hit_count` and `Active`/`Resolved` status |
| `DomainKnowledge` / `KnowledgeCategory` | retrieval hit; categories `FrameworkDocs`, `EntityCatalog`, `Postmortem`, `PegaRuleType` |
| `VerifyLoop` / `VerifyResult` / `VerifyOutcome` | feedback loop (below) with `should_escalate()` / `escalate_hint()` |
| `AutoIndexState` / `AutoIndexProgress` | edit tracking + re-index decision |
| `TierModelResolver` | tier → model id (below) |
| `AnalysisEngine` / `EnterpriseTaskAnalyzer` / `TaskAnalysis` / `ExecutionHint` / `TaskContext` | spec-023 unified analysis plane; `record_outcome` is the only legal write into outcome memory and only accepts a `VerifiedOutcome` |
| `PolicyBinding` / `PolicyLayer` / `CombinedPolicy` / `PolicyConflict`, `RiskAssessment` / `RiskFactor` / `RiskLevel`, `VerificationPlan` / `VerificationStep` | spec-023 surfaces |

Tier budgets (`TierBudget::for_tier`):

| Tier | `max_expansion_depth` | `max_primary_nodes` | `max_expanded_nodes` |
|---|---|---|---|
| `Economical` | 2 | 2 | 8 |
| `Frontier` | 3 | 3 | 24 |
| `AmbiguousDefault` | resolves to `Economical` | — | — |

## Ingest pipeline

`parse::ingest_project(graph, project_root)` (FR-006):

1. **Walk**: start at `src/` when it exists (Java/Kotlin convention), else the
   project root; `max_depth(10)`. Skip files whose extension is not in
   `SUPPORTED_EXTENSIONS`, and skip vendor paths — any path containing
   `node_modules`, `vendor`, `target`, `dist`, `build`, `.git`, `venv`,
   `.venv`, `__pycache__`, `.tox`, or `site-packages`.
2. **Parallel parse (rayon)**: read + tree-sitter parse fan out across cores;
   `collect()` preserves directory order so phase 2 is deterministic. Pega
   rule patterns are recognized during the same pass on Java extractions.
3. **Sequential upsert**: type-level nodes, method nodes (with `signature`),
   field nodes, module-level functions; `MemberOf` edges for members;
   same-file `Implements`/`IsImplementedBy`/`Injects` edges. Packages come
   from the extractor (Java, Go) or are derived from the directory path.
4. **Post-walk edge resolution**: cross-file references accumulated as
   pending edges resolve via an in-memory simple-name index, then FTS with an
   exact-match check.
5. **Tombstone pass**: every `Active` node whose project-relative
   `source_path` no longer exists on disk is marked `Deleted`, so deleted or
   renamed files stop producing phantom FTS hits and stale edges.

Language coverage: **19 grammar languages** registered in
`parse::registry::languages()` — `java`, `python` (`py`/`pyi`), `typescript`
(`ts`/`mts`/`cts`), `tsx`, `javascript` (`js`/`mjs`/`cjs`/`jsx`), `go`, `rust`,
`ruby`, `php`, `csharp`, `cpp` (6 extensions), `c` (`c`/`h`), `scala`,
`haskell`, `julia`, `ocaml`, `bash` (`sh`/`bash`/`zsh`), `verilog`
(`v`/`vh`/`sv`/`svh`), `agda` — compiled against **18 tree-sitter grammar
crates** (the `tree-sitter-typescript` crate provides both the TypeScript and
TSX grammars; see `Cargo.toml`). Extensions without a dedicated extractor (Kotlin, Swift,
Elixir, Lua, Zig, …, listed in `SUPPORTED_EXTENSIONS`) fall back to the
heuristic extractor so every programming language gets at least coarse
structural nodes. Markup/data grammars (CSS, HTML, JSON, …) are out of scope.

`parse::project_has_source()` is the fast gate used on the assembly hot path:
a bounded walk (first ~500 entries) looking for any supported source file or
`Rule-*` file, plus bounded reads of `build.gradle`/`pom.xml` for `com.pega`.
(The older Java-only `project_has_java` is a backward-compatible alias.)

## Pega support

- **Rule families** (`PegaRuleFamily::from_fqcn`): `Rule-Obj-*` patterns →
  `RuleObj`; `Data-*` → `Data`; `Work-*` → `Work`; anything else → `Other`.
  Matched Java types get `ArtifactKind::PegaRule` + `PegaMetadata`;
  `ReferencesRule`/`InheritsRule` edges are emitted to referenced/inherited
  rules (Pega extraction is Java-only — pattern matching keys on Java
  identifiers/annotations).
- **Version detection chain** (`pega::version::detect_pega_version`):
  1. `neurocode.pega.version` config override,
  2. build files — Gradle `build.gradle`/`build.gradle.kts` (com.pega/prweb/prpc
     dependencies) and Maven `pom.xml` (`<groupId>com.pega</groupId>` BOM),
  3. in-source markers (`com.pega.*` packages, `Rule-*` class patterns).
  An empty version still allows pattern-based rule extraction.
- `pega::metadata::rule_type_metadata_for_version()` grounds generation with
  rule-type descriptions (activities, flows, data transforms, decision rules,
  data types, cases); Infinity 23.x/24.x wording vs classic 8.x wording.

## Classifier & tiers

`ComplexityClassifier` is deterministic and non-async (no LLM call). Signals
(`ClassificationSignal` kinds): `Keyword`, `ScopeFanOut`, `GraphHub`.

- Economical-leaning default keywords: `test`, `getter`, `setter`,
  `boilerplate`, `implement method`, `junit`, `mock`, `stub`, `tostring`,
  `equals`, `hashcode`, `builder`, `dto`, `create`, `unit test`, `pytest`,
  `jest`, `vitest`, `unittest`, `docstring`, `comment`, `scaffold`, `rename`,
  `typo`, `log statement`, `constant`.
- Frontier-leaning default keywords: `refactor`, `architecture`,
  `concurrency`, `redesign`, `migrate`, `debug`, `transactional`, `deadlock`,
  `race condition`, `streams`, `optional`, `performance`, `optimize`,
  `thread-safe`, `async`, `await`, `goroutine`, `channel`, `unsafe`,
  `borrow`, `lifetime`, `ownership`, `move semantics`, `promise`, `closure`,
  `asyncio`, `generator`, `decorator`, `middleware`, `hook`, `observer`,
  `callback hell`, `middleware chain`, `memory leak`, `circular dependency`,
  `design pattern`.
- Config override semantics: an **absent** keyword key keeps the built-in
  lists; an explicitly **empty list (`[]`)** disables keyword matching for
  that tier; a custom list replaces the built-ins.
- `neurocode.classifier.scope_fanout_frontier_threshold` (default `4`): more
  referenced artifacts than this leans Frontier.
- `GRAPH_HUB_DEPENDENTS_THRESHOLD = 4`: a target with ≥ 4 dependents is a
  structural hub (Frontier evidence).
- `/neurocode tier pin` installs a manual override (`overridden: true` on the
  route — FR-002).

**Tier model resolution** (`TierModelResolver`, Mode 2): reads
`neurocode.tier.<tier>.model` directly from config; per-provider overrides
`neurocode.tier.providers.<provider>.{frontier,economical}` win per-field with
the flat keys filling gaps; a missing tier model falls back to the agent
default with a recorded reason. (Mode 1 — composing with spec 011's
`ModelAllocator` — is handled by the turn-loop intercept, not this resolver.)

## Context assembly & memory

`context/` builds a ranked, budget-capped expansion per request:

- `discovery.rs` — extracts identifier/path mentions from free text (ranked:
  backtick spans, dotted references, CamelCase/snake_case identifiers, long
  non-stopword words; capped at 10 identifiers / 5 paths).
- `budget.rs` — `TierBudget` caps (table above) keep the assembled context a
  targeted briefing, not a file dump.
- `ExpansionReason` priority (pulled in first when the budget binds):
  `InheritsRule` (0) → `ImplementsInterface` (1) → `MemberOfTarget` (2) →
  `InjectedByTarget` (3) → `ExchangesTypeWithTarget` (4) → `ReferencesRule` (5).
- `tokens.rs` — conservative `estimate_tokens`: max of `ceil(len/3.5)` and the
  whitespace-word count (symbol-dense text is ~3 chars/token, not 4).
- `snapshot.rs` — `ContextGraphSnapshot` structured node/edge view for
  interactive visualization (`AssembledContext.snapshot`).
- `AssembledContext` carries `primary_nodes`, `expanded_nodes` (with reason +
  via + depth), `formatted_context`, `token_estimate`, `cold_mode` (project
  unindexed — FR-016 degraded mode), and an optional `notice`.

Memory (`memory/`):

- **Patterns** (`patterns` table): successful generations keyed by
  `prompt_signature` with `verify_result` and tier.
- **Anti-patterns** (`anti_patterns` table): failures with their fix,
  surfaced as a warning when the same area is edited again (FR-011);
  `hit_count` increments and status is `Active`/`Resolved`.
- **Domain knowledge**: ingested sources (`ingest_text` /
  `KnowledgeSource`) in four categories; directory ingestion is capped at 32
  files / 512 KiB, deterministic name order, binary-looking content skipped.
- **Outcome memory** (`outcomes.rs`): verified completions feed lessons back
  into task context (`TaskContext.lessons`).

## Adaptive memory (feature 027)

Spec 027 layers an **episodic + semantic adaptive memory** over the structural
graph and the RAG index: completed turns are captured as episodes, distilled
into durable semantic preferences, and the most relevant memories are injected
into later turns as a compact block beside the RAG prefetch. The whole feature
is default-off (`neurocode.memory.enabled = false`); a disabled session
behaves exactly as before.

- **Episodic + semantic model**: `memory_episodes` records what happened —
  one episode per completed turn, captured at turn end on both success and
  error exits (hypercode goal-prefix sections and each completed plan unit
  get their own episodes). `memory_preferences` holds the distilled durable
  facts. Both, plus `memory_vectors` for semantic recall, are schema-v4
  tables in the same per-project `graph.db` (see
  [On-disk store](#on-disk-store)).
- **Distillation**: episodes condense into preferences via a deterministic
  heuristic; setting `neurocode.memory.distill_model` to a provider model
  switches distillation to provider-assisted.
- **Injection**: retrieval is bounded per section by
  `neurocode.memory.top_k` (5) and the whole block by
  `neurocode.memory.injection_char_limit` (2048); the episode store is
  capped at `neurocode.memory.max_episodes` (500, oldest evicted first).
  The block renders beside the RAG prefetch, never replacing it.
- **Sanitization choke point**: captured and injected memory text passes
  the same untrusted-content sanitization/threat-scan layers as every other
  external input before it reaches the model.
- **Command surface**: `/neurocode memory` inspects and manages the store —
  grammar `status | list | show | search | correct | delete | enable |
  disable` (see [joey-cli.md](joey-cli.md)).

The retrieval leg reuses the RAG embedding machinery — see
[joey-neurocode-rag.md](joey-neurocode-rag.md).

## Verification loop

Config (`verify.steps[]`, each `{ name, command, parse = "plain",
timeout_sec = 120 }`; `max_fix_iterations = 3`):

- `VerifyStep` runner: `shlex`-split command, `which` presence check, wall
  clock timeout; a missing tool or timeout marks the step **skipped**
  (FR-012 graceful degradation) — skipped ≠ failed and never triggers a fix
  iteration or escalation.
- `VerifyLoop::run_with_fixes`: run all steps; while not everything passed
  and the fix budget isn't exhausted, invoke the fix callback, re-run only
  the failed steps, increment the counter. A fix callback returning `false`
  (nothing changed) breaks early. Never blocks the interactive turn — the
  agent runs it detached.
- `VerifyOutcome::should_escalate()` — true when the run still fails AND
  `fix_iterations_used >= max_fix_iterations`.
- `VerifyOutcome::escalate_hint()` — `Some("frontier")` when the failing run
  was served by the economical tier (the "router/developer disagree" edge
  case: economical was judged sufficient but verification proved otherwise).
  The agent layer re-dispatches on the frontier tier.
- `verify::parse` turns raw output into structured errors per the step's
  `parse` format (`VerifyParseFormat`).

## Auto-index

Feature-015 follow-up (dynamic context across turns). The agent records
source-file edits via `record_file_edit(path, added, removed)`; at turn end it
asks `should_reindex()` and, when true, rebuilds via `reindex_now()`
(`spawn_blocking` — off the turn loop's critical path).

| Key | Default | Meaning |
|---|---|---|
| `neurocode.auto_index.enabled` | `true` | re-index after large edits (when NeuroCode itself is enabled) |
| `neurocode.auto_index.file_threshold` | `3` | distinct edited files that trigger a re-index |
| `neurocode.auto_index.line_threshold` | `200` | cumulative added+removed lines that trigger one even below the file threshold |
| `neurocode.auto_index.min_interval_secs` | `30.0` | debounce: minimum seconds between automatic passes |

`AutoIndexState` tracks edited files (a `BTreeSet`), cumulative lines, the
last index time, and a monotonic generation counter (bumped on every
completed re-index so callers can detect "the graph changed under me").
`AutoIndexProgress` exposes `files/threshold`, `lines/threshold` for status
display ("2/3 files toward auto-reindex"). Re-indexing is additive to
correctness, never blocking: failures are reported and ignored, and the next
turn's assembly continues against the previous index with its staleness note.

## Model-facing tools

Registered in `joey-tools` (see [joey-tools.md](joey-tools.md)); the four
structural tools are absent entirely when NeuroCode is disabled:

| Tool | Role |
|---|---|
| `neurocode_index` | build/refresh the structural graph (parses the tree, returns files scanned / artifacts / edges) — `coding` toolset |
| `neurocode_query` | query the graph by type + symbol |
| `neurocode_status` | index size, tier config, pattern counts |
| `neurocode_ingest` | ingest a domain-knowledge source |
| `neurocode_search` | hybrid semantic search — RAG-gated: registered only when `neurocode.rag.enabled` is true, not merely check()-disabled |

The `/neurocode` CLI slash command maps onto the `NeuroCodeCommands` trait
(`status`, `index`, `query`, `tier show|pin|unpin|set`, `patterns`,
`anti-patterns`, `domain list|remove`, `ingest`) — see
[joey-cli.md](joey-cli.md).

## Configuration

Loaded from `config.yaml` dotted keys by `NeuroCodeConfig::from_config`:

| Key | Default | Notes |
|---|---|---|
| `neurocode.enabled` | `false` | master switch (default-off, FR-003) |
| `hypercode.enabled` + `hypercode.orchestrator_mode` | `false` / `true` | tier-model routing additionally requires the orchestrator-active state (both flags true; the mode flag defaults to true); otherwise the agent stays on the configured main model. RAG/indexing are independent of this gate |
| `neurocode.tier.economical.model` | `""` | economical tier model id |
| `neurocode.tier.frontier.model` | `""` | frontier tier model id |
| `neurocode.tier.ambiguous_default` | `"economical"` | which tier `AmbiguousDefault` maps to (`"economical"` or `"frontier"`) |
| `neurocode.tier.providers.<provider>.frontier` | — | per-provider frontier override |
| `neurocode.tier.providers.<provider>.economical` | — | per-provider economical override (partial entries inherit the flat keys per-field) |
| `neurocode.verify.steps` | `[]` | list of `{name, command, parse, timeout_sec}` |
| `neurocode.verify.max_fix_iterations` | `3` | fix-iteration ceiling |
| `neurocode.classifier.scope_fanout_frontier_threshold` | `4` | fan-out → Frontier threshold |
| `neurocode.classifier.economical_keywords` | absent | absent = built-ins; `[]` = disabled; list = custom |
| `neurocode.classifier.frontier_keywords` | absent | same semantics |
| `neurocode.pega.version` | `""` | explicit override; empty = auto-detect (FR-009, Q4) |
| `neurocode.auto_index.enabled` | `true` | see [Auto-index](#auto-index) |
| `neurocode.auto_index.file_threshold` | `3` | |
| `neurocode.auto_index.line_threshold` | `200` | |
| `neurocode.auto_index.min_interval_secs` | `30.0` | |

RAG keys (`neurocode.rag.*`) are documented in
[joey-neurocode-rag.md](joey-neurocode-rag.md).

## Testing

23 integration test files under `crates/joey-neurocode/tests/`, plus inline
`#[cfg(test)]` unit tests across `src/`:

| File | Covers |
|---|---|
| `classifier.rs` | deterministic tier classification + signals |
| `graph_round_trip.rs` | node/edge upsert + read-back through the store |
| `multilang_ingest.rs` | multi-language ingestion across grammars |
| `non_java_fallback.rs` | heuristic fallback for long-tail languages |
| `tree_sitter_extract.rs` | tree-sitter extraction shapes |
| `parse_line_spans.rs` | line-span mapping from byte spans |
| `pega_ingest.rs`, `pega_context.rs`, `pega_version_detect.rs` | Pega rule extraction, context, version chain |
| `context_assembly.rs`, `context_enrichment.rs`, `context_snapshot.rs` | assembly, enrichment, UI snapshot |
| `verify_loop.rs`, `verify_orchestrator.rs`, `verify_degradation.rs` | feedback loop, orchestration, graceful degradation |
| `anti_pattern_surface.rs`, `domain_knowledge.rs`, `outcome_memory_recurrence.rs` | memory surfaces |
| `enterprise_analysis.rs` | spec-023 unified analysis plane |
| `status_persistence.rs` | `/neurocode status` reads a previously built graph |
| `regression_disabled.rs` | byte-identical behavior when disabled |
| `subagent_cascade.rs` | engine-layer graph.db sharing by project root (FR-021); manager-level cascade removed 2026-09-08 — orchestrator-only injection |
| `rag_schema_migration.rs` | v2 → v3 additive RAG migration |

Run scoped: `cargo test -p joey-neurocode`.

## See also

- [joey-neurocode-rag.md](joey-neurocode-rag.md) — semantic RAG over this graph
- [joey-agent-core.md](joey-agent-core.md) — turn-loop integration
- [joey-orchestration.md](joey-orchestration.md) — orchestrator-only NeuroCode injection policy (FR-021 revised 2026-09-08)
- [joey-tools.md](joey-tools.md) — `neurocode_*` tools
- [joey-cli.md](joey-cli.md) — `/neurocode` command
- [joey-tui.md](joey-tui.md) — context-graph visualization
- [README.md](README.md) — features index
