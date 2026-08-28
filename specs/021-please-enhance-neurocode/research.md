# Research: NeuroCode Semantic Code Retrieval (RAG Enhancement)

Date: 2026-08-27 — second revision: local-ONNX primary embedding backend per user direction; Hugging Face distribution banned (see R2, R6, R8).
Branch: 021-please-enhance-neurocode
Purpose: resolves all Technical Context unknowns for plan.md. All findings below follow the Decision / Rationale / Alternatives-considered format required by the constitution (.specify/memory/constitution.md v1.1.0, Principle VIII + Additional Constraints): every dependency choice is justified against alternatives with binary-size/compile-time weight recorded.

## R1: Vector store & hybrid search backend

### Decision
Embedded vector storage inside the existing per-project SQLite `graph.db`:

- Dense vectors stored as BLOB rows in a new table.
- Exhaustive (brute-force) cosine scan implemented in Rust with rayon parallelism.
- int8 scalar quantization optionally enabled above ~100k chunks.
- Hybrid ranking done **client-side** via Reciprocal Rank Fusion (RRF, k=60) of dense ranks and existing FTS5 BM25 keyword ranks.

### Rationale
Measured on this machine: numpy cosine top-20 over 100k×1024 f32 = ~3 ms; 500k×1024 = ~16 ms — compute is never the bottleneck; BLOB load is (410 MB cold ≈ 1–2 s NVMe, ~0.3 s warm). f32 in-memory at 100k chunks ≈ 400 MB; int8 quantization cuts this to ~100 MB (¼ size, ~4× faster load, minor recall loss) — quantization becomes the default above a configurable chunk-count threshold.

Zero external server, zero new dependencies (rusqlite bundled + rayon are already workspace deps).

FTS5 gotcha documented: FTS5 bm25 rank is NEGATED (lower = better; sqlite.org/fts5.html) — convert to 1-based ascending ordinal ranks before RRF: `score = Σ 1/(60 + rank_i)`.

### Alternatives considered
1. **qdrant-client v1.19.0** — first-class sparse vectors + server-side prefetch/RRF (qdrant.tech/documentation/concepts/hybrid-queries/), but REQUIRES a running Qdrant server (no embedded mode in Rust client), imposes tonic/prost/gRPC compile-time and binary weight, and server ops burden on CLI users — violates zero-ops local default. REJECTED.
2. **sqlite-vec v0.1.10-alpha.4** — pure-C, statically linked via cc, registers with rusqlite via sqlite3_auto_extension, Windows builds shipped; but self-declared pre-v1 with breaking changes expected, and its vec0 KNN is also brute-force (same perf class as DIY BLOB scan). REJECTED for now; recorded as a future swap-in if it stabilizes.
3. **usearch 2.26.1** — mature ANN but C++ via cxx/cxx-build requiring C++ toolchain incl. MSVC on Windows; conflicts with cross-platform lean-build posture (constitution Principle 0/VIII). REJECTED.
4. **ANN pure-Rust crates** — hnsw_rs 0.3.4 (maintained, pure Rust): viable UPGRADE PATH if exhaustive scan ever exceeds budget at >500k chunks. instant-distance 0.6.1 (stale 2023), hora 0.1.1 (abandoned 2021), tiny_hnsw 1.8.0 (166 downloads) — rejected.
5. **Doing nothing semantic (FTS5 only)** — fails FR-001/SC-001. REJECTED.

Consistency with repo philosophy: README.md already documents "SQLite + FTS5 instead of a vector DB" as a deliberate deviation; embedded-SQLite vectors extends, not contradicts, this stance.

## R2: Embedding backend

### Decision
The backend trait is UNCHANGED (async, batch-oriented: `embed(batch: &[String]) -> Vec` of f32 vectors) but now has a two-tier implementation set, selected by a `BackendKind` enum — `LocalOnnx | OpenAiCompat | OllamaNative`:

- **PRIMARY: fully-local, in-process ONNX inference** (`LocalOnnx`) — model + tokenizer loaded from disk (R8), run via `ort` in-process. No daemon, no network.
- **SECONDARY (optional): HTTP backends retained for remote/daemon use under the existing consent model** — an OpenAI-compatible HTTPS JSON client (`OpenAiCompat`) parameterized by `base_url` + optional API key, covering OpenAI, Voyage (api.voyageai.com/v1/embeddings), and Ollama's `/v1/embeddings` compatibility endpoint with zero per-vendor code; plus a native Ollama `/api/embed` client (`OllamaNative`) as a thin second impl for truncate/keep_alive control. Both reuse the workspace's existing reqwest 0.12 (rustls-tls, json) — no new HTTP stack. Caller-side default batch 64; the `neurocode.rag.batch_size` config key validates 16–128 (authoritative: contracts/rag-config-keys.md); 64–128 verified workable across all three remote APIs.

Config `auto` resolves to `LocalOnnx` when the model files are present (R8 `neurocode.rag.local.model_dir`); otherwise the feature degrades to keyword-only (FTS5) until a backend is explicitly configured — never to an implicit network call.

Pooling and prefix handling move OUT of backend code into per-model PROFILES — a small model-profile table (name, prefix_query, prefix_document, pooling, dim) — so swapping models cannot silently corrupt the index. Profile identity is pinned in `rag_index_meta` (model + dim + profile); mismatch → documented full semantic rebuild.

**Default model profile: nomic-embed-text-v1.5, ONNX int8** — 768-dim, 8192 ctx, Apache-2.0. Official ONNX exports exist including 130 MB int8, 261 MB fp16, and 521 MB f32 variants; tokenizer.json available. Client-side post-processing: mean pooling + L2 norm. Task prefixes: queries `search_query: `, documents `search_document: `. Matryoshka note: dimension truncation requires layer-norm-then-renorm — we use the full 768-dim, so no truncation occurs.

**Supported alternative profile: CodeRankEmbed** — 137M params, MIT, 8192 ctx, mean pooling + L2. Query prefix `Represent this query for searching relevant code: `, documents unprefixed. Beats CodeSage-Large on CodeSearchNet MRR 77.9. No official ONNX, but community exports exist, and a one-time optimum export with `--trust-remote-code` is viable — nomic's custom BERT + RoPE exports cleanly, proven by the official text-v1.5 ONNX of the same architecture family.

### Rationale
The user directed that the primary embedding path carry no daemon/server dependency: in-process ONNX inference satisfies that exactly (same process, CPU, no sockets). nomic-embed-text-v1.5 is chosen as the default profile: permissively licensed, official ONNX artifacts exist (so no export gamble), 8192-token context covers the 256–1024-token chunk band of R4 with headroom, and 768-dim keeps the R1 vector BLOB at ~3 KB/chunk f32 (~0.75 KB int8-quantized). The HTTP implementations are retained unchanged as the optional secondary tier so remote/daemon users lose nothing and the consent model (R5) applies as before. Hosted options for remote mode: voyage-code-3 ($0.18/M tokens, 200M free, 1024-dim default, 32k ctx), OpenAI text-embedding-3-small/large — all plain HTTPS JSON, no SDKs.

Per-model profiles (rather than hardcoded prefixes/pooling) are required because the two supported profiles already disagree on both prefix scheme and the mere existence of a document prefix; baking either into backend code would silently corrupt the index on model swap.

### Alternatives considered
- **CRITICAL RECORDED FINDING — `nomic-embed-code` (the user-requested name), EVALUATED AND REJECTED as primary**: it is a 7B Qwen2.5 decoder (~28 GB f32, 3584-dim, last-token pooling — NOT mean pooling) and is resource-incompatible with a CLI tool: 28 GB memory footprint, 3584-dim storage blowup vs 768, and decoder last-token pooling mismatches this pipeline's mean-pooling design. Recorded here so plan.md does not re-litigate; nomic-embed-text-v1.5 (R2 default) and CodeRankEmbed are the supported local profiles.
- **HTTP-only (Ollama/OpenAI-compatible) as primary** — a daemon dependency, exactly what the user directed to remove. REJECTED as primary; retained as the secondary tier.
- **Native Rust inference via candle** — would require hand-writing nomic's custom BERT + RoPE architecture. REJECTED (dependency ledger in R6).
- **rig crate** — full LLM framework for one HTTP call. REJECTED as over-dependency (secondary tier uses existing reqwest).

## R3: Change detection & incremental refresh

### Decision
File-modification-time plus SHA-256 content hashing (sha2 0.10 already a workspace dep, used by 5 crates) as the primary change detector: walk the tree (walkdir/ignore already deps), compare mtime vs indexed_at, confirm via content hash of changed candidates. Version-control metadata used opportunistically by shelling out to the git CLI (`std::process::Command` + `which::which` detection) for rename/move detection — exactly the established pattern of `crates/joey-tools/src/vcs.rs` CheckpointManager. Chunk-level skip: per-chunk content hash compared against stored hash; only re-embed changed chunks.

### Rationale
Zero new dependencies; mtime+hash is sufficient for added/modified/deleted detection (spec only requires VCS metadata "when available" with mtime fallback); git CLI shell-out follows the repo's existing convention; hashing avoids re-embedding unchanged chunks within modified files (SC-004).

### Alternatives considered
- **git2 crate v0.21** (diff_tree_to_tree, diff_tree_to_workdir_with_index) — capable but adds libgit2 (C, cc-built) to every downstream build for marginal benefit over mtime+hash+CLI; violates VIII leanness. REJECTED (recorded as future option if pure-CLI rename detection proves insufficient).
- **Full rebuild per refresh** — fails SC-004. REJECTED.

## R4: Chunking & context expansion

### Decision
Reuse the joey-neurocode parse layer's extracted artifacts (ExtractedType/ExtractedMethod carry byte spans today) as symbol-aligned chunks; ADDITIVELY extend extraction to emit line spans (byte→line conversion at index time) and to emit coarse fallback chunks for source regions containing no named artifacts (heuristic fallback languages already produce the same SourceExtraction shape, so fallback chunks fit the existing interface).

Chunk text for embedding = code body with file path and import/dependency context prepended (improves retrieval per Voyage/Anthropic contextual-chunk guidance; 256–1024-token chunks). The final embedding text is prefixed with the model profile's document prefix (R2), and the query leg uses the profile's query prefix — retrieval quality depends on BOTH prefixes being applied.

Context expansion reads the file from disk at query time using stored line spans ± configurable window (default 20 lines), clamped to file boundaries.

### Alternatives considered
- **Separate finer-grained arbitrary-span chunker** — duplicates parsing and change tracking, misaligns with typed graph. REJECTED per clarification Q1 (hybrid model chosen).
- **Storing full chunk text in the index for expansion** — REJECTED: disk duplication and staleness risk vs reading from disk (files are local by definition).

## R5: Consistency, privacy & consent mechanics

### Decision
- **Atomic snapshot semantics** via SQLite transactional swap: refresh writes new chunk/vector rows in a transaction and commits once; readers use the pre-commit state until commit (WAL mode already in use).
- **Byte-identical-when-disabled** (FR-009/SC-005) achieved by gating ALL new code paths behind `neurocode.rag.enabled` (default false) with registration/no-op short-circuits — verified by a parity test.
- **Per-project consent** stored in a small JSON file beside graph.db (`~/.joey/neurocode/projects/<hash>/consent.json` — file-backed state per constitution Principle III spirit), managed via a new CLI subcommand; states never-acknowledged / acknowledged / revoked; absent or revoked consent forces local/keyword-only.
- **Proactive pre-fetch** (FR-015) additionally requires the backend to be fully local. "Fully local" now means EITHER in-process ONNX inference (R2 primary) OR loopback HTTP (secondary backends whose base_url resolves to loopback); both are consent-free and both satisfy the FR-015 pre-fetch local-only gate.

### Alternatives considered
- **Consent inside graph.db** — couples consent lifecycle to index rebuilds/migrations; separate file is simpler and human-inspectable. REJECTED.
- **Live partial updates during refresh** — REJECTED per clarification Q5 (atomic swap chosen).

## R6: New dependencies summary (Principle VIII ledger)

### Decision
THREE new pinned dependencies, each justified per Principle VIII:

| Dependency | Version | Role here |
|---|---|---|
| ort | `=2.0.0-rc.13` (default-features=false, features `["ndarray","load-dynamic"]`) | ONNX Runtime wrapper — in-process local inference (R2 primary) |
| tokenizers | `0.23` (default-features=false) | offline tokenizer.json loading + batch encoding |
| ndarray | `0.17` | tensor interop with ort rc.13 |

- **ort `=2.0.0-rc.13`** — exact pin because it is a pre-release with per-RC breaking changes. With `load-dynamic`, the ONNX Runtime dylib is loaded at RUNTIME via `ORT_DYLIB_PATH` / `ort::init_from` — NOT embedded in the binary. The wrapper adds ~1–2 MB; the dylib is ~10–31 MB per platform (linux-x64 ~10 MB, macOS-arm64 ~9 MB, win-msvc ~31 MB), shipped alongside the binary or fetched once (R8). License: MIT OR Apache-2.0; ONNX Runtime binaries are MIT.
- **tokenizers `0.23`** — default-features=false avoids the onig C lib and hf-hub. `Tokenizer::from_file` + `encode_batch(texts, true)` confirmed as the current API. Offline tokenizer.json only — the crate never fetches. Apache-2.0.
- **ndarray `0.17`** — matches ort rc.13's ndarray interop: tensors pass straight into `session.run`.

Everything else needed is already a workspace dependency:

| Dependency | Version | Role here |
|---|---|---|
| rusqlite | 0.32 (bundled, FTS5) | vector BLOB table, transactional swap |
| reqwest | 0.12 (rustls-tls/json) | SECONDARY embedding HTTP backends only (R2) |
| sha2 + hex | 0.10 / 0.4 | content hashing for change detection |
| rayon | 1.12 | parallel cosine scan |
| walkdir / ignore | 2 / 0.4 | tree walk |
| tokio | 1 | async runtime (existing) |
| serde / serde_json | — | wire + consent JSON |
| chrono, tracing, anyhow/thiserror | — | existing utilities |
| tempfile | (dev) | parity/consent tests |

Compile-time cost: ort-sys with `load-dynamic` avoids building ONNX Runtime from source — no cc toolchain burden. Binary-size impact bounded: ~1–2 MB wrapper + sidecar dylib; the binary itself stays ~79 MB (release joey today ~78.8 MB thin-LTO stripped).

API-stability risk of a release-candidate pin is acknowledged and mitigated: the exact `=2.0.0-rc.13` pin plus wire/parity tests around the embedder.

### Alternatives considered
- **candle** — requires hand-writing nomic's custom BERT+RoPE architecture. REJECTED.
- **Python sidecar (onnxruntime/transformers)** — violates the no-Python constraint. REJECTED.
- **Keeping Ollama-HTTP-only as primary** — daemon dependency, exactly what the user asked to remove. REJECTED (retained as secondary).
- **Automatic Hugging Face fetch of model/runtime artifacts** — BANNED by user constraint. REJECTED (see R8).

Upgrade paths recorded, not adopted: hnsw_rs (ANN), sqlite-vec (vec0 tables), git2 (VCS-native diffing).

## R7: Open questions resolved → none remaining

All Technical Context NEEDS CLARIFICATION items are resolved by R1–R6 plus the added R8 (model artifact distribution — no Hugging Face); none remain.

## R8: Model artifact distribution — no Hugging Face

### Decision
User constraint forbids huggingface.co downloads. Model artifacts (`model.onnx` + `tokenizer.json` [+ license/attribution files]) live in `neurocode.rag.local.model_dir` (default `~/.joey/neurocode/models/<profile>/`). Two population paths:

(a) **manual placement** by the user, from any source they are permitted to use;
(b) **`/neurocode model fetch`** (new subcommand) which downloads from `neurocode.rag.local.mirror_url` — a PROJECT-CONTROLLED mirror (e.g. the joey-agent project's own release artifacts). The default mirror_url is EMPTY = no auto-download ever.

Both default profiles are permissively licensed (nomic-embed-text-v1.5 Apache-2.0; CodeRankEmbed MIT), so project re-hosting is lawful with attribution preserved (license/attribution files shipped alongside the artifacts).

Fetch verifies the SHA-256 of artifacts against values recorded in the project (integrity check), and the embedder refuses to load mismatching files — a corrupted or tampered mirror cannot poison the index.

### Rationale
Keeps the R2 primary tier fully offline-capable by construction: nothing in the default configuration ever contacts a network host for models. The empty-by-default mirror_url makes network use an explicit opt-in, mirroring the consent posture of R5. SHA-256 verification plus the R2 profile pinning in `rag_index_meta` means artifact integrity and index compatibility are both machine-checked at load time.

### Alternatives considered
- **hf-hub / automatic Hugging Face download** — banned by user constraint. REJECTED.
- **Bundling model artifacts (or the ORT dylib) into the binary/installer** — the 130 MB int8 model (and even the ~10–31 MB dylib) dwarfs the ~79 MB binary; R6 deliberately keeps both out-of-binary. REJECTED.
