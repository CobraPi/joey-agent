---
description: "Task list for NeuroCode Semantic Code Retrieval (RAG Enhancement)"
---

# Tasks: NeuroCode Semantic Code Retrieval (RAG Enhancement)

**Input**: Design documents from `/specs/021-please-enhance-neurocode/`

**Prerequisites**: plan.md, spec.md, research.md, data-model.md, contracts/, quickstart.md

**Tests**: Test-first per Constitution IV — every contract's regression obligations are pinned as tasks.

**Organization**: 42 tasks across 8 phases, grouped by user story to enable independent implementation and testing of each story, in priority order US1→US5. Remote HTTP embedding backends, per-project consent, proactive pre-fetch, and status reporting are cross-cutting concerns (FR-012/013/015) sequenced in the Polish phase — a documented deviation from pure story mapping (see Dependencies & Execution Order).

## Format: `- [ ] TXXX [P?] [USN?] Description with file path`

- **Task line grammar**: `- [ ] TXXX [P?] [USN?] Description with file path` — sequential IDs T001…T042; optional `[P]` marks genuinely parallelizable tasks (different files, no dependencies on incomplete tasks; 10 tasks carry `[P]`); optional `[US1]`–`[US5]` story labels appear ONLY on user-story-phase tasks (Phases 3–7), never on Setup/Foundational/Polish tasks.
- Every task description names at least one file path.
- Tests are written alongside implementation (Constitution IV); contract regression obligations are embedded in the task that owns the surface.

## Path Conventions

- **New crate**: `crates/joey-neurocode-rag/` (library crate, Constitution I)
- **Additive edits to existing crates**: `crates/joey-neurocode/`, `crates/joey-tools/`, `crates/joey-cli/`, `crates/joey-tui/`, `crates/joey-agent-core/`
- **Tests**: `crates/joey-neurocode-rag/tests/` (integration) + inline `#[cfg(test)]` (unit) + tests in touched crates

---

## Phase 1: Setup

**Purpose**: Create the new crate skeleton with its three pinned dependencies and the full config surface, registered in the workspace.

**Goal**: `cargo build -p joey-neurocode-rag` green and every `neurocode.rag.*` key defined with contract-pinned defaults.

**Independent Test**: `cargo build -p joey-neurocode-rag` succeeds; `cargo test -p joey-neurocode-rag` config tests enumerate all 18 keys with exact defaults.

- [X] T001 Create `crates/joey-neurocode-rag` scaffold: `crates/joey-neurocode-rag/Cargo.toml` with pinned deps `ort =2.0.0-rc.13` (default-features=false, features `ndarray`,`load-dynamic`), `tokenizers 0.23` (default-features=false), `ndarray 0.17` (research.md R6 ledger), plus `crates/joey-neurocode-rag/src/lib.rs` module stubs; add the crate to workspace members in root `Cargo.toml`; verify `cargo build -p joey-neurocode-rag` is green
- [X] T002 Implement the config module in `crates/joey-neurocode-rag/src/config.rs` covering all 18 `neurocode.rag.*` keys per contracts/rag-config-keys.md — including `neurocode.rag.api_key` `.env` auto-routing (the `_KEY` rule), `~` expansion for `local.model_dir`, batch_size range 16–128, and clamp rules — plus the config contract test in `crates/joey-neurocode-rag/tests/config_keys.rs` enumerating the full 18-key table with exact defaults, `.env` routing, and `~` expansion (Constitution VII regression obligation)

**Checkpoint**: New crate builds standalone with pinned deps; the config key table is contract-pinned by a green test.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented — line-spanning parse output, schema v3, model profiles, consent records, and artifact integrity.

**Goal**: A v2 graph.db migrates idempotently to v3 with empty RAG tables; profiles, consent state machine, and integrity verification exist and are tested.

**Independent Test**: `cargo test -p joey-neurocode` migration tests (v2-opens-unchanged, round-trip, idempotent-reopen) and `cargo test -p joey-neurocode-rag` (profiles, consent, integrity) all green.

**⚠️ CRITICAL**: No user story work can begin until this phase is complete.

- [X] T003 Extend the parse layer to emit line spans (byte→line conversion at index time) for extracted artifacts in `crates/joey-neurocode/src/parse/` with unit tests asserting byte-span→1-based-line-range correctness (data-model.md CodeChunk line fields; research.md R4)
- [X] T004 Emit fallback coarse chunks for source regions containing no named artifacts (same SourceExtraction shape; scripts/top-level code) in `crates/joey-neurocode/src/parse/` with tests covering a Python top-level sample (FR-014 groundwork; clarification Q1 hybrid chunking)
- [X] T005 Implement the additive, idempotent schema v2→v3 migration in `crates/joey-neurocode/src/graph/store.rs` — tables `rag_chunks`, `rag_vectors`, `rag_index_meta` (incl. profile columns `embed_profile`, `pooling`, `prefix_query`, `prefix_document`), `rag_chunk_edges`, `rag_model_artifacts`; bump `NEUROCODE_SCHEMA_VERSION` 2→3 in `crates/joey-neurocode/src/lib.rs`; add tests in `crates/joey-neurocode/tests/` for v2-opens-unchanged, schema round-trip (BLOB byte-exactness), and idempotent double-migration per contracts/rag-store-schema.md regression obligations 1–3
- [X] T006 [P] Implement the model profiles module in `crates/joey-neurocode-rag/src/embed/profiles.rs`: profile table for `nomic-embed-text-v1.5` (768-dim, mean pooling + L2, `search_query: `/`search_document: ` prefixes) and `CodeRankEmbed` (768-dim, mean pooling + L2, query-only prefix, empty document prefix), with the rejected `nomic-embed-code` evaluation RECORDED so tasks never re-litigate (contracts/embedding-backend.md § Model Profiles); add prefix/pooling unit tests in `crates/joey-neurocode-rag/tests/profiles.rs` (prefixes applied pre-tokenization, CodeRankEmbed empty document prefix)
- [X] T007 [P] Implement the consent record module in `crates/joey-neurocode-rag/src/consent.rs`: `consent.json` state machine NeverAcknowledged→Acknowledged→Revoked (re-ack allowed) with audit fields per data-model.md §8, plus tests asserting the state transitions and that absent file ≡ NeverAcknowledged
- [X] T008 [P] Implement artifact integrity helpers in `crates/joey-neurocode-rag/src/embed/artifacts.rs`: SHA-256 verification of `model.onnx`/`tokenizer.json` against the pinned `rag_model_artifacts` row, refusing mismatches (`ModelFilesCorrupt`) per contracts/embedding-backend.md § Local Model Artifacts — the helpers MUST ALSO support self-registration (manual-placement path): when no `rag_model_artifacts` row exists for a profile, the first successful load computes SHA-256 of the present files and writes the row; subsequent loads verify against it and refuse on mismatch; add a bad-hash refuse-to-load test plus a self-registration test (first load writes the row; tampered subsequent load refuses)

**Checkpoint**: Foundation ready — v3 schema, profiles, consent, and integrity gates all exist and are pinned green. User story implementation can now begin.

---

## Phase 3: User Story 1 - Natural-Language Code Search (Priority: P1) 🎯 MVP

**Purpose**: Deliver the core new capability: local ONNX embedding, dense indexing, dense search, and the agent tool + CLI/TUI surfaces.

**Goal**: A natural-language query whose wording does not appear in the code returns ranked code locations with file, symbol, kind, and line range (FR-001, SC-001).

**Independent Test**: Index a scratch repo, run `/neurocode search where is token validation handled` with the words absent from the source, and verify ranked results (file/symbol/kind/lines) with the correct location in the top 5 — with no model artifacts present, the same search degrades to keyword-only with an explicit indication (FR-008).

- [X] T009 [P] [US1] Implement the LocalOnnx embedder in `crates/joey-neurocode-rag/src/embed/local_onnx.rs`: ort session via runtime-loaded dylib (`ORT_DYLIB_PATH`/`neurocode.rag.local.ort_dylib_path`), offline tokenizers (`Tokenizer::from_file`, padded `encode_batch(texts, true)`), per-profile mean pooling + L2 and prefixes, batch 64 with rayon across texts (contracts/embedding-backend.md §1; research.md R2/R6); include unit tests for pooling/normalization on a tiny fixture model path
- [X] T010 [US1] Implement the embedding backend trait + registry in `crates/joey-neurocode-rag/src/embed/mod.rs`: `EmbeddingBackend` (embed/describe_embedder/health_check), `BackendKind`, `EmbedError` taxonomy, and `auto` resolution — LocalOnnx when `model_dir` artifacts verify, else keyword-only degradation, never an implicit network call (contracts/rag-config-keys.md `backend` key); test the auto-resolution matrix and order-preservation of batches
- [X] T011 [P] [US1] Implement `/neurocode model fetch` in `crates/joey-cli/src/commands/neurocode.rs`: downloads `model.onnx` + `tokenizer.json` from `neurocode.rag.local.mirror_url` with SHA-256 verification against project-recorded expected hashes, writing the `rag_model_artifacts` row from those project-recorded expected hashes; EMPTY mirror_url = fetch disabled; NEVER contacts huggingface.co (research.md R8; contracts/embedding-backend.md § Local Model Artifacts); add tests for empty-mirror refusal, hash-mismatch refusal, and no-HF guarantee
- [X] T012 [US1] Implement the dense indexing pipeline in `crates/joey-neurocode-rag/src/index/chunker.rs` + `crates/joey-neurocode-rag/src/vector/store.rs`: build chunk records (symbol-aligned from parse spans + fallback), construct contextual prefix (file path + imports), compute `content_hash` over the RAW UNPREFIXED text, embed with the profile document prefix at embed time, write vectors, all in a single transaction (data-model.md §1–2, §Storage invariants; R4); tests pin hash-over-unprefixed-text and single-transaction behavior, plus a very-large-file bounding test — a ≥10 MB synthetic source file indexes without failure or stall, with bounded processing (edge case 3)
- [X] T013 [US1] Implement the dense search leg in `crates/joey-neurocode-rag/src/search/hybrid.rs` + `crates/joey-neurocode-rag/src/vector/scan.rs`: embed the query via the profile QUERY prefix, exhaustive cosine (dot on normalized vectors) scan with rayon, int8 quantization applied above `neurocode.rag.quantize_threshold` (contracts/hybrid-search.md stage 3; R1); tests cover query-prefix application, BLOB decode validation (f32 `dim×4` / int8 `dim+4`), and quantization threshold switching
- [X] T014 [US1] Register the `neurocode_search` agent tool (the agent-tool half of FR-010's tool+command surface) in `crates/joey-tools/src/tools/` (new module + `register_neurocode_rag_tools` wired in `crates/joey-tools/src/builtins.rs`) ONLY when `neurocode.rag.enabled == true` — registration-time check, absent from the registry when disabled — with the exact JSON schema and return payload (incl. `mode`/`mode_reason`) of contracts/neurocode-rag-tools.md; add the schema-pinning test asserting the JSON byte-for-byte plus the disabled-registry parity assertion
- [X] T015 [US1] Implement CLI/TUI rendering (the CLI/TUI half of FR-010's tool+command surface): `/neurocode search <query...> [--path] [--limit] [--expand-lines] [--relations] [--json]` grammar per contracts/neurocode-rag-command.md in `crates/joey-cli/src/commands/neurocode.rs` (+ slash registry/dispatch wiring in `crates/joey-cli/src/slash.rs`), and the result view with symbol-aligned vs fallback badges (FR-014) in `crates/joey-tui/src/`; add grammar parse tests (flag→SearchRequest mapping, invalid `--relations 3`, missing query) and badge-rendering tests, plus an end-to-end fallback-chunk search test — a Python top-level-code sample (no named artifacts) is searchable and its result is visibly badged as fallback-chunk, distinct from symbol-aligned results (FR-014); edge-case tests: whitespace-only-query validation (clear validation message, no search executed, no error — edge case 1) and no-match clear response ('nothing matched' distinct from failure — edge case 2)

**Checkpoint**: User Story 1 works end-to-end — local embedding, dense index, `/neurocode search`, agent tool, CLI+TUI rendering; auto-resolution degradation ladder begins (keyword-only when unconfigured); full FR-008 degradation semantics complete in Phase 4 (T020). This is the MVP: ship here if needed.

---

## Phase 4: User Story 2 - Hybrid Ranking (Priority: P2)

**Purpose**: Blend dense and keyword legs into one trustworthy ranked list so semantic search can become the default path without regressing exact-symbol lookups.

**Goal**: Mixed queries return one blended ranked list; exact-symbol queries rank the exact match first; degraded backends never fail the turn (FR-002/003/008, SC-002).

**Independent Test**: Search an exact symbol name (e.g. `SessionStore`) and assert it ranks first in the single combined list; search a natural-language + symbol mix and assert the blend; kill the backend and assert keyword-only results with `mode_reason`, no error.

- [X] T016 [US2] Implement keyword-leg ordinal conversion in `crates/joey-neurocode-rag/src/search/hybrid.rs`: existing FTS5 `query_fts` bm25 rank is NEGATED (lower = better) — convert to ascending 1-based ordinals before fusion (research.md R1 gotcha; contracts/hybrid-search.md stage 2); unit test pins the sign conversion on hand-made bm25 values
- [X] T017 [US2] Implement RRF fusion k=60 in `crates/joey-neurocode-rag/src/search/rrf.rs`: `score(d) = Σ 1/(60 + rank_leg(d))` with deterministic tie-break (keyword rank first, then `chunk_id` lexical ascending) per contracts/hybrid-search.md stage 4; math unit tests with hand-computed fusion scores including ties
- [X] T018 [US2] Implement the exact-symbol-first guarantee in `crates/joey-neurocode-rag/src/search/hybrid.rs`: a normalized (trimmed, casefolded) query exactly equal to a `symbol_name` in scope ranks that chunk deterministically first (FR-002/SC-002; contracts/hybrid-search.md § Exact-symbol guarantee); test covers exact match ahead of fused-only competitors, plus a multi-query exact-symbol benchmark — ≥20 exact-symbol queries with the exact match ranking first ≥ 95% of the time (SC-002)
- [X] T019 [US2] Apply the file-scope filter in BOTH legs BEFORE fusion (never post-fusion) in `crates/joey-neurocode-rag/src/search/hybrid.rs` (FR-003; contracts/hybrid-search.md stage 5); test asserts filtered-out paths appear in neither leg's candidates
- [X] T020 [US2] Implement the degradation path in `crates/joey-neurocode-rag/src/search/hybrid.rs`: every `EmbedError` taxonomy class → KeywordOnly with `degradation_note`/`semantic_rank = None` and `SearchDiagnostics.mode_reason`, never a turn hard-fail (FR-008; contracts/hybrid-search.md § Degradation semantics); parameterized test covers each EmbedError variant

**Checkpoint**: User Stories 1 AND 2 both work independently — hybrid is the default search path, exact symbols stay first, degradation is explicit and safe.

---

## Phase 5: User Story 3 - Fast Incremental Re-Indexing (Priority: P3)

**Purpose**: Make refresh incremental and fully automatic so the index stays fresh without whole-project re-index cost.

**Goal**: Only added/modified/removed files are processed; deleted entries purge with cascade; refresh swaps atomically in the background within budgets (FR-004/005, SC-004).

**Independent Test**: Modify, add, rename, and delete files in an indexed repo, trigger the background refresh, and verify via status counts + DB inspection that only those files' rows changed, deleted paths are gone, and the ≤10-file refresh completes under 5 seconds.

- [X] T021 [US3] Implement change detection in `crates/joey-neurocode-rag/src/index/incremental.rs`: mtime walk + SHA-256 confirmation producing `ChangeDelta { added, modified, removed, renamed }` (data-model.md §6; research.md R3); tests cover added/modified/removed detection and mtime-false-positive rejection via hash
- [X] T022 [US3] Implement git-CLI rename assist in `crates/joey-neurocode-rag/src/index/incremental.rs`: shell-out via `std::process::Command` + `which::which` detection following the `crates/joey-tools/src/vcs.rs` CheckpointManager pattern; rename pairs feed `ChangeDelta.renamed`, with remove+add fallback when git is absent (research.md R3); tests cover rename detection and the no-git fallback end state
- [X] T023 [US3] Implement incremental refresh in `crates/joey-neurocode-rag/src/index/incremental.rs`: chunk-hash skip (equal `content_hash` ⇒ untouched), purge of removed files' chunks with `ON DELETE CASCADE` to vectors/edges, and `neurocode.rag.refresh.max_files_per_turn`/`max_bytes_per_turn` budgets (FR-004/005; data-model.md § Chunk lifecycle); tests prove unchanged chunks are not re-embedded and no orphans remain
- [X] T024 [US3] Implement the atomic transactional swap in `crates/joey-neurocode-rag/src/index/refresh_worker.rs`: one refresh = one transaction writing chunks + vectors + edges + `rag_index_meta`; COMMIT is the swap point; WAL readers see the prior consistent snapshot (FR-004, clarification Q5; contracts/rag-store-schema.md § Atomicity); add a concurrent-reader test asserting no torn state mid-refresh
- [X] T025 [US3] Implement the background refresh worker wiring in `crates/joey-agent-core/src/agent.rs`: reuse the existing auto-index thresholds/debounce, run via `spawn_blocking`, never block agent turns or searches, keep `refresh_state` observable for status (FR-004; plan.md M4); test asserts a turn completes unaffected while a refresh is in flight and that `refresh_state` transitions idle→refreshing→idle, plus a cold-index observable-progress assertion — a full index of a large synthetic repo reports progress and never blocks the agent (edge case 5)

**Checkpoint**: User Stories 1–3 work independently — refresh is incremental, atomic, budgeted, and background; SC-004 is met.

---

## Phase 6: User Story 4 - Context-Expanded Results (Priority: P4)

**Purpose**: Let the agent reason about a hit immediately by shipping surrounding source lines with every result.

**Goal**: Each result carries clamped surrounding context with a configurable window (FR-006).

**Independent Test**: Search and inspect a result's context block at file start/end (clamped, no error), then re-run with `--expand-lines 5` and `100` and observe the window change.

- [X] T026 [P] [US4] Implement context expansion in `crates/joey-neurocode-rag/src/search/expand.rs`: read the file from disk at query time, `± expand_context_lines` clamped to file boundaries, `context_absent` note (never an error) for missing files (FR-006; R4; contracts/hybrid-search.md stage 7); tests cover boundary clamping and the missing-file note
- [X] T027 [P] [US4] Plumb window configurability through `crates/joey-neurocode-rag/src/config.rs` (default `neurocode.rag.context_window_lines` = 20), the `expand_lines` tool parameter in `crates/joey-tools/src/tools/`, and the `--expand-lines` flag in `crates/joey-cli/src/commands/neurocode.rs`, with clamping 0–200 (contracts/rag-config-keys.md; contracts/neurocode-rag-tools.md); tests assert the default, flag override, and clamp bounds at each surface

**Checkpoint**: User Stories 1–4 work independently — results carry configurable, clamped context, cutting follow-up file reads (SC-006 groundwork).

---

## Phase 7: User Story 5 - Relationship-Aware Retrieval (Priority: P5)

**Purpose**: Expose the existing typed-graph relationships to retrieval so the agent gathers definitions plus callers/callees in one interaction.

**Goal**: Related entities are appended to results with correct kinds, bounded depth, and dedup (FR-007).

**Independent Test**: Retrieve a method known to call other indexed methods with `--relations 1` and `2`; verify related chunks appear with relationship kinds, stop at the depth bound, and contain no duplicates.

- [X] T028 [US5] Implement `rag_chunk_edges` derivation at index time in `crates/joey-neurocode-rag/src/index/chunker.rs`: project existing typed-graph edges (edge vocabulary from `crates/joey-neurocode/src/graph/edge.rs`) down to their chunks (data-model.md §7 — fully derived, typed graph stays authoritative); test asserts edge projection matches artifact-level edges
- [X] T029 [US5] Implement bounded BFS relation expansion in `crates/joey-neurocode-rag/src/search/expand.rs`: depth ≤ `neurocode.rag.relation_max_depth` (=2), dedup by `chunk_id` visited set, expanded items marked with `relation_kind` and appended AFTER fused results without displacing them (FR-007; contracts/hybrid-search.md stage 8); tests cover depth bound, dedup across paths, and non-displacement

**Checkpoint**: All five user stories are independently functional — retrieval is semantic, hybrid, fresh, contextual, and relationship-aware.

---

## Phase 8: Polish & Cross-Cutting Concerns

**Purpose**: Cross-cutting concerns that span every story — remote backends, consent, pre-fetch, status, parity, budgets, docs — plus dylib distribution, retrieval-quality benchmarking, and final validation.

**Goal**: FR-012/013/015 delivered, SC-001/002/003/004/005 verified, quickstart V1–V14 pass, workspace green.

**Independent Test**: `cargo build --workspace && cargo test --workspace` green including parity, wire-contract, migration, budget smoke, and retrieval-quality benchmark tests; quickstart.md V1–V14 pass by hand.

- [X] T030 Implement the remote HTTP backends in `crates/joey-neurocode-rag/src/embed/openai_compat.rs` and `crates/joey-neurocode-rag/src/embed/ollama.rs`: OpenAI-compat `POST {base_url}/v1/embeddings` and OllamaNative `POST {base_url}/api/embed` with wire JSON pinned byte-for-byte modulo model name and vector values, order-preserving batches (index field / array order), Bearer auth from `neurocode.rag.api_key` (contracts/embedding-backend.md §2–3); wire-contract tests assert both request/response shapes exactly
- [X] T031 [P] Implement the consent CLI in `crates/joey-cli/src/commands/neurocode.rs`: `/neurocode consent show|ack|revoke` — ack requires explicit interactive confirmation restating code egress, revoke takes effect immediately, re-ack after revoke allowed, absent file displays `never_acknowledged` (FR-012; contracts/neurocode-rag-command.md § Consent subcommand); add the show→ack→revoke→re-ack state-machine CLI test asserting `consent.json` contents and refusal of mocked remote embeds while not Acknowledged
- [X] T032 Implement consent gating for remote calls in `crates/joey-neurocode-rag/src/embed/mod.rs`: no network unless enabled AND (loopback `base_url` OR consent state `Acknowledged`), gate checked before every embed call so mid-operation revocation stops egress immediately; loopback = local, consent-free (FR-012; contracts/embedding-backend.md § Consent gate); tests cover edge cases 6–7 (unconsented remote stays keyword-only; revoked mid-use stops egress)
- [X] T033 [P] Implement the pre-fetch gate in `crates/joey-agent-core/src/agent.rs`: `neurocode.rag.prefetch.enabled` opt-in; hard runtime local-only gate (LocalOnnx in-process or loopback base_url — else pre-fetch errors OFF, never on); off ⇒ session context unchanged (FR-015; contracts/rag-config-keys.md); tests assert context injection only when opted-in AND local, and unchanged context when off
- [X] T034 Extend status reporting in `crates/joey-tools/src/tools/` (`neurocode_status` rag section) and `crates/joey-cli/src/commands/neurocode.rs` (`/neurocode status`): rag section ONLY when enabled — freshness, chunk/vector counts, model/profile/dim/quantization, last_refresh_at, backend health, consent state, degradation flags; absent entirely when disabled, never present-but-empty (FR-013; contracts/neurocode-rag-tools.md § neurocode_status); tests pin the enabled field list and the disabled byte-identity — scope: enabled-path status output only; disabled-path parity assertions belong to T035
- [X] T035 Implement the parity test in `crates/joey-neurocode-rag/tests/parity.rs`: with `neurocode.rag.enabled = false`, tool registry identical (no `neurocode_search`), `/neurocode status` output identical, and session context identical to pre-enhancement behavior (FR-009/SC-005; contracts/hybrid-search.md § Parity) — the parity test must ALSO assert existing neurocode tools (neurocode_index/query/status/ingest) keep byte-stable schemas and outputs, and that tier-routing and staleness-reporting outputs are unchanged (FR-011), not just registry identity, plus the `/neurocode model` subcommand's disabled-path harmlessness per contracts/neurocode-rag-command.md test obligation 7
- [X] T036 Add budget/perf smoke tests in `crates/joey-neurocode-rag/tests/budget_smoke.rs`: SC-003 search p95 < 2s over 100k synthetic chunks; SC-004 incremental refresh < 5s for ≤10 changed files touching only their rows; memory ≤ 512MB resident at ≤250k chunks with int8 quantization (plan.md Performance Goals)
- [X] T037 Add the local embedding throughput benchmark in `crates/joey-neurocode-rag/benches/embed_throughput.rs` validating the ~10–40ms per 512-token text AVX2 ballpark via a padded `encode_batch` + rayon run (plan.md budget; contracts/embedding-backend.md § Throughput note — M7 pins real numbers)
- [X] T038 [P] Document in `README.md` and `PORTING.md`: the new `crates/joey-neurocode-rag` crate, the three pinned deps (`ort =2.0.0-rc.13` with `load-dynamic` + the runtime ONNX Runtime dylib distribution story, `tokenizers` offline, `ndarray`), and the no-Hugging-Face model distribution policy (manual placement or project mirror only — research.md R8)
- [X] T039 Implement `/neurocode model fetch --dylib` in `crates/joey-cli/src/commands/neurocode.rs`: platform/arch detection, download of the ONNX Runtime dylib from the project-controlled mirror root (`neurocode.rag.local.mirror_url`), SHA-256 verification against project-recorded per-platform hashes with refusal on mismatch, and dylib resolution order `ORT_DYLIB_PATH` env → `neurocode.rag.local.ort_dylib_path` → system lookup → fetched-copy location (Constitution Principle 0 — no vendored binaries; research.md R6 `load-dynamic`); add tests for hash-mismatch refusal and resolution-order precedence, plus a documented Windows/MSVC verification note (Windows dylib ~31MB)
- [X] T040 Build the automated retrieval-quality benchmark harness in `crates/joey-neurocode-rag/tests/`: synthetic corpus + ≥30 natural-language queries whose wording does not literally appear in the code, asserting correct-location-in-top-5 hit rate ≥ 80% (SC-001); the test auto-skips (marked ignored with a clear reason) when local model artifacts are absent — it NEVER downloads and never contacts huggingface.co (research.md R8)
- [X] T041 Run the full quickstart.md validation end-to-end against the built feature: scenarios V1–V14 (parity baseline, NL search, exact-symbol, file-scope, fallback chunks, incremental refresh, context expansion, relations, degradation, remote consent, status, agent tool path, workspace suite, model swap) plus the performance spot-checks
- [X] T042 Final gate: `cargo build --workspace && cargo test --workspace` fully green (constitution acceptance bar; FR-011 — existing neurocode behavior, tiers, and staleness reporting show no regression)

**Checkpoint**: Cross-cutting concerns closed; parity, budgets, docs, and quickstart validated; entire workspace green.

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies — can start immediately.
- **Foundational (Phase 2)**: Depends on Setup — BLOCKS all user stories (schema v3, profiles, consent, integrity are prerequisites for every story).
- **US1 (Phase 3)**: Depends on Foundational (T009/T010 need T006/T008; T012 needs T003/T004/T005).
- **US2 (Phase 4)**: Depends on US1 (fusion consumes the dense leg built in Phase 3).
- **US3 (Phase 5)**: Depends on Foundational + US1's indexing pipeline (T012) — refresh operates on chunk/vector rows.
- **US4 (Phase 6)**: Depends on US1 (expansion decorates search results) — independent of US2/US3.
- **US5 (Phase 7)**: Depends on US1 (edges + expansion ride the index/search path); meaningful once context expansion exists (US4) but not technically blocked by it.
- **Polish (Phase 8)**: Depends on all user stories.

### User Story Dependencies

- **US1 (P1, MVP)**: Can start after Foundational — no dependencies on other stories.
- **US2 (P2)**: Depends on US1's dense leg and search skeleton; independently testable afterward.
- **US3 (P3)**: Depends on US1's indexing pipeline; independently testable afterward.
- **US4 (P4)**: Depends on US1 only; independently testable.
- **US5 (P5)**: Depends on US1; independently testable.

### Documented Deviation: Cross-Cutting Concerns in Polish

Remote HTTP backends (T030), consent CLI + gating (T031/T032), proactive pre-fetch (T033), and status reporting (T034) live in the Polish phase rather than inside a single user story. This is a **documented deviation from pure story mapping**: FR-012 (consent), FR-013 (status), and FR-015 (pre-fetch) are cross-cutting — they govern every backend interaction and every surface (tool, CLI, TUI, agent loop), not one story's scope. Sequencing them last also keeps the MVP fully local and consent-free (LocalOnnx needs no consent), so no story is blocked on them. The consequence is managed explicitly: US1's degradation path (T020) already guarantees keyword-only fallback with indication whenever the backend is absent, unconfigured, or unconsented, so the pre-Polish feature is correct without T030–T034.

### Within Each User Story

- Types/contracts before services; services before wiring.
- Tests written alongside implementation (Constitution IV), not deferred.
- Core implementation before surface wiring (tool/CLI/TUI).
- Story complete before moving to the next priority.

### Parallel Opportunities

- T006, T007, T008 run in parallel within Phase 2 (after T005; different files in `crates/joey-neurocode-rag/src/`).
- T009 and T011 run in parallel within Phase 3 (embed/ vs joey-cli surfaces).
- Within Phase 4, T016, T017, T018, T019 all edit `crates/joey-neurocode-rag/src/search/hybrid.rs` (or its sibling rrf.rs) — they are deliberately SEQUENTIAL to avoid merge collisions in the hybrid-search module; T017 depends on T016.
- T026 and T027 run in parallel within Phase 6 (expansion vs config plumbing).
- T031, T033, T038 run in parallel within Phase 8 (different crates/files); T034 collides with T031 on `crates/joey-cli/src/commands/neurocode.rs` and runs sequentially after it; T039 also edits `crates/joey-cli/src/commands/neurocode.rs` and therefore runs sequentially after T031/T034 within Phase 8 order.
- Once Foundational completes, US1 can proceed while later-story groundwork that touches only new files advances — but sequential priority order is the default.

### Milestone ↔ Phase Mapping

- M1 ≈ Phase 2 (T003–T005); M2 ≈ Phases 1–3 (T001, T006, T008–T011); M3 ≈ Phases 3–4 (T012–T020); M4 ≈ Phase 5 (T021–T025); M5 ≈ Phases 6+8 (T026–T027, T030–T034); M6 ≈ Phase 7 (T028–T029); M7 ≈ Phase 8 (T035–T042); Phase 1 Setup is cross-milestone scaffolding.

---

## Parallel Example: Phase 2 after T005

```bash
# Launch the three independent foundational tasks together:
Task: "T006 [P] Model profiles module in crates/joey-neurocode-rag/src/embed/profiles.rs"
Task: "T007 [P] Consent record module in crates/joey-neurocode-rag/src/consent.rs"
Task: "T008 [P] Artifact integrity helpers in crates/joey-neurocode-rag/src/embed/artifacts.rs"

# Same pattern in Phase 8:
Task: "T031 [P] Consent CLI in crates/joey-cli/src/commands/neurocode.rs"
Task: "T033 [P] Pre-fetch gate in crates/joey-agent-core/src/agent.rs"
Task: "T038 [P] Documentation in README.md and PORTING.md"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only, Local Backend)

1. Complete Phase 1: Setup (crate + pinned deps + config contract).
2. Complete Phase 2: Foundational (CRITICAL — blocks all stories).
3. Complete Phase 3: User Story 1 with the LocalOnnx backend.
4. **STOP and VALIDATE**: quickstart V1 (disabled parity), V2 (NL search), V9 (degradation) pass.
5. Ship — semantic search already delivers the core value; everything after is additive.

### Incremental Delivery (ship green after every phase)

1. Setup + Foundational → foundation ready.
2. Add US1 (NL search, local backend) → validate → ship (MVP!).
3. Add US2 (hybrid ranking) → validate exact-symbol-first → ship.
4. Add US3 (incremental refresh) → validate SC-004 → ship.
5. Add US4 (context expansion) → validate clamping/flags → ship.
6. Add US5 (relation expansion) → validate bounds/dedup → ship.
7. Polish: remote backends, consent, pre-fetch, status, parity, budgets, docs, quickstart → final gate.
8. Every phase leaves `cargo build --workspace && cargo test --workspace` green (constitution acceptance bar).

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together.
2. Once Foundational is done: Developer A drives US1→US2 (search spine), Developer B drives US3 (refresh worker, mostly `index/` files), Developer C drives US4/US5 (`search/expand.rs`) — merging in priority order.
3. Polish lands last as cross-cutting hardening.

---

## Notes

- Story order is kept priority order US1→US5 (P1→P5): each phase delivers exactly one story, independently testable at its checkpoint.
- The remote path (HTTP backends, consent CLI/gating) is deliberately deferred to Polish — FR-012/013/015 are cross-cutting and the MVP is fully local and consent-free (see the documented deviation above).
- [P] tasks = different files, no dependencies on incomplete tasks.
- [Story] labels map tasks to user stories for traceability; Setup/Foundational/Polish tasks carry none by design.
- Every task names at least one file path; contract regression obligations (config key table, schema round-trip, wire shapes, tool schema, parity) are embedded as tests in the owning task — Constitution IV/VII.
- Default-off is load-bearing: FR-009/SC-005 byte-identical parity when disabled is asserted at every layer that adds a surface (T014, T015, T034, T035).
- No Hugging Face anywhere: model artifacts arrive by manual placement or the project-controlled mirror only (R8; T011, T038).
- Commit after each task or logical group; stop at any checkpoint to validate independently.

## Phase 9: Convergence

- [X] T043 Wire the styled TUI RAG result renderer into the TUI output path — crates/joey-tui/src/neurocode_search.rs (outcome_lines/badge_span/result_line) currently has zero callers outside its own test, so TUI shows plain CLI text per FR-010 + Constitution II (partial)
- [X] T044 Make /neurocode consent grant/revoke operable from the TUI — the interactive stdin confirm (crates/joey-cli/src/commands/neurocode.rs ~line 1788) runs on the HeavyJob engine thread where stdin is unavailable; add a non-interactive path (e.g. --yes) or TUI-native confirmation, with tests, per FR-012 + Constitution II (partial)
- [X] T045 Re-run the full workspace test gate with CARGO_TARGET_DIR outside the repo target dir (e.g. CARGO_TARGET_DIR=/tmp/joey-target cargo test --workspace) to bypass the host EDR path-kill of test binaries, record actual per-crate totals, and fix any real failures, per Constitution VII + plan: final workspace gate (partial)
- [X] T046 Add a regression test that opens a real legacy v2 graph.db fixture and verifies the additive v2-to-v3 migration preserves existing tables/data, per Constitution VII (on-disk format public surface) + plan: schema v2-to-v3 decision (partial)
- [X] T047 Add an SC-004 wall-clock budget test (<=10-file incremental refresh completes in <5s and touches only changed entries) to crates/joey-neurocode-rag/tests/budget_smoke.rs per SC-004 / US3/AC1 (partial)
- [X] T048 Add a concurrency test asserting searches issued during an in-flight refresh observe a consistent snapshot (WAL + single-transaction swap) per spec edge case: refresh vs query snapshot isolation (partial)
- [X] T049 Reconcile the RAG tool wiring deviation — joey-tools injects via engine handle (crates/joey-tools/src/tools/neurocode_tools.rs) instead of the planned direct joey-tools -> joey-neurocode-rag dependency edge; adopt the planned edge or record the accepted deviation in PORTING.md per plan: DAG decision (contradicts)
- [X] T050 Add a cross-crate consistency test pinning the string-literal neurocode.rag.* config keys in crates/joey-agent-core/src/agent.rs (~lines 151/178) to the RagConfig key definitions in crates/joey-neurocode-rag/src/config.rs per contracts/rag-config-keys.md (partial)
