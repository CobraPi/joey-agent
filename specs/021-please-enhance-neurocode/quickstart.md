# Quickstart: NeuroCode Semantic Code Retrieval

Runnable validation guide for the RAG enhancement. Proves the feature
end-to-end against [spec.md](./spec.md) FRs and success criteria.
Contracts are linked, not duplicated:
[embedding-backend.md](./contracts/embedding-backend.md),
[hybrid-search.md](./contracts/hybrid-search.md),
[rag-store-schema.md](./contracts/rag-store-schema.md),
[rag-config-keys.md](./contracts/rag-config-keys.md),
[neurocode-rag-tools.md](./contracts/neurocode-rag-tools.md),
[neurocode-rag-command.md](./contracts/neurocode-rag-command.md);
data model: [data-model.md](./data-model.md).

## Prerequisites

- Rust stable toolchain; repo on branch `021-please-enhance-neurocode`;
  `cargo build --workspace` green.
- **Nothing else is required**: with no model artifacts placed, search
  degrades to keyword-only with an explicit indication (FR-008) until
  models are present.
- To go hybrid, obtain the model artifacts — `model.onnx` +
  `tokenizer.json` for the `nomic-embed-text-v1.5` profile (int8 variant
  recommended, ~130 MB) — from a source you are permitted to use.
  **joey itself NEVER downloads from Hugging Face**; artifacts arrive via
  manual placement or the project-controlled mirror only (research.md R8;
  [embedding-backend.md](./contracts/embedding-backend.md) § Local Model
  Artifacts & Distribution).
- OPTIONAL secondary paths: an Ollama daemon on `localhost:11434` or any
  OpenAI-compatible HTTP endpoint still work as backends — remote ones
  consent-gated (FR-012), loopback counts as local.
- A scratch repository to index. Suggested: a small mixed-language sample
  with a Rust file containing several functions and a Python script with
  top-level code (the Python top-level exercises fallback chunks, FR-014).

## Setup

1. **Parity baseline FIRST** (before enabling anything): run the agent
   REPL with neurocode enabled but RAG disabled (default
   `neurocode.rag.enabled=false`), issue a code question, capture the
   transcript/context — this is the FR-009/SC-005 parity reference for V1.
2. **Place model artifacts** (skip if keyword-only suffices): either copy
   `model.onnx` + `tokenizer.json` into `model_dir` (default
   `~/.joey/neurocode/models/nomic-embed-text-v1.5/`), OR configure
   `neurocode.rag.local.mirror_url` and run `/neurocode model fetch`
   (downloads from the project mirror, verifies SHA-256, REFUSES
   mismatches). If `ort` load-dynamic cannot find the ONNX Runtime dylib,
   set `ORT_DYLIB_PATH` or `neurocode.rag.local.ort_dylib_path` — or,
   if the ORT dylib is not installed system-wide, fetch it from the
   project mirror with `/neurocode model fetch --dylib` before first
   hybrid search.
3. **Enable**: set config keys per
   [rag-config-keys.md](./contracts/rag-config-keys.md) —
   `neurocode.rag.enabled=true` (plus optional `model`/`base_url`
   overrides). Note: `neurocode.rag.api_key` auto-routes to `.env`
   (the `_KEY` rule), never `config.yaml`.
4. **First index**: `/neurocode index --force` — cold index runs; expected:
   progress observable, completes; `/neurocode status` shows the RAG
   section (chunk count > 0, model, dim, state `Ready`).

## Validation Scenarios

Each: steps → expected outcome → FR/SC reference.

### V1 Disabled parity (P1)

- Steps: with `neurocode.rag.enabled=false` (default), run an agent
  session, `/neurocode status`, and the agent tool listing.
- Expected: byte-identical to the pre-enhancement baseline captured in
  Setup step 1; no RAG section in status; no `neurocode_search` tool
  registered.
- Refs: FR-009, SC-005.

### V2 Natural-language search (P1)

- Steps: `/neurocode search where is token validation handled` against
  the scratch repo (wording deliberately absent from the code).
- Expected: ranked results, each with file/symbol/kind/line range; the
  correct location in the top 5.
- Refs: FR-001, SC-001.

### V3 Exact-symbol ranking (P2)

- Steps: search an exact function name from the sample.
- Expected: that exact symbol ranked first in the single blended list.
- Refs: FR-002, SC-002.

### V4 File-scope filter (P2)

- Steps: repeat V2 with `--path` restricting to a subdirectory.
- Expected: only results whose paths match the filter.
- Refs: FR-003.

### V5 Fallback chunks (P3)

- Steps: search for logic living in the Python script's top level (no
  named artifact).
- Expected: a fallback-chunk result, visibly badged differently from
  symbol-aligned results.
- Refs: FR-014.

### V6 Incremental refresh (P3)

- Steps: touch/modify ≤10 files; wait for background refresh (or force
  it); time it. Inspect status counts and the DB. Then delete a file and
  refresh again.
- Expected: refresh < 5s excluding embedding latency; only changed
  files' rows updated (status counts + DB inspection); deleted file's
  entries purged (cascade, no orphans).
- Refs: FR-004, FR-005, SC-004.

### V7 Context expansion (P4)

- Steps: inspect a result's context block; pick a hit near file
  start/end; retry with `--expand-lines 5` and `--expand-lines 100`.
- Expected: surrounding lines present, clamped at file boundaries
  without error; window visibly changes with the flag.
- Refs: FR-006.

### V8 Relationship expansion (P5)

- Steps: `--relations 1` (and `2`) on a method that calls others, and on
  a class with members.
- Expected: related chunks appended with relationship kind; deduplicated;
  bounded at the depth limit.
- Refs: FR-007.

### V9 Degradation (P1)

- Steps (backend gone): point `base_url` at a dead port (or stop the
  secondary HTTP backend); search; restore; search again.
- Steps (model files gone): remove or byte-corrupt the artifacts in
  `model_dir`; search; then place valid artifacts and search again.
- Expected: keyword-only results with an explicit degradation indication
  in every case; the agent turn never fails. Corrupt artifacts are
  refused on SHA-256 mismatch (`ModelFilesCorrupt`), never loaded. After
  the backend returns / valid artifacts are placed, hybrid search resumes
  (degradation note gone).
- Refs: FR-008.

### V10 Remote consent (P1)

- Scope: REMOTE HTTP backends only — local ONNX (and loopback backends)
  are consent-free.
- Steps: configure a REMOTE `base_url` without acknowledgement; search
  and refresh. `/neurocode consent show` → `consent ack` → search →
  `consent revoke` mid-use → search again.
- Expected: before ack, search/refresh stay local/keyword-only and
  `consent show` reports `never_acknowledged`; after ack, remote
  embedding begins; after revoke, immediately local/keyword-only and
  nothing further is sent to the remote service.
- Refs: FR-012, edge cases 6–7.

### V11 Status reporting (P3)

- Steps: `/neurocode status` with RAG enabled.
- Expected: freshness (including semantic index), entry counts, last
  update time, backend health, consent state.
- Refs: FR-013.

### V12 Agent tool path (P2)

- Steps: in an agent session, have the model call `neurocode_search`
  itself.
- Expected: same result shape as the CLI (`--json`); context enters only
  via the tool call (on-demand default); pre-fetch stays off unless
  opted in AND the backend is local.
- Refs: FR-010, FR-015.

### V12b Prefetch gate (optional)

- Steps: enable `neurocode.rag.prefetch.enabled` with the default local
  ONNX backend (trivially satisfiable locally — no daemon needed); then
  point the backend at a remote `base_url`.
- Expected: with the local backend, prompt-relevant results injected into
  context; with a remote backend, pre-fetch silently inactive.
- Refs: FR-015.

### V13 Workspace suite (P0 gate)

- Steps: `cargo build --workspace && cargo test --workspace`.
- Expected: green, including the new parity / wire-contract / migration
  / budget smoke tests.
- Refs: constitution gate.

### V14 Model swap (P3)

- Steps: change `neurocode.rag.model` to `CodeRankEmbed`, place that
  profile's artifacts in its `model_dir`, then `/neurocode index --force`;
  rerun the V2/V3 searches.
- Expected: results correct with the new profile's prefixes/pooling
  applied (CodeRankEmbed: query-side prefix, empty document prefix, mean
  pooling); the profile change takes the documented full semantic rebuild
  path — all vectors re-embedded, keyword index and typed graph untouched.
- Refs: FR-001; [embedding-backend.md](./contracts/embedding-backend.md)
  § Model Profiles.

## Performance Spot-Checks

- **SC-003**: on a ≥100k-chunk index (generate synthetic or use a large
  repo), time `/neurocode search` → p95 < 2s warm.
- **SC-004**: time the V6 refresh (< 5s for ≤10 changed files,
  excluding embedding latency).
- **Memory**: observe resident memory during search at scale → within
  [plan.md](./plan.md) budget (≤512MB at ≤250k chunks via int8).
- **Local embedding throughput**: time a 64-chunk embed batch — ballpark
  ~10–40 ms per 512-token text on an AVX2-class CPU (VERIFY this
  ballpark; plan.md M7 benchmarks pin the real numbers).

## Troubleshooting (brief)

- **No RAG section in status** → enabled flag not set, or config layer
  routing (check `.env` for `api_key`).
- **Embedding errors** → see the degradation ladder in
  [hybrid-search.md](./contracts/hybrid-search.md); check backend health
  in status.
- **Dimension mismatch after model change** → expected full semantic
  rebuild ([embedding-backend.md](./contracts/embedding-backend.md)).
- **Search stuck keyword-only** → model files missing or hash-mismatched:
  check `model_dir` contents and the `rag_model_artifacts` hashes
  (`ModelFilesMissing` / `ModelFilesCorrupt` degrade to keyword-only with
  indication by design).
- **`ProfileMismatch`** → expected after a profile change; run the
  documented full semantic rebuild (`/neurocode index --force`) —
  keyword index untouched.
- **ort dylib not found** → set `ORT_DYLIB_PATH` or
  `neurocode.rag.local.ort_dylib_path` to the ONNX Runtime shared
  library location, or fetch the platform dylib from the project
  mirror with `/neurocode model fetch --dylib`.
