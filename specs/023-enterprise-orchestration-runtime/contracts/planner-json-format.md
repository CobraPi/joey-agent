# Contract: Strict Planner JSON Format

New strict format for planning runs that use the execution graph. Legacy `<workstreams>` output remains parseable and is converted immediately (conservative conversion: objective = focus; read/write sets empty ⇒ "undeclared", routed SingleWorker, never concurrent).

```json
{
  "format": "joey-taskgraph/1",
  "baseline_revision": "<git sha the plan was made against>",
  "tasks": [
    {
      "id": "task-auth",
      "objective": "Implement token refresh",
      "dependencies": [],
      "read_set": ["crates/joey-core/src/auth.rs"],
      "write_set": ["crates/joey-core/src/auth.rs", "crates/joey-core/tests/auth.rs"],
      "artifact_ids": [42, 1337],
      "role": "implementor",
      "model_tier": "economical",
      "risk": "medium",
      "acceptance": [
        {"criterion": "cargo test -p joey-core auth", "kind": "command"}
      ],
      "verification": {
        "steps": [
          {"name": "scoped-tests", "command": "cargo test -p joey-core auth", "parse": "plain", "timeout_sec": 300, "required": true}
        ],
        "risk_triggered_review": false
      },
      "isolation": "isolated_worktree"
    }
  ]
}
```

Schema rules:
- `format` is required and must equal `joey-taskgraph/1`; unknown format strings are rejected.
- `id`: unique, non-empty, `[a-z0-9-]+`; referenced `dependencies` must exist (missing ⇒ rejection naming both ids).
- `model_tier`: `economical` | `frontier`.
- `risk`: `low` | `medium` | `high`.
- `role`: existing orchestration worker roles.
- `isolation`: `shared_checkout` (read-only workers) | `isolated_worktree` (writers; default when write_set is non-empty).
- All paths are project-relative, normalized, and must resolve inside the project root (`..` traversal rejected).
- Every task requires ≥1 acceptance criterion; `high` risk requires ≥1 required verification step or `risk_triggered_review: true`.
- Validation rejections identify the offending task id(s) and the violated rule (FR-010).
