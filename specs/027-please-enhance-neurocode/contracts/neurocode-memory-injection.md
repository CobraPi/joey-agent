# Contract: memory injection + hypercode integration

## Interactive injection (Agent)

- Point: the effective-system-prompt assembly beside the RAG prefetch block (`apply_rag_prefetch` precedent); content appended only when `neurocode.memory.enabled` and the store is non-empty.
- Content: top-k (default 5) active preferences ordered by (origin explicit first, then recency) plus top-k relevant episodes by similarity to the current prompt — top_k applies PER SECTION (up to top_k preferences AND up to top_k episodes, not a shared budget; analyze A1); compact text block, hard-capped at `injection_char_limit` (default 2048) — truncation drops whole entries, never mid-sentence.
- Budget: ≤ 2s p95 added to first response (SC-003); retrieval = one query embedding + dense scan + fusion via the memory retrieval leg; on any retrieval error the block is silently omitted (turn never fails — mirrors degraded RAG FR-008 behavior).
- Capture (write side): post-turn at the `neurocode_auto_reindex` exit-path call-sites; skipped entirely when disabled, when the turn produced no completed task, or when redacted episode text would be empty.

## Hypercode integration

- Write: in `finalize_graph_run`, beside `record_verified_outcomes` — one `kind=workstream` episode per workstream: task from workstream focus, outcome from the run's success flag (`success`/`failure`), approach+lessons from the build summary, evidence referencing the run node artifacts; gated post-verification exactly like outcome recording (completed workstreams only).
- Read: in `HypercodeDispatcher::dispatch`, the goal prefix gains (after the existing verified-lessons prefix) two bounded sections: active preferences relevant to the task objective, and relevant past episodes — same top_k and char budget, retrieval leg shared with interactive injection.
- Disabled (`neurocode.memory.enabled=false`): both sides are no-ops; hypercode behavior byte-identical to today (regression test required).

## Distillation continuity (Q1/Q3)

- Explicit preference statements detected in user text at capture time -> written synchronously (origin=explicit, no gate, no delay).
- Per-episode inferred distillation -> spawned immediately at capture (continuous), one economical-tier call, `MemoryDistiller` trait with provider impl wired in `joey-cli`; heuristic recurrence strengthening (cosine ≥ 0.92 same-category) needs no model call.
