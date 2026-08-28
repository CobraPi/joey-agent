# Contract: Embedding Backend

**Spec**: [spec.md](../spec.md) | **Plan**: [plan.md](../plan.md) | **Research**: [research.md](../research.md) R2/R6/R8 | **Data model**: [data-model.md](../data-model.md)

The async trait abstracting embedding providers, plus the pinned wire
shapes for its three implementations. Lives in
`crates/joey-neurocode-rag/src/embed/`. Research decisions R1/R2/R6/R8
are FINAL: the **PRIMARY backend is fully-local in-process ONNX
(`LocalOnnx`)**; the HTTP backends (`OpenAiCompat`, `OllamaNative`) are
**SECONDARY**; **huggingface.co is never contacted** (R8).

## Trait surface

```rust
#[async_trait]
pub trait EmbeddingBackend: Send + Sync {
    /// Embed a batch of chunk texts. Returns one f32 vector per input,
    /// in input order. Vectors are normalized before return.
    /// Callers pass RAW text — prefixes are applied by the embedder.
    async fn embed(&self, batch: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;

    /// Static descriptor — no network I/O.
    fn describe_embedder(&self) -> EmbedderInfo;

    /// Liveness probe (one trivial embed or equivalent cheap request).
    async fn health_check(&self) -> Result<(), EmbedError>;
}

pub enum BackendKind {
    LocalOnnx,     // PRIMARY   — in-process ONNX Runtime inference
    OpenAiCompat,  // SECONDARY — remote path, consent-gated
    OllamaNative,  // SECONDARY — loopback = local, consent-free
}

pub struct EmbedderInfo {
    pub backend_kind: BackendKind,  // LocalOnnx | OpenAiCompat | OllamaNative
    pub profile_name: String,       // model-profile identity (see Model Profiles)
    pub dim: u32,                   // 0 = unknown until first embed
    pub pooling: Pooling,           // profile-pinned pooling mode (Mean for both profiles)
    pub prefixes: EmbedPrefixes,    // query/document prefixes (see Model Profiles)
    pub base_url: String,           // HTTP backends only; empty for LocalOnnx
    pub model: String,
}
```

## Implementations

### 1. `LocalOnnx` — PRIMARY: fully-local, in-process ONNX inference

- Runs the embedding model **in-process** via `ort` **`=2.0.0-rc.13`**
  (exact pin — pre-release with per-RC breaking changes), built with
  `default-features = false, features = ["ndarray", "load-dynamic"]`.
- The ONNX Runtime **dylib is located at runtime** via
  `ORT_DYLIB_PATH` or programmatic init (`ort::init_from`), resolved
  through `neurocode.rag.local.ort_dylib_path` → env → system lookup.
  It is **NOT embedded in the binary** (R6).
- Tokenization uses the `tokenizers` crate (`0.23`,
  `default-features = false`) fully **OFFLINE**
  (`Tokenizer::from_file`; padded `encode_batch(texts, true)`); no
  `hf-hub` feature, no network, ever.
- Model + tokenizer artifacts are loaded from
  `neurocode.rag.local.model_dir` under the integrity rules of
  "Local Model Artifacts & Distribution" below.
- Pooling, normalization, and prefixes are applied per the model
  **PROFILE** (next section) — never hardcoded in backend code.
- No daemon, no socket, no network — fully local by construction.

### 2. `OpenAiCompat` — SECONDARY — POST `{base_url}/v1/embeddings`

Remote path, consent-gated (see Consent gate). Covers Ollama's
compatibility endpoint, OpenAI, and Voyage
(`api.voyageai.com/v1/embeddings`) with zero per-vendor code (R2).

**Request JSON (pinned EXACTLY — field set and ordering as serialized):**

```json
{"model": "<model>", "input": ["<chunk text 1>", "<chunk text 2>"]}
```

Optional header: `Authorization: Bearer *** when
`neurocode.rag.api_key` is non-empty.

**Response JSON (pinned):**

```json
{"data": [{"embedding": [0.0123, -0.0456], "index": 0},
          {"embedding": [0.0789,  0.0321], "index": 1}]}
```

Wire-contract tests MUST assert both shapes byte-for-byte, modulo the
model-name string and vector values.

### 3. `OllamaNative` — SECONDARY — POST `{base_url}/api/embed`

Thin second impl giving truncate/keep_alive control. Loopback
`base_url` = local, consent-free.

**Request JSON (pinned):**

```json
{"model": "<model>", "input": ["<s1>", "<s2>"], "truncate": true, "keep_alive": "<configured>"}
```

**Response JSON (pinned):**

```json
{"embeddings": [[0.0123, -0.0456], [0.0789,  0.0321]]}
```

Same byte-for-byte test obligation, modulo model name and values.

The HTTP wire contracts above are **pinned unchanged** from the
previous revision; only their tier changed (primary → secondary).

## Model Profiles (normative)

Pooling and prefix handling live in a per-model PROFILE table, NOT in
backend code, so swapping models cannot silently corrupt the index.
**Swapping models without changing profile handling accordingly is a
contract violation.**

| Profile | dim | ctx | Pooling + norm | Query prefix | Document prefix | License |
|---|---|---|---|---|---|---|
| `nomic-embed-text-v1.5` | 768 | 8192 | mean pooling + L2 (client-side) | `search_query: ` | `search_document: ` | Apache-2.0 |
| `CodeRankEmbed` | 768 | 8192 | mean pooling + L2 (client-side) | `Represent this query for searching relevant code: ` | `` (none) | MIT |

Normative rules:

1. The embedder MUST apply the profile's prefixes **BEFORE
   tokenization**; callers pass raw text (the trait stays raw-text in,
   vector out).
2. Pooling MUST match the profile (mean pooling + client-side L2 for
   both profiles above); the backend never improvises.
3. **Profile identity (name + dim + pooling) is persisted in
   `rag_index_meta` alongside `model`.**
4. Any mismatch on load → `ProfileMismatch` (a DimensionMismatch-style
   error) → documented full semantic rebuild path — drop all
   `rag_vectors` rows and re-embed every chunk; the typed graph and
   keyword index are untouched (see
   [rag-store-schema.md](./rag-store-schema.md)).

Evaluated and **REJECTED**: profile `nomic-embed-code` — 7B Qwen2.5
decoder, ~28 GB, 3584-dim, last-token pooling — incompatible with CLI
resource targets (R2). Recorded so tasks do not re-litigate.

## Local Model Artifacts & Distribution (no Hugging Face)

`neurocode.rag.local.model_dir` (default
`~/.joey/neurocode/models/<profile>/`) layout:

```
<model_dir>/
  model.onnx        # embedding graph
  tokenizer.json    # offline tokenizer
  LICENSE*          # license / attribution files (required — both
                    # profiles are permissively licensed; re-hosting
                    # keeps attribution)
```

Population ONLY via:

- **(a) manual user placement** — from any source the user is
  permitted to use; or
- **(b) `/neurocode model fetch`** — downloads from the configured
  `neurocode.rag.local.mirror_url` with **SHA-256 integrity
  verification** against project-recorded hashes.

Rules:

- **Self-registration on manual placement**: when no
  `rag_model_artifacts` row exists for the profile, the **first
  successful load computes the SHA-256 of the present files and WRITES
  the row** (manual placement cannot know project-recorded hashes —
  first-load trust, later immutability). Every subsequent load
  verifies against the stored row and **refuses on mismatch**
  (`ModelFilesCorrupt`) — a corrupted or tampered artifact set cannot
  poison the index.
- `/neurocode model fetch` is **stricter**: it writes the row from the
  project-recorded expected hashes at fetch time (see
  [neurocode-rag-command.md](./neurocode-rag-command.md) § Model fetch
  subcommand).
- **huggingface.co MUST NOT be contacted by joey.** No `hf-hub`, no
  auto-download (R8; user constraint).
- `mirror_url` defaults to EMPTY = fetch disabled — joey never
  downloads models; it is only ever pointed at project-controlled
  mirrors.
- The `tokenizers` crate is used offline (`default-features = false`;
  no `hf-hub`).

**Consent rule: `LocalOnnx` requires NO consent** — fully local,
in-process. It satisfies the FR-015 pre-fetch local-only gate together
with loopback HTTP backends.

## Batching

- Caller-side default batch is **64**; the config key
  `neurocode.rag.batch_size` **VALIDATES 16–128** (out-of-range →
  config-load warning + fallback to default — authoritative:
  [rag-config-keys.md](./rag-config-keys.md)). Valid on all three
  backends (R2).
- The backend MAY split a batch into smaller requests/inference runs
  internally; the trait-level result MUST remain order-preserving:
  response position `i` corresponds to input `batch[i]` in all impls
  (OpenAiCompat uses the `index` field; OllamaNative uses array order;
  LocalOnnx uses in-process batch order).
- `LocalOnnx` batches via padded `encode_batch` and parallelizes with
  rayon across texts.

## Throughput note (non-normative estimate)

~10–40 ms per 512-token text on AVX2-class CPUs (estimate; M7
benchmarks to pin real numbers). Batching via padded `encode_batch`;
rayon across texts. Adequate for incremental-refresh budgets (FR-004);
full semantic rebuilds are expected to be long-running.

## Error taxonomy

```rust
pub enum EmbedError {
    Unreachable,          // connect/timeout/DNS (secondary HTTP backends)
    AuthRejected,         // HTTP 401/403
    RateLimited,          // HTTP 429
    DimensionMismatch,    // dim != rag_index_meta.embed_dim
    ProfileMismatch,      // profile identity (name+dim+pooling) != rag_index_meta
    ModelFilesMissing,    // model_dir absent or incomplete
    ModelFilesCorrupt,    // SHA-256 mismatch / unloadable artifacts
    MalformedResponse,    // JSON shape violation, wrong count
}
```

Every variant maps to **degradation, never a turn hard-fail**
(FR-008): the search pipeline catches `EmbedError` and falls back to
KeywordOnly with an explicit indication (see
[hybrid-search.md](./hybrid-search.md)).

## Consent gate (FR-012)

`LocalOnnx` performs NO network calls and requires **NO consent**
(fully local, in-process). For the SECONDARY HTTP backends, NO network
call is made unless BOTH hold:

1. `neurocode.rag.enabled == true`, AND
2. the backend is local — `base_url` host resolves to loopback
   (`127.0.0.1`, `::1`, `localhost`) — OR the per-project consent state
   is `Acknowledged` for this project (`consent.json`, see
   [data-model.md](../data-model.md) §8).

A loopback `base_url` counts as local unconditionally (Ollama on
loopback = consent-free). `Revoked`, `NeverAcknowledged`, or a missing
consent file forces local/keyword-only; the gate is checked before
every embed call so revocation takes effect immediately (edge case 7).
The FR-015 pre-fetch local-only gate is satisfied by `LocalOnnx` OR a
loopback HTTP backend.

## Dimension pinning

- The first successful embed fixes `rag_index_meta.embed_dim`.
- Any later embed returning a different dimension yields
  `DimensionMismatch`.
- Profile identity (name + dim + pooling) is persisted alongside
  `model` (Model Profiles rule 3); mismatch → `ProfileMismatch` →
  full semantic rebuild (keyword index untouched). See
  [rag-store-schema.md](./rag-store-schema.md).

---

Regression coverage: required by constitution Principle VII — tasks
MUST include tests pinning this contract. Obligations: byte-for-byte
wire-shape tests for both HTTP backends; **profile prefix/pooling unit
tests** (prefixes applied pre-tokenization; mean pooling + L2 per
profile; CodeRankEmbed's empty document prefix); **model-artifact
integrity test** (bad SHA-256 → refuse load); **LocalOnnx consent-free
assertion** (no consent file required; no network attempted).
