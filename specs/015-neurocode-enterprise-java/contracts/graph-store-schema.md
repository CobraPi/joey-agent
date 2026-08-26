# Contract: Graph Store Schema (graph.db)

**Spec**: [spec.md](../spec.md) | **Plan**: [plan.md](../plan.md) | **Research**: [research.md](../research.md) §2, §8

The on-disk SQLite schema for the NeuroCode structural knowledge graph. This
is a **new, versioned** public format, separate from the session DB
(`SCHEMA_VERSION = 22` in `joey-core::state`). Stored at
`~/.joey/neurocode/projects/<project-hash>/graph.db`.

## Schema versioning

- `neurocode_schema_version: 1` (this document defines v1).
- A `schema_meta` table stores the version row: `(key TEXT PRIMARY KEY, value TEXT)`.
- Any breaking change to this schema requires incrementing
  `neurocode_schema_version` with a documented migration path
  (Constitution VII — versioned on-disk public format).

## Tables (v1)

### `code_artifacts`

```sql
CREATE TABLE code_artifacts (
    id              INTEGER PRIMARY KEY,
    kind            TEXT NOT NULL,           -- 'Class'|'Interface'|'Enum'|'Method'|'Field'|'PegaRule'
    fqcn            TEXT NOT NULL,           -- fully-qualified canonical name
    enclosing_type  TEXT,                    -- for methods/fields
    package         TEXT NOT NULL,
    implemented_interfaces TEXT,             -- JSON array of strings
    annotations     TEXT,                    -- JSON array of strings
    declared_dependencies TEXT,              -- JSON array of strings
    source_path     TEXT NOT NULL,
    source_span_start INTEGER,               -- byte offset (tree-sitter)
    source_span_end   INTEGER,
    pega_metadata   TEXT,                    -- JSON PegaMetadata, nullable
    framework_version TEXT,
    status          TEXT NOT NULL DEFAULT 'Active',  -- 'Active'|'Stale'|'Deleted'
    indexed_at      TEXT NOT NULL,           -- ISO-8601
    UNIQUE(fqcn, kind, source_path)
);
CREATE INDEX idx_artifacts_enclosing ON code_artifacts(enclosing_type);
CREATE INDEX idx_artifacts_package ON code_artifacts(package);
CREATE INDEX idx_artifacts_status ON code_artifacts(status);
```

### `graph_edges`

```sql
CREATE TABLE graph_edges (
    from_id    INTEGER NOT NULL REFERENCES code_artifacts(id),
    to_id      INTEGER NOT NULL REFERENCES code_artifacts(id),
    edge_kind  TEXT NOT NULL,               -- 'Implements'|'IsImplementedBy'|'Injects'|'ExchangesType'|'ReferencesRule'|'InheritsRule'
    PRIMARY KEY (from_id, to_id, edge_kind)
);
CREATE INDEX idx_edges_from ON graph_edges(from_id, edge_kind);
CREATE INDEX idx_edges_to ON graph_edges(to_id, edge_kind);
```

### `code_artifacts_fts` (FTS5 virtual table)

```sql
CREATE VIRTUAL TABLE code_artifacts_fts USING fts5(
    fqcn,
    enclosing_type,
    package,
    annotations,
    declared_dependencies,
    content='code_artifacts',
    content_rowid='id',
    tokenize='unicode61'    -- handles camelCase/symbols adequately for v1
);
-- triggers to keep FTS in sync (standard external-content pattern)
```

FTS5 provides BM25-ranked symbol search (research.md §2). The `unicode61`
tokenizer splits on non-alphanumeric boundaries, giving reasonable
camelCase/snake_case coverage for v1. A custom tokenizer can be added later
(non-breaking — additive to the FTS table).

### `patterns` (LearnedPattern)

```sql
CREATE TABLE patterns (
    id                INTEGER PRIMARY KEY,
    prompt_signature  TEXT NOT NULL,
    generation_summary TEXT NOT NULL,
    verify_result     TEXT NOT NULL,        -- JSON VerifyResult
    artifact_ids      TEXT NOT NULL,        -- JSON array of ArtifactId
    tier              TEXT NOT NULL,
    created_at        TEXT NOT NULL
);
CREATE INDEX idx_patterns_signature ON patterns(prompt_signature);
CREATE INDEX idx_patterns_artifacts ON patterns(artifact_ids);
```

### `anti_patterns` (LearnedAntiPattern)

```sql
CREATE TABLE anti_patterns (
    id              INTEGER PRIMARY KEY,
    error_signature TEXT NOT NULL,
    error_output    TEXT NOT NULL,
    resolution      TEXT NOT NULL,
    artifact_ids    TEXT NOT NULL,          -- JSON array of ArtifactId
    created_at      TEXT NOT NULL,
    hit_count       INTEGER NOT NULL DEFAULT 0,
    status          TEXT NOT NULL DEFAULT 'Active'  -- 'Active'|'Resolved'
);
CREATE INDEX idx_anti_artifacts ON anti_patterns(artifact_ids);
CREATE INDEX idx_anti_signature ON anti_patterns(error_signature);
```

### `domain_knowledge` (DomainKnowledgeSource)

```sql
CREATE TABLE domain_knowledge (
    id           INTEGER PRIMARY KEY,
    category     TEXT NOT NULL,             -- 'FrameworkDocs'|'EntityCatalog'|'Postmortem'
    source_path  TEXT NOT NULL,
    version_tag  TEXT,
    provenance   TEXT NOT NULL,
    ingested_at  TEXT NOT NULL,
    fts_indexed  INTEGER NOT NULL DEFAULT 1
);
```

### `domain_knowledge_fts` (FTS5 virtual table)

```sql
CREATE VIRTUAL TABLE domain_knowledge_fts USING fts5(
    content,
    provenance,
    version_tag,
    tokenize='unicode61'
);
```

## Concurrency

Each `graph.db` is opened by one agent process at a time (per-project).
Writes (ingestion, pattern recording) use SQLite `WAL` mode +
`atomic_json_write`-style discipline (the workspace's existing atomic-replace
primitive is reused for the `meta.json` sidecar). Reads (context assembly,
graph queries) are read-only and never block.

## Migration policy

- v1 → vN: additive changes (new columns with defaults, new tables, new index)
  are non-breaking and applied with `ALTER TABLE` / `CREATE TABLE IF NOT
  EXISTS` on open, bumping `neurocode_schema_version`.
- A column type change or column removal is a breaking change requiring a
  MAJOR version bump and a documented migration function in `joey-neurocode`.
