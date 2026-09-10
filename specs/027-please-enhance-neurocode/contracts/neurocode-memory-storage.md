# Contract: memory storage (schema v4)

Per-project `graph.db`; `NEUROCODE_SCHEMA_VERSION` 3 -> 4; additive-idempotent batch appended to `apply_schema` (v2->v3 pattern: pure `CREATE TABLE IF NOT EXISTS`, no column changes to existing tables, version row in `schema_meta` updated).

## DDL (normative)

```sql
CREATE TABLE IF NOT EXISTS memory_episodes (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('task','workstream')),
  title TEXT NOT NULL,
  task TEXT NOT NULL,
  context TEXT NOT NULL DEFAULT '',
  approach TEXT NOT NULL DEFAULT '',
  outcome TEXT NOT NULL CHECK (outcome IN ('success','failure','partial')),
  lessons TEXT NOT NULL DEFAULT '',
  source TEXT NOT NULL CHECK (source IN ('interactive','hypercode')),
  origin_run TEXT NOT NULL DEFAULT '',
  evidence_ids TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memory_preferences (
  id TEXT PRIMARY KEY,
  category TEXT NOT NULL,
  statement TEXT NOT NULL,
  origin TEXT NOT NULL CHECK (origin IN ('explicit','inferred')),
  evidence_ids TEXT NOT NULL DEFAULT '[]',
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','superseded')),
  supersedes TEXT,
  superseded_by TEXT,
  confidence INTEGER NOT NULL DEFAULT 50 CHECK (confidence BETWEEN 0 AND 100),
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS memory_vectors (
  item_id TEXT PRIMARY KEY,
  item_kind TEXT NOT NULL CHECK (item_kind IN ('episode','preference')),
  dim INTEGER NOT NULL,
  quantization TEXT NOT NULL CHECK (quantization IN ('f32','int8')),
  vector BLOB NOT NULL
);
CREATE INDEX IF NOT EXISTS memory_episodes_created ON memory_episodes(created_at);
CREATE INDEX IF NOT EXISTS memory_preferences_status ON memory_preferences(status, category);
CREATE INDEX IF NOT EXISTS memory_vectors_kind ON memory_vectors(item_kind);
```

## Invariants

- An existing v3 DB opens unchanged and gains empty memory tables; re-opening is a no-op (idempotency) — pinned by tests mirroring `rag_schema_migration.rs`.
- Deleting an episode/preference row must cascade to its `memory_vectors` row (application-level cascade in one transaction, mirroring the `rag_chunks`->`rag_vectors` cascade test).
- Vector BLOB encoding is byte-identical to `rag_vectors` (f32 LE / int8) — round-trip byte-exact test required.
- No FK or shared row crosses into `rag_*`/code tables in either direction.
- `schema_meta.neurocode_schema_version` reads `4` after migration, exactly once (idempotent upsert).
