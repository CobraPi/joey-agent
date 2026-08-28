# Contract: Agent Tools (neurocode_rag)

**Spec**: [spec.md](../spec.md) | **Plan**: [plan.md](../plan.md) | **Data model**: [data-model.md](../data-model.md)

The agent-facing tool surface for semantic search. Registered explicitly
in `joey_tools::builtins` — the repo has NO auto-discovery; a tool not
wired there does not exist.

## New tool: `neurocode_search`

Registered **ONLY when `neurocode.rag.enabled == true`** — absent from
the registry otherwise (preserving FR-009 parity: the model never sees it
in disabled state).

```json
{
  "name": "neurocode_search",
  "description": "Semantic code search over the indexed project: natural-language and exact-symbol queries return ranked code locations with surrounding context and optional related entities.",
  "parameters": {
    "type": "object",
    "properties": {
      "query":        { "type": "string",  "description": "Natural language and/or exact symbol names" },
      "file_filter":  { "type": "string",  "description": "Glob restricting results to matching file paths" },
      "limit":        { "type": "integer", "description": "Max results (default: neurocode.rag.top_k)", "maximum": 50 },
      "expand_lines": { "type": "integer", "description": "± context lines (default: neurocode.rag.context_window_lines)", "maximum": 200 },
      "relation_depth": { "type": "integer", "description": "Relationship expansion depth 0-2", "minimum": 0, "maximum": 2 }
    },
    "required": ["query"]
  }
}
```

**Return payload:**

```json
{
  "results": [
    {
      "file": "src/auth/token.rs",
      "symbol": "validate_token",
      "kind": "method",
      "lines": [42, 87],
      "chunk_kind": "symbol-aligned",
      "score": 0.032786,
      "context": "fn validate_token(...) { ... }",
      "relations": [ { "file": "...", "symbol": "...", "relation_kind": "Injects" } ]
    }
  ],
  "mode": "hybrid",
  "mode_reason": null
}
```

- `mode` ∈ `hybrid | keyword_only`; `keyword_only` carries a
  `mode_reason` (backend health / consent / config) — the FR-008
  explicit degradation indication.
- `relations` present only when `relation_depth > 0`.
- **Errors**: empty/whitespace `query` → validation error. Backend
  errors NEVER hard-fail the turn — the tool degrades to `keyword_only`
  results (FR-008).
- Toolset: `coding`; parallel-safe (read-only over the snapshot) —
  dispatched concurrently by the tool runtime.

## Existing tool: `neurocode_status` (additive extension)

When RAG is **enabled**, output gains a `rag` section:

```json
"rag": {
  "index_state": "ready",
  "chunk_count": 1234,
  "vector_count": 1230,
  "model": "nomic-embed-text",
  "dim": 768,
  "quantization": "f32",
  "last_refresh_at": "2026-08-27T14:22:09Z",
  "backend_health": "reachable",
  "consent_state": "never_acknowledged",
  "degradation": []
}
```

(field list normative; values illustrative). When RAG is **disabled**,
output MUST be **byte-identical to today** — the `rag` key does not
appear at all (FR-009 parity; a present-but-empty section is explicitly
REJECTED).

## Existing tools unchanged

`neurocode_index`, `neurocode_query`, `neurocode_ingest` keep their
schemas and behavior exactly as-is (FR-011).

## Registration point

`joey_tools::builtins::register_all` via a `register_neurocode_rag_tools`
function matching the established `register_*` pattern; the enabled-flag
check happens at registration time (absent when disabled), not merely at
`check()` time.

## Test obligations

1. **Tool schema pinned** — the JSON above asserted exactly.
2. **Parity** — with RAG disabled, the registry is identical to
   pre-enhancement (no `neurocode_search`; `neurocode_status` output
   byte-identical).
3. **Degradation result shape** — backend failure yields the
   `keyword_only` payload with `mode_reason`, not an error.

---
Regression coverage: required by constitution Principle VII — tasks MUST include tests pinning this contract.
