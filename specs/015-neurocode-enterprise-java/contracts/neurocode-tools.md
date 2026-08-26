# Contract: NeuroCode Tools (agent-callable)

**Spec**: [spec.md](../spec.md) | **Plan**: [plan.md](../plan.md) | **Research**: [research.md](../research.md) §6

NeuroCode exposes four tools the agent can call directly, registered in
`joey_tools::builtins::register_all` (conditionally-enabled — `check()`
returns false when NeuroCode is disabled, so the model never sees them in
disabled state; FR-020). They implement the `joey_tools::Tool` trait.

## Tool 1: `neurocode_index`

Trigger or refresh the structural index for the target project.

```json
{
  "name": "neurocode_index",
  "description": "Index or re-index the current project's Java/Pega source into the NeuroCode structural graph. Runs asynchronously; returns immediately with a job id.",
  "parameters": {
    "type": "object",
    "properties": {
      "path": { "type": "string", "description": "Project root to index (defaults to cwd)" },
      "force": { "type": "boolean", "description": "Force full re-index even if incremental is available", "default": false }
    }
  }
}
```

- **Async**: returns a job id; ingestion runs off the hot path (FR-017).
- **Output**: `{ "job_id": "...", "status": "started", "artifacts_seen": <count> }`.
- **Toolset**: `coding` (grouped with other coding tools).

## Tool 2: `neurocode_query`

Query the structural dependency graph.

```json
{
  "name": "neurocode_query",
  "description": "Query the NeuroCode structural graph: what implements an interface, what injects a type, what references a rule.",
  "parameters": {
    "type": "object",
    "properties": {
      "query_type": { "type": "string", "enum": ["implements", "injects", "referenced_by", "references", "search"] },
      "symbol": { "type": "string", "description": "The symbol/FQCN to query about" },
      "limit": { "type": "integer", "default": 10 }
    },
    "required": ["query_type", "symbol"]
  }
}
```

- **Non-async, read-only**: reads from the local SQLite index (no network).
- **Output**: `{ "results": [{ "fqcn": "...", "kind": "...", "edges": [...] }] }`.
- **Toolset**: `coding`.
- Parallel-safe (read-only) — dispatched concurrently by the tool runtime.

## Tool 3: `neurocode_status`

Report NeuroCode's state for the current project.

```json
{
  "name": "neurocode_status",
  "description": "Report NeuroCode status: index state, tier config, detected Pega version, learned patterns count, domain knowledge sources.",
  "parameters": { "type": "object", "properties": {} }
}
```

- **Output**: `{ "enabled": true, "indexed": true, "artifact_count": 1234, "tier_config": {...}, "pega_version": "Infinity 24", "patterns": 12, "anti_patterns": 3, "domain_sources": [...] }`.
- **Toolset**: `coding`.
- Parallel-safe (read-only).

## Tool 4: `neurocode_ingest`

Ingest a domain-knowledge source (framework docs, entity catalog, postmortem).

```json
{
  "name": "neurocode_ingest",
  "description": "Ingest a domain-knowledge source into the NeuroCode retrieval graph. Categories: framework docs (version-specific), entity catalog (DTOs/entities), postmortem (historical incidents).",
  "parameters": {
    "type": "object",
    "properties": {
      "category": { "type": "string", "enum": ["framework_docs", "entity_catalog", "postmortem"] },
      "source_path": { "type": "string", "description": "Path to the file or directory to ingest" },
      "version_tag": { "type": "string", "description": "Version this knowledge applies to (for framework_docs)" },
      "provenance": { "type": "string", "description": "Human-readable source identifier" }
    },
    "required": ["category", "source_path", "provenance"]
  }
}
```

- **Async**: returns a job id; ingestion runs off the hot path.
- **Output**: `{ "job_id": "...", "status": "started", "category": "..." }`.
- **Toolset**: `coding`.

## Registration

All four tools are registered in `joey_tools::builtins::register_all` via a
new `register_neurocode_tools(registry, engine_handle)` function (matching
the pattern of `register_session_tools`, `register_clarify_tool`). Each
tool's `check()` returns `false` when the engine handle is `None` (disabled),
so they are invisible to the model in disabled state (FR-020).

No tool shadows an existing tool name (verified against the current
`register_all` registry — research.md §6).
