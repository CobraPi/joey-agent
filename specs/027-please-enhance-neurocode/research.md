# Research: NeuroCode Adaptive Memory

Phase 0 output. Every decision below was resolved against verified code facts (file references inline); no NEEDS CLARIFICATION remains.

## R1: Where memories live

- Decision: new tables `memory_episodes`, `memory_preferences`, `memory_vectors` inside the existing per-project `graph.db`, added by an additive-idempotent schema-v4 batch in `apply_schema` (`crates/joey-neurocode/src/graph/store.rs`), bumping `NEUROCODE_SCHEMA_VERSION` 3 → 4 (`crates/joey-neurocode/src/lib.rs:74`).
- Rationale: per-project DB gives FR-012 scoping for free (path = SHA-256 of canonical repo root, `store.rs:952`); the v2→v3 migration proves the pattern (pure `CREATE TABLE IF NOT EXISTS`, old DBs open unchanged); versioning via `schema_meta` row is the established mechanism (no PRAGMA user_version in this codebase).
- Alternatives considered: (a) memories as rows in `rag_chunks`/`rag_vectors` — rejected: `chunk_kind` has a `CHECK (IN ('symbol','fallback'))` constraint, so memory kinds would need a table rebuild of a v3 table (regression risk, Principle VII) and would couple memory lifecycle to code-index refreshes (`purge_paths` on every refresh could destroy memories); (b) separate `memory.db` beside `graph.db` (the `outcomes.db` precedent) — rejected: FR-005 demands the same retrieval pipeline, and vector search machinery reads from the graph store connection; a second DB would duplicate that path.

## R2: How memories are retrieved (RAG reuse)

- Decision: a dedicated memory retrieval leg in `crates/joey-neurocode-rag/src/memory_search.rs` that reuses the existing machinery as functions — `EmbeddingBackend` resolve (`embed/mod.rs:295,708`), quantized vector encode (`vector/quantize.rs` encode_f32/encode_int8, same BLOB format as `rag_vectors`), dense cosine scan (`vector/scan.rs:171`), and RRF fusion (`search/rrf.rs:63`) of the dense leg with a keyword leg over memory text; memories get synthetic source ids `memory://episode/<id>` and `memory://preference/<id>`.
- Rationale: FR-005 requires the same machinery without a parallel search stack; composing the public functions gives exactly that while keeping the code index and memory index namespace-isolated (memory delete/supersede never touches code chunks and vice versa).
- Alternatives considered: routing memory text through `index_file` (`index/chunker.rs:133`) — rejected with R1(a); tagging code-search results with a memory filter — rejected: mixes lifecycles and pollutes code search relevance.

## R3: Interactive capture point

- Decision: post-turn capture at the three `run_turn` exit paths in `crates/joey-agent-core/src/agent.rs` (the exact sites that call `neurocode_auto_reindex` today, lines ~2705/2801/2815), mirroring that precedent: gated on the memory config, executed off the response critical path, one episode per completed task (clarified Q2).
- Rationale: no generic `on_turn_end` hook exists on `Agent` (verified — zero matches); `neurocode_auto_reindex` is the sanctioned post-turn pattern already on every exit path; post-turn placement keeps SC-003's ≤2s bound untouched.
- Alternatives considered: (a) per-LLM-message tool-call capture — rejected: relies on model compliance, loses failed tasks the model abandons; (b) session-end batch — rejected: violates Q3 (continuous, no batching delay).

## R4: Hypercode capture and read points

- Decision: write one episode per completed workstream in `finalize_graph_run` beside `record_verified_outcomes` (`crates/joey-cli/src/hypercode.rs:1961-1997, 2089-2099`), sourcing task/focus from `Workstream`, outcome from `successes[i]`, summary/lessons from `build_summaries[i]` (`HypercodeReport`, hypercode.rs:585-592); read side extends the existing `lesson_prefix` injection in `HypercodeDispatcher::dispatch` (hypercode.rs:2197-2228) with the top-k active preferences and relevant episodes retrieved by similarity to the task objective.
- Rationale: these are the already-proven write (post-gate, `TaskStatus::Completed`-only) and read (goal-prefix) points; FR-006 asks exactly for both directions; the existing `OutcomeStore`/verified-lesson mechanism is composed with, not replaced (spec Assumptions).
- Alternatives considered: capturing per child-subagent turn — rejected: episode unit is the workstream/task (Q2); per-subagent detail stays in evidence ids referencing `runs/<run-id>/nodes/<id>.json`.

## R5: Distillation (continuous, per episode)

- Decision: a `MemoryDistiller` trait in `joey-neurocode::memory::distill` with two parts: (1) a synchronous heuristic detector for explicit preference statements in user text (patterns like "I prefer/always/never …") that writes `origin=explicit` preferences immediately at capture; (2) an async per-episode distillation job (spawned at capture, continuous per Q3) where the provider-backed implementation (wired in `joey-cli`, using the NeuroCode economical tier) proposes/strengthens `origin=inferred` preferences; recurrence strengthening = same category + statement-embedding cosine ≥ 0.92 → append evidence, bump confidence, refresh `updated_at`; otherwise insert new.
- Rationale: Q1 (fully automatic) + Q3 (continuous) demand no gate and no batch; the trait keeps `joey-neurocode` provider-free (crate DAG unchanged, Principle VI); heuristic-first keeps explicit preferences working with zero model cost (Principle VIII).
- Alternatives considered: LLM-only distillation — rejected: adds a model call dependency to explicit statements that need none; nightly consolidation — rejected: batching delay violates Q3.

## R6: The Q1 mandate — "modify all existing implementations to match"

- Decision: inventory result — the `/memory` approval gate (`crates/joey-cli/src/slash_extra.rs:906-946`) is display-only for the separate MEMORY.md/USER.md subsystem; this port has no enforcement queue at all (line 930: "No pending memory writes — this port applies memory writes directly"). Therefore nothing existing gates the new memory path. The mandate is implemented as: (1) the neurocode memory path never consults `memory.approval_required` and never routes through approval UI; (2) no new gate is introduced; (3) the MEMORY.md display subsystem remains untouched (different subsystem, no conflict).
- Rationale: verified against source; avoids inventing work or breaking an unrelated surface (Principle VII).
- Alternatives considered: removing the `memory.approval_required` config key and `/memory approval` display — rejected: it is an existing public config surface (Principle VII) for a different subsystem; the clarification only requires the adaptive-memory model to be ungated.

## R7: Conflict resolution and aging (FR-008/FR-009)

- Decision: resolution rank = (origin: explicit > inferred, then recency by `updated_at`); on insert of a same-category preference with higher rank, the lower-ranked one is marked `superseded` with `supersedes`/`superseded_by` links (soft state, kept for audit); hard delete (`/neurocode memory delete`) removes row + vector so it can never reappear (SC-004); equal-rank explicit-vs-explicit conflicts are listed as unresolved in `/neurocode memory status` (surfaced, per FR-008).
- Rationale: deterministic order the spec mandates; soft-supersede preserves evidence trail; hard delete honors SC-004.
- Alternatives considered: physical deletion on supersede — rejected: loses audit/evidence trail that distillation uses; last-write-wins silently — rejected: FR-008 requires ties surfaced.

## R8: Injection shape and budget

- Decision: a memory block appended to the effective system prompt exactly like the RAG prefetch block (`apply_rag_prefetch` / `effective_system_prompt`, agent.rs:1060-1078): top-k (default 5) active preferences + relevant episodes by similarity to the current prompt, formatted compactly, capped by `neurocode.memory.injection_char_limit` (default 2048, mirroring the existing `memory_char_limit: 2200` precedent); hypercode children receive the same content as a goal prefix beside the existing lesson prefix.
- Rationale: reuses the sanctioned injection point (system prompt built once, cache-friendly); char cap bounds latency and context cost (Principle VIII, SC-003).
- Alternatives considered: per-message tool retrieval — rejected: extra round trips and model-driven variance; unbounded top-k — rejected: violates SC-003 spirit.

## R9: Config keys and default-off

- Decision: five additive keys in the `RAG_CONFIG_KEYS` pattern (`crates/joey-neurocode-rag/src/config.rs:165-188`): `neurocode.memory.enabled` (Bool, false), `.top_k` (Int, 5), `.injection_char_limit` (Int, 2048), `.max_episodes` (Int, 500), `.distill_model` (Str, "" = use NeuroCode economical tier). Full contract in contracts/neurocode-memory-config-keys.md.
- Rationale: default false satisfies FR-010 byte-identical disabled behavior; the key-spec table pattern is the established contract surface with clamping/validation precedent.
- Alternatives considered: one master key only — rejected: unbounded injection would violate SC-003.

## R10: Dependencies

- Decision: zero new runtime dependencies. Reuses pinned `rusqlite` (bundled), `sha2`, and the `ort`/`tokenizers`/`ndarray` embedding stack already required by `joey-neurocode-rag` (pinned per docs/features/joey-neurocode-rag.md).
- Rationale: Principle VIII — no measurable benefit from any new crate; everything needed (SQLite, embeddings, cosine, fusion, JSON) is already present.
- Alternatives considered: a dedicated vector library — rejected: duplicate capability, binary-size and compile-time cost with no gain.

## R11: Secrets

- Decision: run episode/preference text through `joey-core`'s existing secret-redaction layer before persist (same layer that sanitizes tool/context output), and skip persisting a memory whose redacted text is empty.
- Rationale: FR-011; reusing the existing layer avoids a second redaction implementation (Principle VI).
- Alternatives considered: redaction at read time — rejected: secrets would sit at rest in `graph.db`.

## R12: Performance budget (Principle VIII)

- Decision: budgets — injection: ≤ 2s p95 hard (SC-003), < 100 ms target local (one query embedding + dense scan over ≤ max_episodes + preferences vectors); capture+distill: post-turn, off critical path, episode text capped 4 KB, distillation one economical-tier call per episode; eviction: FIFO beyond `max_episodes` (default 500).
- Rationale: SC-003 and Principle VIII; matches the auto-reindex precedent for background work.
- Alternatives considered: unbounded corpus — rejected: scan cost grows monotonically, violating the budget.

## R13: Addendum — `tracing` direct dependency in joey-neurocode-rag (final gate, Constitution VIII record)

- Decision: `tracing.workspace = true` was added as a direct dependency of `joey-neurocode-rag` during feature 027's final verification gate.
- Why: the panic-proof degradation path in `memory_search.rs` (dense leg skipped after an embedder panic, per FR-008 silent-degradation semantics) emits a warning on that path; the workspace observability standard is `tracing` (already the mechanism used by `joey-neurocode`, `joey-cli`, and the engine's `neurocode` target), so feature-027 warnings follow it.
- Weight: zero added cost. `tracing` was already in `joey-neurocode-rag`'s build graph through its direct dependency on `joey-neurocode` (which depends on `tracing`); promoting it to a direct dependency adds no compilation unit, no binary size, and no transitive surface — it only makes an existing transitive use explicit.
- Alternatives considered: (a) `eprintln!` — rejected: inconsistent with workspace observability (no target-level filtering; the crate's one prior degradation notice predates this standard and is unchanged); (b) the `log` crate — rejected: not used anywhere in this workspace, so it would be a genuinely new dependency with real cost. This satisfies Constitution VIII's requirement that dependency weight be recorded against alternatives in the feature's research.md.
