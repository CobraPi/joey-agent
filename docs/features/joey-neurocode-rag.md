# joey-neurocode-rag — local-first semantic RAG over the code graph

`joey-neurocode-rag` (spec 021, `specs/021-please-enhance-neurocode`) adds
local-first semantic code retrieval on top of the existing NeuroCode typed
code graph (see [joey-neurocode.md](joey-neurocode.md)): natural-language code
search, hybrid dense+BM25 ranking fused client-side via Reciprocal Rank
Fusion, fast incremental background re-indexing with atomic snapshot swaps,
context-expanded results, and relationship-aware retrieval. The semantic
engine is a fully-local, in-process ONNX embedder (`ort` with a
runtime-loaded ONNX Runtime dylib; no daemon, no network by default), with
optional OpenAI-compatible, Ollama, and GitHub Copilot HTTP backends behind
per-project recorded, revocable consent. The whole feature is default-off
(`neurocode.rag.enabled = false`) and byte-identical to pre-enhancement
behavior while disabled (FR-009/SC-005).

> See also: [joey-neurocode.md](joey-neurocode.md) — the structural graph,
> classifier, and verify loop this crate builds upon.

## Overview

- **Spec 021**; all data lives inside `joey-neurocode`'s `graph.db` (schema
  v3 tables `rag_chunks`, `rag_vectors`, `rag_index_meta`,
  `rag_model_artifacts`, `rag_chunk_edges`).
- **Pinned dependencies** (recorded in `Cargo.toml` / research.md R6):
  `ort =2.0.0-rc.13` (exact pin — pre-release with per-RC breaking changes)
  with `default-features = false` and the `ndarray` + `load-dynamic`
  features — the ONNX Runtime dylib (~10–31 MB per platform) is loaded at
  runtime, never embedded or vendored; `tokenizers 0.23`
  (`default-features = false`, `fancy-regex` — pure-Rust, avoids the onig C
  lib, and NO hf-hub: the crate never fetches); `ndarray 0.17` (tensor
  interop matching `ort` rc.13).
- Local-first: the primary backend runs in-process with no network; remote
  backends exist only behind explicit consent (below) and every error class
  degrades to keyword-only search, never a turn hard-fail (FR-008).

## Module map

35 files total: 23 `src` files, 10 integration tests, 1 bench, plus
`Cargo.toml`. `src` layout: `lib.rs`, `config.rs`, `consent.rs`, `parity.rs`;
`embed/{mod,profiles,artifacts,local_onnx,openai_compat,ollama,copilot}.rs`;
`index/{mod,chunker,incremental,refresh_worker}.rs`;
`search/{mod,hybrid,rrf,expand}.rs`; `vector/{mod,store,scan,quantize}.rs`.
Tests: `config_keys.rs`, `profiles.rs`, `v2_migration.rs`, `dense_index.rs`,
`chunk_edges.rs`, `retrieval_quality.rs`, `snapshot_isolation.rs`,
`budget_smoke.rs`, `local_model_smoke.rs`, `parity.rs`. Bench:
`benches/embed_throughput.rs`.

| Module | Role |
|---|---|
| `config` | all 18 `neurocode.rag.*` keys (T002) + the Copilot extension key |
| `consent` | per-project `consent.json` state machine (T007) |
| `parity` | byte-identical-when-disabled guard (FR-009/SC-005; test in `tests/parity.rs`) |
| `embed::profiles` | model profile table (name, dim, ctx, pooling, prefixes, license) |
| `embed::artifacts` | SHA-256 artifact integrity gate for `model_dir` |
| `embed::local_onnx` | PRIMARY in-process ONNX embedder |
| `embed::openai_compat` | `POST {base_url}/v1/embeddings` (T030) |
| `embed::ollama` | `POST {base_url}/api/embed` native (T030) |
| `embed::copilot` | GitHub Copilot `POST {base}/embeddings`, explicit-backend-only (Joey-native extension) |
| `embed` (mod) | `EmbeddingBackend` trait, `BackendKind` registry, `EmbedError` taxonomy, `auto` resolution, consent gate |
| `index::chunker` | symbol-aligned + fallback coarse chunks, `content_hash`, derived chunk edges (T012/T028) |
| `index::incremental` | mtime+SHA-256 change detection, git rename assist, budgeted refresh (T021–T023) |
| `index::refresh_worker` | atomic transactional snapshot swap, `idle ↔ refreshing` flag (T024) |
| `search::hybrid` | dense leg + pipeline + degradation mapping (T013+) |
| `search::rrf` | Reciprocal Rank Fusion, k = 60 (T017) |
| `search::expand` | context-window expansion + bounded BFS relations (T026/T029) |
| `vector::store` | `rag_chunks`/`rag_vectors`/`rag_index_meta` read/write |
| `vector::scan` | exhaustive rayon cosine scan, both BLOB encodings (T013) |
| `vector::quantize` | f32 / int8 BLOB codec — single source of the layout |

## Configuration

The pinned 18-key contract table (`RAG_CONFIG_KEYS`,
`contracts/rag-config-keys.md`) plus the Joey-native Copilot extension key:

| Key | Default | Kind / notes |
|---|---|---|
| `neurocode.rag.enabled` | `false` | master switch; `false` = byte-identical parity |
| `neurocode.rag.backend` | `auto` | enum `auto \| local_onnx \| openai_compat \| ollama \| copilot`; unknown → warning + `auto` |
| `neurocode.rag.base_url` | `http://localhost:11434` | base for the HTTP backends |
| `neurocode.rag.model` | `nomic-embed-text-v1.5` | model profile name |
| `neurocode.rag.api_key` | `""` | Bearer token; WRITES route to `.env` as `JOEY_NEUROCODE_RAG_API_KEY` (env wins on read; dotted getter is fallback) |
| `neurocode.rag.local.model_dir` | `~/.joey/neurocode/models/<profile>/` | path, `~` expanded at load; default resolves through `joey_home()` so `-p/--profile` scoping is honored |
| `neurocode.rag.local.mirror_url` | `""` | mirror for `/neurocode model fetch`; empty = fetch disabled |
| `neurocode.rag.local.ort_dylib_path` | `""` | ONNX Runtime dylib; empty = `ORT_DYLIB_PATH` env → system lookup |
| `neurocode.rag.batch_size` | `64` | validated range 16–128; out-of-range → warning + default 64 |
| `neurocode.rag.top_k` | `10` | default hybrid result limit |
| `neurocode.rag.context_window_lines` | `20` | ± context lines per result; clamped 0–200 |
| `neurocode.rag.relation_max_depth` | `2` | max BFS depth for relation expansion; clamped 0–2 |
| `neurocode.rag.include_fallback_chunks` | `true` | FallbackCoarse chunks participate in ranking (FR-014) |
| `neurocode.rag.quantize_threshold` | `100000` | chunk count above which int8 storage applies (strictly above) |
| `neurocode.rag.prefetch.enabled` | `false` | proactive pre-fetch into agent context (FR-015) |
| `neurocode.rag.refresh.max_files_per_turn` | `50` | refresh budget (FR-004) |
| `neurocode.rag.refresh.max_bytes_per_turn` | `52428800` | refresh budget, 50 MiB (FR-004) |
| `neurocode.rag.timeout_secs` | `30` | HTTP timeout for embedding calls + model-fetch downloads |
| `neurocode.rag.copilot.model` | `"metis-1024-I16-Binary"` | Copilot wire model id — extension, NOT part of the pinned 18 |

Invalid values never crash — warn + fallback. Changes take effect at the next
refresh/turn; no hot-reload contract. Unknown `neurocode.rag.*` keys follow
the existing config-layer behavior verbatim.

## Backends

`EmbeddingBackend` trait: `embed` (batch, order-preserving, normalized
vectors, RAW text in — prefixes are the embedder's job),
`describe_embedder` (static, no I/O), `health_check`. Implementations are
keyed by `BackendKind` (`local_onnx`, `openai_compat`, `ollama`, `copilot`,
`keyword_only`):

| Backend | Wire / mechanism | Notes |
|---|---|---|
| `LocalOnnx` | in-process `ort` inference; dylib located via `neurocode.rag.local.ort_dylib_path` → `ORT_DYLIB_PATH` → system | PRIMARY; fully local, consent-free; offline `tokenizer.json` loading (padded `encode_batch`), never downloads; artifacts pass the SHA-256 integrity gate before any ort work |
| `OpenAiCompat` | `POST {base_url}/v1/embeddings`, Bearer when api_key non-empty | covers OpenAI, Voyage, Ollama's `/v1` compat endpoint with zero per-vendor code; order restored via the response `index` field; client-side L2 |
| `Ollama` | `POST {base_url}/api/embed` | native truncate/keep_alive control (`truncate: true`, `keep_alive` default `"5m"`); positional array order; a loopback `base_url` counts as LOCAL (consent-free) |
| `Copilot` | `POST {base}/embeddings` (no `/v1` prefix) | selected ONLY by explicit `neurocode.rag.backend = copilot`; independent of the LLM provider (works with any chat provider). Auth uses `joey_providers::copilot::CopilotAuth` token-exchange machinery with credentials resolved independently of the chat provider: `neurocode.rag.api_key`, else `COPILOT_GITHUB_TOKEN`/`GH_TOKEN`/`GITHUB_TOKEN`, else `gh auth token`; api.github.com dotcom serves the native Metis family (`metis-1024-I16-Binary`, raw credential, GitHub-native body `inputs`/`input_type`/`embedding_model`); api.githubcopilot.com CAPI fallback serves `text-embedding-3-small` (OpenAI-style body, `input` as JSON array — string inputs are rejected 400); a pinned custom endpoint skips the token exchange like chat traffic. Input caps: 23,000 B per input, 850,000 B per request, 64 items per sub-request |
| `KeywordOnly` | degradation marker | `auto` resolution with no verifiable local artifacts — the pipeline runs its FTS5 keyword leg only, with an explicit FR-008 indication; resolving here never involves a network call |

`auto` resolution probes the local `model_dir` artifact gate ONLY (no dylib,
no session, no network): artifacts verify → `local_onnx`; missing → keyword-only
degradation. `local_onnx` explicit with missing artifacts is a hard error, no
silent degradation. Explicit backend values always win over `auto` wiring.

**`EmbedError` taxonomy** (structural, no string matching): local classes
`MissingArtifacts`, `CorruptArtifacts`, `DylibLoad`, `SessionLoad`,
`Inference`, `DimensionMismatch`, `EmptyResult`; remote classes
`Unreachable` (connect/timeout/DNS), `AuthRejected` (HTTP 401/403),
`RateLimited` (429), `MalformedResponse` (shape/count violations); consent
gate refusal `Consent`; plus `RemoteBackendsLandLater`, `UnknownProfile`,
`Other`. Every class maps to keyword-only degradation, never a turn
hard-fail.

## Embedding profiles

Pooling and prefixes live in the per-model PROFILE table, not in backend
code, so swapping models cannot silently corrupt the index. Rule 1: prefixes
are applied BEFORE tokenization (`query_input`/`document_input` = prefix ++
raw text, verbatim). Rule 2: pooling must match the profile (mean pooling +
client-side L2 for every accepted profile). Profile identity
(name + dim + pooling) is persisted in `rag_index_meta`; a mismatch on load
triggers a full semantic rebuild.

| Profile | Dim | Ctx | Pooling | Prefixes | License |
|---|---|---|---|---|---|
| `nomic-embed-text-v1.5` (default) | 768 | 8192 | mean + L2 (client-side) | `search_query: ` / `search_document: ` | Apache-2.0 |
| `CodeRankEmbed` | 768 | 8192 | mean + L2 (client-side) | query `Represent this query for searching relevant code: `; document prefix EMPTY (load-bearing) | MIT |
| `text-embedding-3-small` | 1536 | 8192 | server-side; L2 client-side after decode | none (instruction-free; prefixes MUST NOT be prepended) | Proprietary — GitHub Copilot subscription |
| `metis-1024-I16-Binary` | 1024 | 8192 | server-side; L2 client-side | none | Proprietary — GitHub Copilot subscription |

Rejected candidates are recorded so the choice is never re-litigated:
`nomic-embed-code` (7B Qwen2.5 decoder, ~28 GB f32, 3584-dim, last-token
pooling — resource-incompatible; see `REJECTED_CANDIDATES`).

## Consent model

Remote embedding egress is gated per project. The record lives beside
`graph.db` — deliberately NOT inside it — at
`~/.joey/neurocode/projects/<sha256-of-root>/consent.json`, human-inspectable
and decoupled from index rebuilds.

State machine (`ConsentState`, snake_case on disk):

```text
NeverAcknowledged ──(explicit CLI ack)──▶ Acknowledged
Acknowledged      ──(revoke, any time)──▶ Revoked
Revoked           ──(re-ack allowed)────▶ Acknowledged
```

An absent file ≡ `NeverAcknowledged`. Invariant (FR-012): only `Acknowledged`
permits remote embedding calls; `NeverAcknowledged`, `Revoked`, or a missing
file forces local/keyword-only operation. The gate
(`remote_egress_permitted`) allows a remote call only when
`neurocode.rag.enabled` AND (loopback `base_url` OR consent `Acknowledged`),
re-checked before EVERY embed call — mid-operation revocation stops egress on
the next call, immediately. Local backends (`LocalOnnx`, loopback Ollama) are
consent-free. The record carries audit fields: `project_root`,
`remote_backend_url`, `acknowledged_at`, `revoked_at`, `model_at_ack_time`.

## Indexing pipeline

- **Chunker** (`index::chunker`): symbol-aligned chunks from the parse
  layer's extracted artifacts (with `artifact_id` FK → `code_artifacts.id`
  when the artifact exists in the typed graph) plus fallback coarse chunks
  for regions with no named artifacts. A contextual prefix (file path +
  imports, capped at 30 imports) is prepended to the chunk body; chunks
  longer than 200 lines (`DEFAULT_MAX_CHUNK_LINES`) are split. `chunk_id` is
  deterministic: `source_path + start_line + end_line + kind discriminator
  (+ symbol name)` — a moved chunk gets a new id; ids are never mutated.
- **`content_hash`**: SHA-256 hex over the chunk's RAW UNPREFIXED text — the
  constructed contextual text WITHOUT the profile's document prefix (applied
  at embed time only, never stored, never hashed). A profile change never
  invalidates chunk hashes; only embeddings rebuild.
- **Derived chunk edges** (`rag_chunk_edges`): the typed graph's
  artifact-level `graph_edges` are projected down to their chunks at index
  time, inside the same transaction as the chunk rows. Fully derived and
  rebuildable — the typed graph stays authoritative; no FK by design.
- **Incremental detection** (`index::incremental`): an mtime walk produces
  candidates; SHA-256 content hashing confirms them (`FileFingerprint`).
  When mtime and content disagree, the hash is the authority — a
  touched-but-identical file is NOT modified; with `trust_mtime` off, an
  mtime-spoofed edit IS caught. Output is a `ChangeDelta`
  (`added`/`modified`/`removed`/`renamed`). Opportunistic **git rename
  assist** shells out to the git CLI (`which` detection, explicit
  `GIT_DIR`/`GIT_WORK_TREE`, config isolation, wall-clock timeout); without
  git, the same content-hash pairing applies, and anything unmatched
  degrades to remove+add — an equivalent end state. Refresh honors the
  `max_files_per_turn` / `max_bytes_per_turn` budgets with skipped work
  reported; equal `content_hash` chunks are never re-embedded.
- **Refresh worker** (`index::refresh_worker`): one refresh = ONE data
  transaction writing chunks + vectors + edges + `rag_index_meta`; COMMIT is
  the atomic snapshot swap — WAL readers keep seeing the prior fully
  consistent snapshot, so no query ever observes torn state. The
  `refresh_state` flag flips `idle → refreshing` in its own small committed
  transaction BEFORE the heavy work (observable status for concurrent
  readers) and back `refreshing → idle` afterwards — on the error path too,
  so the flag never stays stuck. A fingerprint sidecar is rewritten from
  store truth each pass, healing drift. Plain sync `fn`; the agent spawns it
  via `spawn_blocking`. Single-writer per project DB.

## Search

Hybrid pipeline (`search::hybrid`, contracts/hybrid-search.md):

1. **Dense leg**: the raw query is prefixed with the profile's QUERY prefix,
   embedded, then scanned exhaustively against `rag_vectors` (joined to
   `rag_chunks`) — cosine similarity as a plain dot product on normalized
   vectors, rayon-parallel across rows, deterministic top-k, BLOB decode
   validation for both encodings. `include_fallback_chunks` filters whether
   coarse chunks participate.
2. **Keyword leg**: the existing FTS5 BM25 search over the structural store
   (negated rank → ascending ordinals).
3. **RRF fusion** (`search::rrf`): `score(d) = Σ_legs 1/(60 + rank_leg(d))`
   with `k = 60` (`RRF_K`); ranks are ascending 1-based ordinals per leg,
   never raw bm25 or cosine values. Deterministic tie-break: fused score
   descending (`total_cmp`), then keyword rank ascending (`None` last), then
   `chunk_id` lexical ascending.
4. **Context expansion** (`search::expand`): the chunk's file is read from
   disk at query time; `± context_window_lines` around the 1-based inclusive
   span, clamped to file boundaries; a missing file yields a
   `context_absent` note, never an error.
5. **Relation expansion**: bounded BFS over `rag_chunk_edges` to depth ≤
   `relation_max_depth` (hard cap 2), deduped by `chunk_id` (visited set),
   outgoing edges only (inverse kinds exist as their own rows), neighbors
   visited in `(to_chunk_id, edge_kind)` order for determinism. Expanded
   items ride the per-result `relations` field appended AFTER fused results
   — they never displace, reorder, or rescore the fused list.
6. **Degradation**: every `EmbedError` class maps to keyword-only mode with a
   `mode_reason` — the turn never hard-fails (FR-008). The exact-symbol-first
   guarantee and file-scope filters apply in both legs before fusion.

## Vector store

Vectors are SQLite BLOBs in `rag_vectors`, written in the SAME transaction as
their chunks (all-or-nothing; a mid-batch failure leaves no partial rows).
Purging a chunk cascades to its vector via `ON DELETE CASCADE` — no orphan
vectors (FR-005).

| Encoding | Layout | Byte length |
|---|---|---|
| `f32` | `dim` little-endian IEEE-754 words | `dim × 4` |
| `int8` | one f32 scale prefix (4 bytes LE) + `dim` int8 codes (value ≈ code × scale) | `dim + 4` |

Anything whose byte length differs is corrupt and rejected (chunk-naming
error). int8 is symmetric over `[-127, 127]` (`scale = max|v| / 127`); a
zero vector encodes with scale 0 and all-zero codes. The codec lives only in
`vector::quantize`; the scan imports it — the layout has exactly one
implementation site. Quantization is an index-time decision: int8 applies
strictly ABOVE `quantize_threshold` chunks (default 100000: ~400 MB →
~100 MB resident); a store mid-reindex may legitimately contain a mix, so
the scan decodes both formats per row.

## Parity & benches

- `src/parity.rs` + `tests/parity.rs` — the FR-009/SC-005 guard: with
  `neurocode.rag.enabled = false`, tool registry, `/neurocode status` output,
  and session context must be byte-identical to pre-enhancement behavior.
- `tests/config_keys.rs` — pins the 18-key contract surface against silent
  additions, renames, or default drift.
- `tests/profiles.rs` — profile table integrity (unique names, no rejected
  candidate leaks, default resolution).
- `tests/v2_migration.rs` — v2 → v3 additive migration of existing DBs.
- `tests/dense_index.rs` — dense write + scan round trips.
- `tests/chunk_edges.rs` — derived chunk-edge projection.
- `tests/retrieval_quality.rs` — end-to-end hybrid retrieval quality.
- `tests/snapshot_isolation.rs` — atomic snapshot swap semantics.
- `tests/budget_smoke.rs` — refresh budget enforcement.
- `tests/local_model_smoke.rs` — real-graph input-surface matrix (skips
  cleanly without artifacts).
- `benches/embed_throughput.rs` — 3 benches validating the ~10–40 ms per
  512-token text AVX2 ballpark of `LocalOnnx` (end-to-end documents, pure
  pooling path, skip path). AUTO-SKIPS with a printed reason when no verified
  `model.onnx` + `tokenizer.json` set is present (probed via
  `$JOEY_RAG_MODEL_DIR` or the config default); it NEVER downloads artifacts
  and NEVER contacts the network. Run:
  `cargo test -p joey-neurocode-rag --benches -- --ignored --nocapture`.

## See also

- [joey-neurocode.md](joey-neurocode.md) — the structural graph this builds on
- [joey-tools.md](joey-tools.md) — the RAG-gated `neurocode_search` tool
- [joey-copilot.md](joey-copilot.md) — Copilot surface this crate's
  embeddings backend plugs into
- [joey-providers.md](joey-providers.md) — `CopilotAuth` token exchange
- [joey-core.md](joey-core.md) — config layers, `.env` routing, `joey_home()`
- [README.md](README.md) — features index
