# Data Model: NeuroCode Semantic Code Retrieval

**Branch**: `021-please-enhance-neurocode` | **Date**: 2026-08-27 | **Spec**: [spec.md](./spec.md) | **Research**: [research.md](./research.md)

This document concretizes the spec's Key Entities into fields, types,
validation rules, and relationships, and defines the on-disk representation:
an additive v2→v3 migration of the existing per-project SQLite `graph.db`
(`~/.joey/neurocode/projects/<sha256-of-root>/graph.db`) plus a sibling
`consent.json`. Technical decisions are FINAL per research.md R1–R8.

## Entities (conceptual, from spec Key Entities, now concretized)

### 1. CodeChunk

The unit of indexed code. Discriminated union on `chunk_kind`
(clarification Q1: hybrid chunking — symbol artifacts are first-class,
coarse chunks cover the rest).

```
CodeChunk ::= SymbolAligned | FallbackCoarse

SymbolAligned {
    artifact_id: ArtifactId (FK → code_artifacts.id; NOT NULL for this kind),
    symbol_name: String,                       // e.g. "UserServiceImpl"
    symbol_kind: SymbolKind,                   // class | interface | enum | method | field
    language: String,                          // e.g. "java", "python"
}
FallbackCoarse {
    // no symbol identity; file + line-range identity only (FR-014)
}
```

Common fields (both kinds):

| Field | Type | Semantics |
|---|---|---|
| `chunk_id` | `String` (stable identity) | Deterministic: `source_path + start_line + end_line + kind discriminator`. Not a rowid. |
| `source_path` | `String` | Repository-relative path of the source file. |
| `start_line` | `u32` (1-based, inclusive) | First line of the chunk's code body. |
| `end_line` | `u32` (1-based, inclusive) | Last line of the chunk's code body. |
| `language` | `String` | Language tag (redundant on SymbolAligned; canonical location on FallbackCoarse). |
| `content_hash` | `String` (SHA-256 hex) | Hash over the **RAW UNPREFIXED chunk text** — the constructed chunk text including the R4 contextual prefix (file path + imports/dependency context) but **EXCLUDING the profile's document prefix**. NOT the exact bytes sent to the embedder: the engine applies the profile prefix at embed time. **Pinned so a profile change never invalidates chunk hashes; only embeddings rebuild.** |
| `embed_model` | `String` | Model id that produced this chunk's vector. |
| `embed_dim` | `u32` | Dimensionality of this chunk's vector. |
| `quantization` | `f32 \| int8` | Storage encoding of this chunk's vector. |
| `updated_at` | `String` (RFC 3339) | Last (re-)embed time. |

**Validation rules** (derived from FRs):

- `start_line ≤ end_line`; both ≥ 1 (1-based inclusive).
- Line spans clamped to actual file boundaries at read time — expansion
  near file start/end never errors (FR-006 acceptance 2).
- `content_hash` recomputed on every candidate change during refresh;
  equal hash ⇒ chunk untouched (FR-004 chunk-level skip, SC-004).
- `chunk_id` is derived identity: a moved/shifted chunk gets a new id
  (old purged, new created) — no in-place id mutation.

**Prefixing note (pinned)**: the chunk embedding text is constructed with
the profile's DOCUMENT prefix at embed time; queries use the profile's
QUERY prefix. Prefixes live in `rag_index_meta` (`prefix_query` /
`prefix_document`) and are applied by the engine — never baked into
stored chunk text or `content_hash`.

**Relationships**: 1:1 with `VectorRecord`; N:1 with `code_artifacts`
(SymbolAligned only); N:M via `RelationshipLink`; 1:N per source file.

### 2. VectorRecord

The dense embedding of exactly one CodeChunk, stored as a BLOB (R1).

| Field | Type | Semantics |
|---|---|---|
| `chunk_id` | `String` (PK, FK → rag_chunks) | 1:1 with CodeChunk. |
| `vector` | `BLOB` | Little-endian elements. `f32`: raw IEEE-754 words. `int8`: one f32 scale prefix (4 bytes) followed by `dim` int8 codes (value ≈ code × scale). |
| `dim` | `u32` | Element count. |
| `quantization` | `f32 \| int8` | Encoding of the BLOB. |

Vectors are **stored normalized**; cosine similarity is computed as a plain
dot product in the exhaustive rayon scan (R1).

The vector is the embedding of the chunk text WITH the profile's document
prefix applied (see the CodeChunk prefixing note); queries are embedded
with the profile's query prefix. Prefixes come from `rag_index_meta` and
are applied by the engine.

**Validation rules**:

- `dim` must equal `rag_index_meta.embed_dim` — enforced at write time.
- BLOB byte length must equal `dim × 4` (f32) or `dim × 1 + 4` (int8
  scale prefix); any other length is corrupt and rejected.
- Absence of a VectorRecord for an existing chunk means "not yet
  embedded" — the chunk is excluded from semantic ranking but remains
  keyword-searchable.

### 3. IndexMetadata (`rag_index_meta`, singleton row id=1)

Per-project RAG index descriptor; pins the embedding configuration so a
model/dim change is detectable (R2).

| Field | Type | Semantics |
|---|---|---|
| `schema_version` | `u32` (= 3) | NeuroCode graph.db schema version after migration. |
| `embed_profile` | `String` | Model PROFILE name in force (e.g. `nomic-embed-text-v1.5`, `CodeRankEmbed`) — the profile-identity anchor. |
| `embed_model` | `String` | Model id in force for the stored vectors. |
| `embed_dim` | `u32` | Dimensionality in force. |
| `pooling` | `String` (`mean \| last_token`) | Profile-pinned pooling mode in force (both shipped profiles: mean pooling + client-side L2). |
| `prefix_query` | `String` | Profile's query prefix; applied by the engine to search queries before tokenization. |
| `prefix_document` | `String` | Profile's document prefix; applied by the engine to chunk text at embed time. |
| `quantization_policy` | `String` | `f32`, `int8`, or an auto policy (int8 above threshold). |
| `chunk_count` | `u64` | Number of rag_chunks rows (denormalized for cheap status). |
| `last_refresh_at` | `String` (RFC 3339, nullable) | Last committed refresh. |
| `refresh_state` | `idle \| refreshing` | Observable refresh status (FR-013); also the FR-004 swap coordinator. |
| `created_at` | `String` (RFC 3339) | Index creation time. |

**Rules** (documented behavior, research.md R2):

- On configuration change, if requested
  `embed_profile`/`embed_model`/`embed_dim`/`pooling`/`prefix_query`/
  `prefix_document`/`quantization` ≠ stored values, the semantic index
  undergoes a full rebuild (all vectors re-embedded); the keyword index
  and typed graph are untouched.
- **Profile identity mismatch on load → `ProfileMismatch` → documented
  full semantic rebuild path**: drop all `rag_vectors` rows and re-embed
  every chunk; keyword index and typed graph untouched.

**Artifact integrity** (companion `rag_model_artifacts` record): each
local-ONNX profile loaded from `model_dir` is pinned by a companion row
(keyed by `profile`) recording the SHA-256 of `model.onnx` and
`tokenizer.json`, the model size, fetch time, and the mirror used. The
table is created by the same idempotent, additive v3 migration. The
embedder **refuses to load hash-mismatching artifacts**
(`ModelFilesCorrupt`) — a corrupted or tampered mirror cannot poison
the index.

**Manual-placement self-registration (pinned)**: when no
`rag_model_artifacts` row exists for a profile, the **first successful
load computes the SHA-256 of the present files and WRITES the row**
(self-registration). Manual placement cannot know project-recorded
hashes — the rule is first-load trust + later immutability. Every
subsequent load verifies against the stored row and refuses on
mismatch (`ModelFilesCorrupt`). `/neurocode model fetch` instead
writes the row from the project-recorded expected hashes at fetch time
(stricter — mismatched downloads are refused before anything lands in
`model_dir`).

### 4. SearchRequest

| Field | Type | Default | Semantics |
|---|---|---|---|
| `query` | `String` | — | Natural language and/or exact symbol names. **Must be non-empty after trim; else validation error (edge case 1).** |
| `file_scope_filter` | `Option<String>` (glob) | `None` | Restricts results to matching paths (FR-003). |
| `limit` | `usize` | 10 | Max RankedResults returned. |
| `expand_context_lines` | `u32` | 20 | Context window ± around the hit (FR-006). |
| `relation_depth` | `u8` | 0 | Relationship expansion depth; bounded `≤ 2` (FR-007). |
| `include_fallback_chunks` | `bool` | `true` | Whether FallbackCoarse chunks participate (FR-014 coverage). |

### 5. RankedResult

One entry in the single blended response list (FR-002).

| Field | Type | Semantics |
|---|---|---|
| chunk reference | `chunk_id` + denormalized `source_path`, `symbol_name?`, `symbol_kind?`, `start_line`/`end_line` | Enough to act without a join (FR-001 acceptance 1). |
| `chunk_kind` badge | `symbol-aligned \| fallback` | Visible distinction in output (FR-014). |
| `fused_score` | `f64` | RRF score: `Σ 1/(60 + rank_i)` over the dense and keyword rank lists (R1; FTS5 bm25 negated rank converted to ascending ordinals first). |
| `semantic_rank` | `Option<u32>` | Rank in the dense list; `None` in keyword-only degradation. |
| `keyword_rank` | `Option<u32>` | Rank in the BM25 list; `None` if the chunk didn't match keywords. |
| `degradation_note` | `Option<String>` | Present **iff** results are keyword-only (FR-008 explicit indication). |
| `expanded_context` | `String` | Chunk text ± window lines, read from disk at query time, clamped to file boundaries (R4). |
| `related` | `Vec<RelatedEntity>` | Populated when `relation_depth > 0`; deduplicated (FR-007). |

### 6. ChangeDelta

Output of change detection (R3: mtime walk + SHA-256 confirmation; git CLI
shell-out when available); input to refresh.

| Field | Type | Semantics |
|---|---|---|
| `added` | `Vec<PathBuf>` | New files to index. |
| `modified` | `Vec<PathBuf>` | Files whose chunk hashes differ from stored. |
| `removed` | `Vec<PathBuf>` | Files whose chunks+vectors are purged (FR-005). |
| `renamed` | `Vec<(PathBuf, PathBuf)>` | `(old, new)` pairs from git CLI rename detection when available; otherwise detected as remove+add (equivalent end state). |

### 7. RelationshipLink

A chunk-level edge derived at index time from the existing typed-graph
edges (artifact-to-artifact projected down to their chunks).

| Field | Type | Semantics |
|---|---|---|
| `from_chunk_id` | `String` (FK → rag_chunks) | Source chunk. |
| `to_chunk_id` | `String` (FK → rag_chunks) | Target chunk. |
| `edge_kind` | `EdgeKind` | Reuses the existing graph vocabulary (`crates/joey-neurocode/src/graph/edge.rs`): `Implements`, `IsImplementedBy`, `Injects`, `ExchangesType`, `MemberOf`, `ReferencesRule`, `InheritsRule` — as applicable at chunk level. |

Fully derived and rebuildable (never authoritative — the typed graph is).
Traversal at query time is bounded by `relation_depth ≤ 2` and deduplicates
visited chunks (FR-007 acceptance 2–3).

### 8. ConsentRecord (`consent.json` — NOT in graph.db)

Per-project remote-backend consent, stored as a sibling JSON file beside
graph.db: `~/.joey/neurocode/projects/<hash>/consent.json` (R5 — a
separate file decouples consent from index rebuilds and stays
human-inspectable).

| Field | Type | Semantics |
|---|---|---|
| `project_root` | `String` | Absolute path of the consented project. |
| `remote_backend_url` | `String` | The remote embedding base_url consented to. |
| `state` | `NeverAcknowledged \| Acknowledged \| Revoked` | Current state. |
| `acknowledged_at` | `Option<String>` (RFC 3339) | When consent was given/re-given. |
| `revoked_at` | `Option<String>` (RFC 3339) | When consent was revoked. |
| `model_at_ack_time` | `Option<String>` | embed_model in force at acknowledgement (audit trail). |

**State transitions**:

```
NeverAcknowledged ──(explicit CLI ack)──▶ Acknowledged
Acknowledged ──(revoke, any time)───────▶ Revoked
Revoked ──(re-ack allowed)──────────────▶ Acknowledged
```

**Invariant**: only `Acknowledged` permits remote embedding calls for this
project (FR-012). `NeverAcknowledged`, `Revoked`, or a missing file forces
local/keyword-only operation; revocation mid-operation stops all further
content egress on subsequent refreshes/searches (edge case "consent
revoked mid-operation").

## Storage Schema (graph.db v3 — additive DDL, idempotent migration)

All new tables live in the existing per-project SQLite DB. Nothing
existing is dropped or altered.

```sql
-- Chunk registry (both kinds; symbol fields NULL for fallback chunks)
CREATE TABLE IF NOT EXISTS rag_chunks (
    chunk_id     TEXT PRIMARY KEY,
    chunk_kind   TEXT NOT NULL CHECK (chunk_kind IN ('symbol','fallback')),
    artifact_id  INTEGER REFERENCES code_artifacts(id) ON DELETE CASCADE,
    source_path  TEXT NOT NULL,
    start_line   INTEGER NOT NULL,
    end_line     INTEGER NOT NULL,
    language     TEXT,
    symbol_name  TEXT,
    symbol_kind  TEXT,
    content_hash TEXT NOT NULL,
    embed_model  TEXT,
    embed_dim    INTEGER,
    updated_at   TEXT              -- RFC 3339
);
CREATE INDEX IF NOT EXISTS rag_chunks_source_path ON rag_chunks(source_path);
CREATE INDEX IF NOT EXISTS rag_chunks_hash        ON rag_chunks(content_hash);
CREATE INDEX IF NOT EXISTS rag_chunks_artifact    ON rag_chunks(artifact_id);

-- Dense vectors, 1:1 with chunks
CREATE TABLE IF NOT EXISTS rag_vectors (
    chunk_id     TEXT PRIMARY KEY
                 REFERENCES rag_chunks(chunk_id) ON DELETE CASCADE,
    dim          INTEGER NOT NULL,
    quantization TEXT NOT NULL CHECK (quantization IN ('f32','int8')),
    vector       BLOB NOT NULL
);

-- Singleton (id = 1 enforced by CHECK)
CREATE TABLE IF NOT EXISTS rag_index_meta (
    id                  INTEGER PRIMARY KEY CHECK (id = 1),
    schema_version      INTEGER NOT NULL,
    embed_profile       TEXT,
    embed_model         TEXT,
    embed_dim           INTEGER,
    pooling             TEXT,             -- 'mean' | 'last_token'
    prefix_query        TEXT,
    prefix_document     TEXT,
    quantization_policy TEXT,
    chunk_count         INTEGER NOT NULL DEFAULT 0,
    last_refresh_at     TEXT,
    refresh_state       TEXT NOT NULL DEFAULT 'idle'
                        CHECK (refresh_state IN ('idle','refreshing')),
    created_at          TEXT NOT NULL
);

-- Artifact integrity for local-ONNX profiles (one row per profile)
CREATE TABLE IF NOT EXISTS rag_model_artifacts (
    profile          TEXT PRIMARY KEY,
    model_sha256     TEXT NOT NULL,
    tokenizer_sha256 TEXT NOT NULL,
    model_size_bytes INTEGER NOT NULL,
    fetched_at       TEXT NOT NULL,       -- RFC 3339
    mirror_url_used  TEXT                 -- NULL for manual placement
);

-- Derived chunk-level edges (rebuildable)
CREATE TABLE IF NOT EXISTS rag_chunk_edges (
    from_chunk_id TEXT NOT NULL,
    to_chunk_id   TEXT NOT NULL,
    edge_kind     TEXT NOT NULL,
    PRIMARY KEY (from_chunk_id, to_chunk_id, edge_kind)
);
```

**Migration rules (v2 → v3)**:

- Pure `CREATE TABLE IF NOT EXISTS` set — never drops or ALTERs existing
  tables; safe to re-run (idempotent).
- `rag_model_artifacts` is part of the same additive set (idempotent);
  rows are written by `/neurocode model fetch` or first verified manual
  placement, and SHA-256 re-verified before every embedder load.
- `NEUROCODE_SCHEMA_VERSION` constant bumped `2 → 3` in
  `crates/joey-neurocode/src/lib.rs`.
- An existing v2 DB opens unchanged and simply gains empty RAG tables;
  keyword/graph behavior is identical until RAG is enabled (FR-009).
- No export/import step; migration runs inside the normal open path.

**Invariants**:

- A refresh writes chunks + vectors + edges + `rag_index_meta` updates
  inside **one transaction**; COMMIT is the atomic swap point (FR-004,
  clarification Q5). No reader ever observes torn state.
- Readers on WAL (already in use) continue seeing the pre-commit snapshot
  until commit lands.
- Chunk purge cascades to its vector and derived edges via the FK
  (`ON DELETE CASCADE`), so removed files leave no orphans (FR-005).
- `rag_index_meta.embed_dim` is the single source of truth for the
  `dim == embed_dim` write-time check on VectorRecords.
- The embedder refuses to load local artifacts whose SHA-256 differs from
  the pinned `rag_model_artifacts` row (`ModelFilesCorrupt`).

## State & Lifecycle

**Index lifecycle**:

```
Empty ──(first index)──▶ Indexing ──▶ Ready
                             │
Ready ──(changes detected)──▶ Refreshing ──▶ Ready
```

- `Refreshing` never serves partial state: queries run against the last
  fully consistent snapshot until the transaction commits (FR-004).
- `refresh_state` is observable so FR-013 status reporting can show
  "refreshing" with progress, and last_update time.

**Chunk lifecycle during refresh**:

| Outcome | Condition |
|---|---|
| unchanged | stored `content_hash` == recomputed hash → row untouched (no re-embed) |
| re-embedded | hash differs → new vector written in-transaction |
| new | file in `added`, or new chunk in a modified file |
| purged | file in `removed` (or chunk absent from re-parse) → rows deleted, cascading |
| migrated | file in `renamed` → old-path chunk_ids purged, new-path chunks created (end state identical to remove+add) |

**Consent lifecycle**: see ConsentRecord transitions above; absent file ≡
`NeverAcknowledged`.

**Degradation ladder**:

```
Dense + Keyword (RRF fused)  ──▶  Keyword-only
   (backend unconfigured | unreachable | no-consent-for-remote | RAG off
    | ModelFilesMissing | ModelFilesCorrupt)
```

`ModelFilesMissing` (model_dir absent or incomplete) and
`ModelFilesCorrupt` (SHA-256 mismatch / unloadable artifacts) land on the
keyword-only rung with `degradation_note`; placing verified artifacts
restores hybrid search with no further action.

`ProfileMismatch` never degrades silently — it takes the documented full
semantic rebuild path (§3 IndexMetadata): drop `rag_vectors`, re-embed
every chunk under the new profile; keyword index and typed graph
untouched.

Keyword-only results always carry `degradation_note` — explicit
indication, never a hard failure of the agent turn (FR-008).

## Validation Rules Cross-Reference

| Req | Enforced by |
|---|---|
| FR-001 | `CodeChunk` (symbol fields) + `RankedResult` denormalized reference; VectorRecord normalized dot-product scan |
| FR-002 | `RankedResult.fused_score` (RRF k=60) + `semantic_rank`/`keyword_rank`; exact-symbol rank via BM25 contribution |
| FR-003 | `SearchRequest.file_scope_filter` applied to chunk `source_path` before ranking |
| FR-004 | `content_hash` chunk-level skip + single-transaction swap + `refresh_state` + WAL snapshot reads |
| FR-005 | `ChangeDelta.removed` → DELETE with `ON DELETE CASCADE` to vectors/edges |
| FR-006 | `SearchRequest.expand_context_lines` + read-time clamp to file boundaries |
| FR-007 | `RelationshipLink` + `relation_depth ≤ 2` bound + visited-set dedup |
| FR-008 | Degradation ladder; `RankedResult.degradation_note` present iff keyword-only |
| FR-009 | All v3 objects empty and unused when `neurocode.rag.enabled=false`; migration is additive, no behavior change |
| FR-010 | `SearchRequest`/`RankedResult` are backend-agnostic value types shared by tool + slash-command surfaces |
| FR-011 | Migration never alters v2 tables; typed graph, tiers, staleness untouched |
| FR-012 | `ConsentRecord` invariant: only `Acknowledged` permits remote embedding calls |
| FR-013 | `rag_index_meta` (chunk_count, last_refresh_at, refresh_state) + ConsentRecord state |
| FR-014 | `chunk_kind` discriminator + fallback chunks + `RankedResult.chunk_kind` badge |
| FR-015 | Pre-fetch gating reads backend locality + consent; no new persisted entity (config-only) |
| FR-001 (profile pinning) | `rag_index_meta` profile identity (`embed_profile`, `embed_model`, `embed_dim`, `pooling`, `prefix_*`) enforced at embed and load — prefixes/pooling/dim always match the profile; mismatch → `ProfileMismatch` → full semantic rebuild (retrieval quality never silently mixes profiles) |
| FR-008 (artifact ladder rung) | `rag_model_artifacts` presence + integrity gate the degradation ladder: missing/corrupt artifacts (`ModelFilesMissing`/`ModelFilesCorrupt`) → keyword-only with indication, turn never fails |
| FR-012 (local stance/security) | Artifact SHA-256 integrity (`rag_model_artifacts` pinning; load refuses mismatches) + no-HF distribution (manual placement or project mirror only — never a Hugging Face fetch) |
| SC-001 | VectorRecord dim/BLOB integrity + fused ranking over both chunk populations |
| SC-002 | RRF fusion with keyword ranks preserving exact-symbol primacy |
| SC-003 | BLOB-backed exhaustive scan (R1 measurements) + `source_path`/`hash`/`artifact` indexes |
| SC-004 | content_hash skip + ChangeDelta-driven refresh touching only changed files |
| SC-005 | Additive migration + `enabled=false` leaves all v3 tables empty/unused (parity) |
| SC-006 | `RankedResult.expanded_context` (read-at-query-time, clamped) reducing follow-up file reads |
