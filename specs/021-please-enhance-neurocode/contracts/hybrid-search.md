# Contract: Hybrid Search Pipeline

**Spec**: [spec.md](../spec.md) | **Plan**: [plan.md](../plan.md) | **Research**: [research.md](../research.md) R1 | **Data model**: [data-model.md](../data-model.md)

The single blended search path (FR-001/002/003/006/007/008/014). Lives in
`crates/joey-neurocode-rag/src/search/`.

## Input / Output

- **Input**: `SearchRequest` per [data-model.md](../data-model.md) §5
  (query, file_scope_filter, limit, expand_context_lines,
  relation_depth, include_fallback_chunks).
- **Output**: `Vec<RankedResult>` (data-model §6) plus:

```rust
pub struct SearchDiagnostics {
    pub mode: SearchMode,               // Hybrid | KeywordOnly
    pub backend_health: BackendHealth,  // from health_check / last embed attempt
    pub refresh_state_at_query: RefreshState, // idle | refreshing (snapshot read)
    pub total_candidates: usize,        // pre-limit fused candidate count
}
```

## Pipeline stages (normative order)

1. **Validate query** — empty/whitespace-only after trim → validation
   error, NOT a search (edge case 1).
2. **Keyword leg** — existing FTS5 `query_fts`; the FTS5 bm25 rank is
   NEGATED (lower = better), convert to an **ascending 1-based ordinal**
   before fusion (R1).
3. **Dense leg** — embed the query via the backend; on ANY `EmbedError`
   → degrade to KeywordOnly with explicit indication (FR-008). Skip the
   leg entirely when RAG is disabled, unconfigured, or consent is
   absent/revoked for a remote backend (see
   [embedding-backend.md](./embedding-backend.md) consent gate).
4. **Fusion — RRF k=60**:

   ```
   score(d) = Σ_legs 1 / (60 + rank_leg(d))
   ```

   Rank order MUST be deterministic; tie-break: keyword rank first, then
   `chunk_id` lexical ascending.
5. **File-scope filter** — the glob filter is applied in BOTH legs
   BEFORE fusion (FR-003), never post-fusion.
6. **Top limit** — truncate to `SearchRequest.limit`.
7. **Context expansion** — read the file from disk; `± expand_context_lines`
   clamped to file start/end; a missing file keeps the result with a
   `context_absent` note, never an error (FR-006, R4).
8. **Relation expansion** — only when `relation_depth > 0`: follow
   `rag_chunk_edges` BFS to depth ≤ 2, dedup by `chunk_id` (visited set).
   Expanded items are marked with their `relation_kind` and **DO NOT
   displace fused results** — appended after them, capped by the limit
   budget for expansions (FR-007).
9. **Badge** — each result carries its `chunk_kind` badge
   (`symbol-aligned | fallback`) (FR-014).

## Exact-symbol guarantee (FR-002 / SC-002)

If the query, normalized (trimmed, casefolded), **exactly equals** a
`symbol_name` in `rag_chunks` within the active file scope, that chunk
MUST rank first in the final list — deterministically, ahead of all
fused-only competitors.

## Parity (FR-009 / SC-005)

When `neurocode.rag.enabled = false`, this pipeline MUST NOT exist on
any code path: no struct construction, no dense leg, no diagnostics
emission — existing keyword behavior is byte-identical to
pre-enhancement. Verified by parity test.

## Degradation semantics

KeywordOnly mode still serves results; every result carries
`degradation_note` and `semantic_rank = None`; the agent turn never
hard-fails (FR-008). `SearchDiagnostics.mode` reports the reason source
(backend health / consent / config).

## Test obligations

1. **RRF math unit test** — hand-computed fusion scores including the
   negated-bm25 → ascending-ordinal conversion and the deterministic
   tie-break.
2. **Exact-symbol-first test** — exact `symbol_name` query ranks the
   matching chunk first.
3. **Degradation test** — `EmbedError` of each taxonomy class yields
   KeywordOnly + indication, no error.
4. **Parity test** — disabled flag yields zero pipeline code path and
   unchanged keyword output.

---
Regression coverage: required by constitution Principle VII — tasks MUST include tests pinning this contract.
