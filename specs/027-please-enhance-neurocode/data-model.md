# Data Model: NeuroCode Adaptive Memory

Phase 1 output. Entities map to schema-v4 tables in the per-project `graph.db` (DDL and invariants: contracts/neurocode-memory-storage.md).

## MemoryEpisode (table `memory_episodes`)

One concrete work event — the episodic memory (spec Key Entities). Unit: one per completed interactive task, one per completed orchestrated workstream/subtask, success or failure (clarified Q2).

| Field | Type | Rules |
|-------|------|-------|
| id | TEXT PK | stable unique id (ULID-style timestamp+random, assigned at capture) |
| kind | TEXT | `task` (interactive) or `workstream` (orchestrated) — CHECK constraint |
| title | TEXT | short human title, ≤ 200 chars |
| task | TEXT | what was asked / the workstream focus, ≤ 4 KB |
| context | TEXT | relevant project context (paths, symbols), ≤ 4 KB |
| approach | TEXT | what was done / how, ≤ 4 KB |
| outcome | TEXT | `success` \| `failure` \| `partial` — CHECK constraint |
| lessons | TEXT | distilled lesson(s), may be empty, ≤ 4 KB |
| source | TEXT | `interactive` \| `hypercode` — CHECK constraint |
| origin_run | TEXT | hypercode run id or session key; empty for ad-hoc interactive turns |
| evidence_ids | TEXT | JSON array (e.g. `runs/<run>/nodes/<id>.json`, turn references) |
| created_at | TEXT | RFC 3339 |
| updated_at | TEXT | RFC 3339 |

Lifecycle: immutable after write (no state machine); eviction = FIFO delete (row + vector cascade) when `max_episodes` exceeded. Validation: `task` non-empty; `outcome` ∈ enum; text fields redacted (R11) and length-capped before insert.

## MemoryPreference (table `memory_preferences`)

A generalized preference/convention — the semantic memory; the applied layer that shapes code output.

| Field | Type | Rules |
|-------|------|-------|
| id | TEXT PK | stable unique id |
| category | TEXT | e.g. `error-handling`, `naming`, `structure`, `libraries`, `formatting`; free-form slug |
| statement | TEXT | the preference, ≤ 1 KB, non-empty |
| origin | TEXT | `explicit` (user said it) \| `inferred` (distilled) — CHECK constraint |
| evidence_ids | TEXT | JSON array of episode ids supporting it |
| status | TEXT | `active` \| `superseded` — CHECK constraint |
| supersedes | TEXT NULL | id of the preference this one replaced |
| superseded_by | TEXT NULL | set when superseded |
| confidence | INTEGER | 0..=100 |
| created_at / updated_at | TEXT | RFC 3339; `updated_at` refreshed on strengthening |

State transitions: `active → superseded` (deterministic rank: explicit > inferred, then recency — R7; sets `superseded_by`, backfills `supersedes` on the winner); `active/superseded → deleted` (hard delete row + vector, SC-004 never-reappear). No transition back to `active` except via explicit user `correct` (writes a new revision as a new row, superseding the old).

## MemoryVector (table `memory_vectors`)

Embedding storage for retrieval — same BLOB encoding as `rag_vectors` (f32 or int8, `vector/quantize.rs`).

| Field | Type | Rules |
|-------|------|-------|
| item_id | TEXT PK | episode or preference id; ON DELETE CASCADE from owner table |
| item_kind | TEXT | `episode` \| `preference` — CHECK constraint |
| dim | INTEGER | matches embedding profile |
| quantization | TEXT | `f32` \| `int8` — CHECK constraint |
| vector | BLOB | quantized embedding |

Episode embedding input = title + task + approach + lessons; preference embedding input = statement (+ category). Query-side uses the profile query prefix, document-side the document prefix (embed/profiles.rs) — prefixes applied at embed time only, never persisted into hashes.

## Relationships

- MemoryPreference.evidence_ids → MemoryEpisode.id (0..n; explicit preferences may predate any episode)
- MemoryVector.item_id → MemoryEpisode.id | MemoryPreference.id (cascade delete)
- MemoryEpisode.origin_run → hypercode run artifacts (opaque reference, no FK)
- Code index (`rag_chunks`/`rag_vectors`) is fully isolated: no foreign keys cross the memory/code boundary in either direction.

## Validation rules (from requirements)

- FR-001/FR-002 field capture as above; FR-008/FR-009 enforced by the supersede rank + transitions; FR-011 by redaction before insert; FR-012 by the per-project DB boundary; SC-004 by hard-delete cascade.
