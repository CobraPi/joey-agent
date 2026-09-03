# Contract: Run-State Format (on-disk)

Location: `~/.joey/hypercode/projects/<project-hash>/runs/<run-id>/` where `<project-hash>` is a stable hash of the project root path and `<run-id>` is a unique run identifier.

```text
runs/<run-id>/
├── graph.json          # full TaskGraph: nodes (with status/attempts), baseline_revision, run_id
├── nodes/<task-id>.json    # per-task state snapshot, rewritten atomically on each transition
├── evidence/<task-id>.json # immutable EvidenceRecord list per task
├── patches/<task-id>.patch # unified diff produced by the task's isolated worker
└── decisions.jsonl     # append-only decision log; one JSON line per entry
```

decisions.jsonl entry shape (one JSON object per line):
```json
{"ts": "2026-09-02T12:00:00Z", "run_id": "r-1", "task_id": "task-auth", "from": "Dispatched", "to": "Evaluating", "cause": "worker_completed", "evidence_ids": ["ev-7"], "detail": "..."}
```
Required `cause` vocabulary: `dependency_completed`, `worker_completed`, `gate_passed`, `gate_failed`, `repair_scheduled`, `escalated`, `degraded`, `override_acknowledged`, `replanned`, `deferred_concurrency_cap`, `conflict_sequenced`, `baseline_mismatch_abort`, `run_resumed`.

Rules:
- graph.json is rewritten atomically after every accepted transition; decisions.jsonl is append-only.
- Resume: persisted `baseline_revision` must equal the current baseline revision, else the run refuses to resume and stops with a report (FR-030). Completed tasks are never re-executed.
- The log plus graph.json must be sufficient to reconstruct why each task reached its final status (SC-007).
- No user-visible git history entries are created by run-state management (FR-018).
