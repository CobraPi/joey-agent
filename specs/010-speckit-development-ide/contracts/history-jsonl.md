# Contract: Workflow History JSONL (versioned on-disk format)

**Feature**: `010-speckit-development-ide` | **Implements**: FR-018, FR-019,
FR-031, FR-033

This is a **public on-disk format** (Constitution VII). `schema_version`
is mandatory on every record. A breaking change requires a MAJOR bump +
documented migration + round-trip test.

## Location

`~/.joey/speckit-ui/history/<feature-id>.jsonl` — one file per feature,
one self-contained attempt record per line (append-only). `JOEY_HOME`
override applies (AGENTS.md).

## Record: schema v1

Each line is a complete `WorkflowAttempt` (see `data-model.md` §5).
Ellipses (`…`) denote arrays whose element shapes are defined inline.

```json
{
  "schema_version": 1,
  "attempt_id": "uuid",
  "feature_id": "010-speckit-development-ide",
  "step_id": "plan",
  "initiator": "user",
  "started_at": "2026-08-03T12:00:00Z",
  "ended_at": "2026-08-03T12:02:14Z",
  "status": "succeeded",
  "run_config": {
    "step_id": "plan",
    "effective_instructions": "…",
    "scope": { "targets": [ { "path": "specs/010-…/plan.md", "kind": "plan" } ] },
    "options": { "model": "…", "reasoning_effort": "…", "max_iterations": 25 },
    "option_catalog_rev": "sha256:…",
    "change_mode": "staged",
    "override_id": null,
    "prepared_at": "2026-08-03T12:00:00Z"
  },
  "transcript": [
    { "kind": "progress", "text": "…", "at": "…" },
    { "kind": "tool", "name": "edit", "summary": "plan.md +12 -3", "at": "…" }
  ],
  "interactions": [
    { "interaction_id": "…", "kind": "question", "payload": { "prompt": "…" },
      "confirmed": true, "at": "…" }
  ],
  "changes": {
    "attempt_id": "…", "mode": "staged", "recovery_action": null,
    "files": [
      { "path": "specs/010-…/plan.md", "status": "modified",
        "additions": 12, "removals": 3, "why": "agent rewrote summary",
        "accept_state": "accepted",
        "hunks": [ { "hunk_id": "h1", "old_range": "10,5", "new_range": "10,8",
          "accept_state": "accepted", "depends_on": [] } ] }
    ]
  },
  "validation": [
    { "finding_id": "…", "severity": "Warning", "code": "unresolved_marker",
      "description": "NEEDS CLARIFICATION at line 21",
      "location": { "path": "specs/010-…/plan.md", "line_or_section": "21" },
      "remediation": "Resolve before /speckit-tasks." }
  ],
  "checkpoint": {
    "tree_ish": "sha1:<git-tree>",
    "last_confirmed_interaction_id": "…",
    "at": "2026-08-03T12:01:00Z"
  },
  "prior_attempt_id": null,
  "expires_at": "2026-11-01T12:00:00Z"
}
```

## Field semantics

- `schema_version: 1` — mandatory; absent/unknown ⇒ record skipped with a
  warning (tolerant parser, `model.rs::Status::Unparsed` philosophy).
- `status` — the persisted attempt status
  (`preparing|running|awaiting_input|awaiting_approval|recoverable_failure|
  conflicted|recovery_failed|succeeded|failed|cancelled|recovery_needed`).
  Presentation-only `attention_needed` is **never** persisted; it is
  derived by the UI (spec US2 note).
- `run_config` — frozen snapshot of the prepared configuration (immutable
  after prepare; data-model §4).
- `transcript` / `interactions` — the ordered run record (FR-012/013).
- `changes` — **summary only** (path + counts + hunk metadata + accept
  state). Full per-hunk line diffs are **not** duplicated here; they are
  resolved on demand from the Git `checkpoint.tree_ish` so each line stays
  bounded even for a 1 000-file change set (FR-031 / SC-010).
- `checkpoint` — latest safe recovery point (FR-033). Present while the
  attempt is in progress and after completion.
- `prior_attempt_id` — links re-runs for comparison (FR-019).
- `expires_at` — `started_at + 90d` (FR-018).

## Append / read / expiry

- **Append** (FR-018): O(1) `writeln!` to end-of-file. The in-progress
  attempt line is rewritten in place (temp + atomic rename of the whole
  file) only when a checkpoint advances; appends dominate.
- **Read** (SC-010): streamed, line-by-line via
  `serde_json::Deserializer::from_reader`; the history endpoint paginates
  (newest first) so a 100-attempt file is never fully buffered.
- **Expiry** (FR-018): a startup task + hourly tick scans
  `~/.joey/speckit-ui/history/*.jsonl`, rewrites files without records
  whose `expires_at` has passed (atomic temp + rename), and removes empty
  files. 90-day retention is a file/record mtime sweep — no SQL, no
  reindex.
- **Crash safety**: a partial last line is skipped on read (tolerant).

## Versioning & migration (Constitution VII)

- `schema_version` bumps are **additive** (new optional fields) ⇒ no
  migration needed; old readers ignore unknown fields, new readers handle
  absent fields via `#[serde(default)]`.
- A **breaking** change (renamed/removed field, changed semantics) ⇒
  MAJOR bump of the format major version + a documented migration in this
  file + a round-trip/migration test (`tests/history_jsonl_roundtrip.rs`).

## Non-goals

- Not a SQL DB / query engine (Constitution VIII — sequential append/read
  log; `research.md` §5).
- Not a per-attempt file (rejected — explodes file count, no natural
  ordering).
- Not a single pretty-printed JSON array (rejected — not append-safe, not
  streamable).
