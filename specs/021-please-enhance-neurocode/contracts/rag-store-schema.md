# Contract: RAG Store Schema (graph.db v3)

**Spec**: [spec.md](../spec.md) | **Plan**: [plan.md](../plan.md) | **Research**: [research.md](../research.md) R1 | **Data model**: [data-model.md](../data-model.md)

The additive v2→v3 migration of the per-project SQLite `graph.db`
(`~/.joey/neurocode/projects/<sha256-of-root>/graph.db`). Column
semantics are defined in [data-model.md](../data-model.md); the SQL is
repeated here as the normative DDL.
**[data-model.md](../data-model.md) is AUTHORITATIVE for column
semantics and DDL**; this contract repeats the DDL for implementer
convenience and **MUST stay in sync** with data-model.md — any DDL
change lands there first, then is mirrored here.

## DDL (idempotent, additive)

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

-- Derived chunk-level edges (rebuildable; typed graph stays authoritative)
CREATE TABLE IF NOT EXISTS rag_chunk_edges (
    from_chunk_id TEXT NOT NULL,
    to_chunk_id   TEXT NOT NULL,
    edge_kind     TEXT NOT NULL,
    PRIMARY KEY (from_chunk_id, to_chunk_id, edge_kind)
);
```

## Migration guarantees

- Every statement is `CREATE ... IF NOT EXISTS` — the migration is
  **idempotent**; re-running on a v3 DB is a no-op.
- An existing **v2 DB opens unchanged**: no existing table is altered or
  dropped; it simply gains empty RAG tables. Keyword/graph behavior is
  identical until RAG is enabled (FR-009, FR-011).
- `NEUROCODE_SCHEMA_VERSION` constant bumps `2 → 3` in
  `crates/joey-neurocode/src/lib.rs`.
- Migration runs inside the normal open path; no export/import step.
- **Rollback story**: if the feature is disabled, the new tables simply
  remain empty and harmless; a pre-v3 binary that ignores them still
  functions (additive-only means no v2 reader breaks).

## Artifact integrity (`rag_model_artifacts`)

- **Manual placement self-registration**: when NO `rag_model_artifacts`
  row exists for a profile, the **first successful load** computes the
  SHA-256 of the present files and **WRITES the row**
  (self-registration). Manual placement cannot know project-recorded
  hashes — the rule is first-load trust + later immutability.
- **Every subsequent load verifies** the files against the stored row
  and **refuses on mismatch** (`ModelFilesCorrupt`) — the pinned row is
  immutable after registration.
- **`/neurocode model fetch` is stricter**: it writes the row from the
  project-recorded expected hashes at fetch time, refusing mismatched
  downloads before anything lands in `model_dir` (see
  [neurocode-rag-command.md](./neurocode-rag-command.md) § Model fetch
  subcommand).
- Rows are written by fetch or by first verified manual placement, and
  SHA-256 is re-verified before every embedder load.

## Atomicity (FR-004, clarification Q5)

- One refresh = **one transaction** writing chunks + vectors + edges +
  `rag_index_meta` updates. COMMIT is the swap point.
- WAL readers continue seeing the prior fully consistent snapshot until
  commit lands; no query ever observes torn or mixed old/new state.
- Purge of removed files' chunks cascades to vectors and derived edges
  via `ON DELETE CASCADE` — no orphans (FR-005).

## BLOB encoding (R1)

Vectors are stored **normalized**; cosine = plain dot product on decode.

| Encoding | Layout | Byte length |
|---|---|---|
| `f32` | `dim` little-endian IEEE-754 words | `dim × 4` |
| `int8` | one f32 scale prefix (4 bytes LE) + `dim` int8 codes (value ≈ code × scale) | `dim × 1 + 4` |

**Validation formula** (enforced at write and read): byte length MUST
equal `dim × 4` (f32) or `dim + 4` (int8); anything else is corrupt and
rejected. `dim` MUST equal `rag_index_meta.embed_dim` at write time.

## Regression-test obligations

1. **Schema round-trip**: write chunk + vector + edge + meta rows, reopen
   the DB, read back identical values (incl. BLOB byte-exactness).
2. **Migration-from-v2**: create a v2 DB with populated existing tables,
   run the v3 open path, assert existing rows intact AND new tables
   present AND nothing dropped/altered.
3. **Idempotent-reopen**: run the migration twice; assert no error and no
   duplicate rows.

---
Regression coverage: required by constitution Principle VII — tasks MUST include tests pinning this contract.
