# joey-speckit-ui — visual UI over spec-kit artifacts

`joey-speckit-ui` is the local backend for the SpecKit Visual UI: an axum
HTTP + WebSocket server that parses the spec-kit artifacts under a
repository's `specs/<feature>/` directories (`spec.md`, `plan.md`,
`tasks.md`, plus checklists/research/data-models/contracts/quickstarts) into
a typed model, serves that model to a web frontend, performs conflict-checked
writes back to the same files, watches them for external changes, and drives
the spec-kit lifecycle (`clarify`/`analyze`/`implement`) by shelling out to
the repo's own `.specify/scripts/bash/*.sh` scripts — never by
reimplementing SpecKit. It binds to `127.0.0.1` only, stores all mutable
non-content state (history, preferences, UI state) under `~/.joey/speckit-ui/`,
and is launched for users via `joey speckit --port 4173`.

> See also: [../speckit-ui.md](../speckit-ui.md), [../speckit-workflow.md](../speckit-workflow.md), [../speckit-ui-launcher.md](../speckit-ui-launcher.md)

## Overview

- 41 `.rs` files under `src/` (39 library modules plus `main.rs` and
  `lib.rs`), 13,261 LOC, plus 19 test files under `tests/` (18 test crates +
  a shared `common/mod.rs`).
- Produces one binary, `joey-speckit-ui`. End users don't run it directly:
  `joey speckit --port 4173` (from `joey-cli`'s `speckit_cmd.rs`) spawns it
  with `JOEY_SPECKIT_UI_ROOT=<repo>` and `JOEY_SPECKIT_UI_PORT=<port>`
  (default 4173). Both env vars can also be set manually.
- **The core rule:** the `.specify/` + `specs/` files are the single source
  of truth. The UI reads them, writes them through hash-checked surgical
  edits, and re-derives everything else (semantic graph, coverage, workflow
  readiness) from them on every change. It never diverges into UI-only
  state; the only things persisted outside the repo are non-content state
  (history, preferences, layout) under `~/.joey/speckit-ui/`, and the
  ui-state store refuses to write inside any `specs/` directory
  (`ui_state::is_write_tree_isolated`).
- On startup (`main.rs`) the server starts an hourly history expiry sweep
  and a restart-recovery scan that resumes or fails in-progress workflow
  attempts found in the JSONL history.
- The `joey_home()` override chain is `JOEY_HOME` env var, else
  `~/.joey` (same convention as `joey-core`; see
  [joey-core.md](joey-core.md)).

## Module map

| File(s) | Role |
|---|---|
| `src/main.rs` | Binary entry: tracing init, repo root + port from env, history sweep + recovery spawns, binds `127.0.0.1:<port>`, serves the axum router. |
| `src/lib.rs` | Crate root. `load_feature` / `list_feature_ids`, `.specify/feature.json` active-feature reader, `AppState` (run channels, overrides, preferences, active attempts, FR-015 conflict guard via `paths_overlap`). |
| `src/model.rs` | The whole typed data model (see Data model below), incl. Feature-010 additive types. |
| `src/commands.rs` | Thin subprocess wrappers for `clarify`/`analyze`/`implement`/`init`: prefer `.specify/scripts/bash/<name>.sh`, fall back to the `specify` CLI. |
| `src/conflict.rs` | SHA-256 `content_hash` (`sha256:<hex>`) + `check_conflict` — the optimistic-concurrency primitive every write flows through. |
| `src/editor.rs` | Multi-artifact conflict-safe writes composing `writer.rs`; `whole` and `section` scopes; 409 on external change. |
| `src/history.rs` | Append-only JSONL attempt history under `~/.joey/speckit-ui/history/`, `schema_version: 1` gate, expiry sweep, overlay record kinds. |
| `src/recovery.rs` | Restart recovery: scan in-progress attempts, resume from valid checkpoint or mark `recovery_failed` preserving effects. |
| `src/runner.rs` | `WorkflowRunner` trait — the out-of-process runner contract (Constitution VI: no in-process link to `joey-agent-core`). |
| `src/runner_impl.rs` | Concrete runner: spawns `joey /speckit-<step>` (or the bash script fallback) with piped stdio, classifies output into `RunnerEvent`s, forwards interaction answers to stdin. |
| `src/staging.rs` | `StagingArea` trait — git-backed staging contract for staged/direct change modes. |
| `src/staging_impl.rs` | Git-backed staging: `gix` for read/object side, `git` CLI for worktree lifecycle and `git apply --reject`; staged mode = temp worktree on `joey/staging/<feature>/<attempt>`. |
| `src/ui_state.rs` | Per-repo+branch UI-state JSON (`schema_version: 1`), atomic save/load, write-tree isolation check, overlay record types (`AcceptedClarifyRecord`, `CommentThreadRecord`). |
| `src/validation.rs` | Required-structure + unresolved-marker validation per `ArtifactKind`; findings anchored to `ArtifactLocation`. |
| `src/watcher.rs` | Debounced (~500 ms) fs watcher for `spec.md`/`plan.md`/`tasks.md`; one debouncer per distinct feature dir, shared across WS connections. |
| `src/workflow.rs` | Workflow step catalog + readiness (`StepState`) derivation, dependency graph + stale propagation. |
| `src/writer.rs` | Conflict-checked single-line writes (`replace_line_if_unchanged`) to feature Markdown files. |
| `src/api/mod.rs` | Router assembly combining `rest` + `ws`. |
| `src/api/rest.rs` | 46 REST routes (see REST API below). |
| `src/api/ws.rs` | 5 WebSocket routes: file watch, clarify session, task run, attempt stream, meaning stream. |
| `src/cst/mod.rs` | Lossless CST types (`CstDocument`, `CstNode`, `CstKind`, `NodeId`); nodes partition `[0, file_len)` with no gaps. |
| `src/cst/parser.rs` | CST parser built on `pulldown-cmark`'s `OffsetIter`; always total, never drops bytes; ≤400 ms p95 for a 200-task file. |
| `src/cst/anchors.rs` | Per-node byte anchors: `byte_start`/`byte_end` (UTF-8), `expected_bytes`, `revision_hash` (SHA-256), `fingerprint`. |
| `src/cst/fingerprint.rs` | Structural fingerprint per `CstKind` + semantic id (e.g. `requirement/FR-016`); used by merge pairing and UI re-binding. |
| `src/cst/parser_trait.rs` | Parser + materializer traits — the narrow Truth↔Meaning interface. |
| `src/meaning/mod.rs` | `SemanticGraph` types; derived, in-memory only, never persisted, never a source of truth. |
| `src/meaning/graph.rs` | Graph builder: nodes + edges (traceability spine, coverage, containment, dependency, proposed-entity-relationship). |
| `src/meaning/mapping.rs` | Markdown-construct → `SemanticKind` mapping catalog; pure function, no I/O. |
| `src/meaning/coverage.rs` | Defect detection: `OrphanRequirement`, `RogueTask`, `Unverified`, `ConstitutionBreach`, each with `Scaffold` + optional `GenerativeFollowon`. |
| `src/meaning/cache.rs` | In-memory per-feature semantic cache, invalidated by watcher events, lazy recompute (≤400 ms budget). |
| `src/parser/mod.rs` | Parser module root. |
| `src/parser/spec.rs` | Tolerant line-scan parser for `spec.md` → `Specification`. |
| `src/parser/plan.rs` | Parser for `plan.md` → `Plan` incl. the Constitution Check gate table. |
| `src/parser/tasks.rs` | Parser for `tasks.md` checkbox lines → `Vec<Task>`. |
| `src/parser/discovery.rs` | Discovery + tolerant parsing of artifacts beyond the spec/plan/tasks trio. |
| `src/patch/mod.rs` | Patch engine root: `PatchOp`, `PatchResult`; the single point that writes accepted edits. |
| `src/patch/guard.rs` | Before-write verification of `revision_hash` + `expected_bytes` per targeted node; routes to `PatchResult::Conflict`. |
| `src/patch/merge.rs` | Three-way merge at semantic-block (CST node) level; pairs by fingerprint; `<500 ms` for a 200-task file. |
| `src/patch/node_lock.rs` | Per-node locking for concurrent developer/agent edits; locked nodes divert agent output to the review pane. |
| `src/patch/surgical.rs` | Applies `PatchOp::{Replace, InsertAfter, Delete}` to a temp buffer so only the edited node's range changes. |
| `src/patch/transaction.rs` | Temp buffer → re-parse → validation → atomic rename → verified inverse `undo: Vec<PatchOp>`. |

## Data model

All in `src/model.rs`, `serde`-derived. Parsing is tolerant: malformed
markers become `Unparsed` rather than panicking or being dropped.

Core enums:

| Enum | Variants | Serde / default |
|---|---|---|
| `Status` | `Draft`, `InProgress`, `Completed`, `Approved`, `Unparsed` | `PascalCase`, default `Unparsed` |
| `TaskStatus` | `Todo`, `InProgress`, `Done`, `Unparsed` | `snake_case`, default `Unparsed` |
| `GateResult` | `Pass`, `Fail`, `Unparsed` | `PascalCase`, default `Unparsed` |
| `Severity` | `Info`, `Warning`, `Critical`, `Unparsed` | `PascalCase`, default `Unparsed` |

Core structs:

| Struct | Fields |
|---|---|
| `Feature` | `id`, `directory`, `branch_name`, `specification`, `plan`, `tasks`, `missing`, `spec_content_hash`, `plan_content_hash`, `tasks_content_hash` |
| `Specification` | `title`, `created`, `status`, `user_stories`, `requirements`, `clarifications`, `key_entities`, `success_criteria` |
| `UserStory` | `id`, `title`, `priority`, `acceptance_scenarios`, `status` |
| `Requirement` | `id`, `text`, `user_story_ref` |
| `ClarificationEntry` | `session_date`, `question`, `answer` |
| `Plan` | `summary`, `technical_context`, `constitution_gates` |
| `ConstitutionGate` | `principle`, `result` (`GateResult`), `notes` |
| `Task` | `id`, `parallel_eligible`, `description`, `target_files`, `status`, `user_story_ref` |
| `AnalysisFinding` | `severity`, `description`, `target_file`, `target_line_or_section` |

Feature-010 additive types (strictly additive per Constitution VII):

| Type | Notes |
|---|---|
| `ArtifactKind` (10) | `spec`, `plan`, `tasks`, `checklist`, `research`, `data_model`, `contract`, `quickstart`, `constitution`, `supporting`; `workflow_phase()` maps each to its owning phase. |
| `WorkflowPhase` (9) | `constitution`, `specify`, `clarify`, `plan`, `checklist`, `tasks`, `analyze`, `implement`, `supporting`. |
| `SaveState` (8) | `clean`, `dirty`, `saving`, `saved`, `invalid`, `externally_changed`, `read_only`, `unparsed`. |
| `Artifact` | `path`, `kind`, `exists`, `content_hash`, `dirty`, `save_state`, `validity`, `workflow_phase`, `stale`, `stale_reason`. |
| `ValidationFinding` | `finding_id`, `severity`, `code`, `description`, `location`, `remediation`. |
| `ArtifactLocation` | `path`, `line_or_section`. |
| `WorkflowStep` | `id`, `order`, `purpose`, `inputs`/`outputs` (`ArtifactRef`), `prerequisites`, `available`, `state`, `blocking_reason`, `latest_attempt_id`, `installed_definition_ref`. |
| `StepState` (9) | `ready`, `blocked`, `running`, `attention_needed`, `succeeded`, `failed`, `stale`, `unavailable`, `unparsed`. |
| `RunConfiguration` | `step_id`, `effective_instructions`, `scope`, `options`, `option_catalog_rev`, `change_mode`, `override_id`, `prepared_at` — immutable after prepare. |
| `Scope` | `targets` (`Vec<ArtifactRef>`), `task_ids`. |
| `AgentOptions` | `model`, `reasoning_effort`, `max_iterations` (all optional). |
| `ChangeMode` | `staged` or `direct` — mandatory explicit selection every run. |
| `WorkflowAttempt` | `attempt_id`, `feature_id`, `step_id`, `initiator`, `started_at`, `ended_at`, `status`, `run_config`, `transcript`, `interactions`, `changes`, `validation`, `checkpoint`, `prior_attempt_id`, `expires_at`. |
| `AttemptStatus` (11) | `preparing`, `running`, `awaiting_input`, `awaiting_approval`, `recoverable_failure`, `conflicted`, `recovery_failed`, `succeeded`, `failed`, `cancelled`, `recovery_needed` (+ `unparsed` default). |
| `TranscriptEntry` | `kind`, `text`, `name`, `summary`, `at`. |
| `AgentInteraction` | `interaction_id`, `attempt_id`, `kind`, `payload` (JSON), `confirmed`, `at`. |
| `InteractionKind` (6) | `question`, `answer`, `approval_request`, `approval_decision`, `progress`, `tool_activity` (+ `unparsed`). |
| `ChangeSet` | `attempt_id`, `files`, `mode`, `recovery_action`. |
| `ChangedFile` | `path`, `status`, `additions`, `removals`, `why`, `hunks`, `accept_state`. |
| `FileChangeStatus` | `added`, `modified`, `removed`, `unparsed`. |
| `Hunk` | `hunk_id`, `old_range`, `new_range`, `accept_state`, `depends_on`. |
| `AcceptState` | `pending`, `accepted`, `rejected`, `unparsed`. |
| `DependencyLink` | `from`/`to` (`ArtifactLocation`), `kind`. |
| `DependencyKind` (5) | `requirement_to_plan`, `plan_to_task`, `task_to_attempt`, `attempt_to_finding`, `artifact_to_step_output` (+ `unparsed`). |
| `WorkspacePreference` | `last_feature_id`, `open_artifacts`, `active_view`, `pane_layout`, `filters`. |
| `Checkpoint` | `tree_ish`, `last_confirmed_interaction_id`, `at`. |

`AppState::options_catalog()` advertises the run-options catalog (FR-010):
models `claude-sonnet-4-5` / `gpt-4o` / `default`; reasoning efforts
`low` / `medium` / `high`; `max_iterations` min 1, max 100, default 25; the
catalog carries a content-hash `revision`.

## Files read & written

In-repo (source of truth):

| Path | Use |
|---|---|
| `specs/<id>/spec.md`, `specs/<id>/plan.md`, `specs/<id>/tasks.md` | Read by `load_feature` (each with a `sha256:<hex>` content hash); written via hash-checked single-line/section/patch edits. |
| `specs/<id>/**` (checklists, research, data-model, contracts, quickstart, …) | Discovered + validated by `parser/discovery.rs`; authored via the artifact endpoints. |
| `.specify/feature.json` | Read-only. Only the `feature_directory` key (e.g. `"specs/012-spec-studio-visual-ide"`) is required; unknown keys are ignored so future spec-kit versions don't break the UI. |
| `.specify/scripts/bash/*.sh` | Executed as subprocesses (clarify/analyze/implement, `create-new-feature.sh` is the documented writer of `feature.json`). |

Under `~/.joey/speckit-ui/` (override via `JOEY_HOME`):

| Path | Use |
|---|---|
| `preferences.json` | Per-feature `WorkspacePreference` map (FR-026); load-merge-write-back. |
| `history/<feature-id>.jsonl` | Append-only attempt history; one self-contained record per line, mandatory `schema_version: 1`. |
| `ui-state/<repo-hash>-<branch>.json` | Per-repo+branch UI state (`UiState`, `schema_version: 1`); rewritten atomically (temp + rename); never inside `specs/`. |

All multi-line writes are atomic (write-temp + rename), and every
content-bearing write is guarded by `conflict.rs` optimistic concurrency:
the client supplies the `sha256:<hex>` `based_on_hash` it read; a mismatch
returns the current hash and leaves the file untouched.

## CST & meaning layer

- The CST (`cst/`) preserves every byte of the source file — whitespace,
  comments, unknown extensions become `Raw` nodes. The round-trip invariant
  `parse(p, b)?.materialize() == b` holds for all inputs and is enforced by
  `tests/cst_roundtrip.rs`.
- `anchors.rs` gives each node UTF-8 `byte_start`/`byte_end`,
  `expected_bytes`, a SHA-256 `revision_hash`, and a structural
  `fingerprint` (e.g. `requirement/FR-016`) used to pair nodes across edits.
- The meaning layer (`meaning/`) derives a `SemanticGraph` purely by
  pattern-matching CST nodes (`mapping.rs` catalog). It is a derived
  projection only — never a source of truth, never persisted — and is
  rebuilt lazily by `cache.rs` on watcher events within a ≤400 ms budget.
- `coverage.rs` detects defects (`OrphanRequirement`, `RogueTask`,
  `Unverified`, `ConstitutionBreach`), each carrying a `Scaffold` and an
  optional generative follow-on.
- Related endpoints: `GET /api/features/:id/cst/:artifact`,
  `GET .../meaning/graph`, `GET .../meaning/tree-diff`,
  `GET .../meaning/board`, `POST .../meaning/board/:task_id/toggle`,
  `GET .../meaning/coverage`, `GET .../meaning/clarify` (+ `POST
  .../meaning/clarify/:marker_id/answer`), and the
  `WS /api/features/:id/meaning/stream` live-update channel.

## Patch engine

Every developer-accepted edit compiles to `PatchOp`s and flows through
`patch/`:

- `guard.rs` verifies each targeted node's `revision_hash` +
  `expected_bytes` before any write; mismatch routes to
  `PatchResult::Conflict` (100% external-change detection).
- `surgical.rs` applies `PatchOp::{Replace, InsertAfter, Delete}` to a temp
  buffer so only the edited node's byte range changes — every byte outside
  it stays identical (`tests/byte_anchor_patch.rs`).
- `transaction.rs` performs temp buffer → CST re-parse → validation →
  atomic file replace, returning a verified inverse `undo: Vec<PatchOp>`;
  validation failure replaces nothing.
- `merge.rs` implements three-way merge at semantic-block level: nodes are
  paired by `fingerprint` across base/current/proposed, non-conflicting
  nodes auto-merge, both-sides-changed nodes surface `MergeConflict` with
  `TakeBase | TakeCurrent | TakeProposed | Edit(bytes)` resolution
  (`tests/three_way_merge.rs`).
- `node_lock.rs` locks individual nodes during concurrent developer/agent
  edits — the agent's output for a locked node diverts to the review pane
  instead of clobbering it; unrelated nodes in the same file still stage.
- Simpler single-line and section edits go through `writer.rs`
  (`replace_line_if_unchanged`) and `editor.rs` (whole/section scopes),
  both hash-checked with 409-on-conflict semantics.

## Workflow engine

- `commands.rs` never reimplements SpecKit: it runs
  `.specify/scripts/bash/<name>.sh <args>` when present, else the `specify`
  CLI subcommand (`specify clarify/analyze/implement ...`, `specify init
  --here --integration <agent> --script <type>`). Run-scoped instructions
  travel via the `SPEC_KIT_RUN_INSTRUCTIONS` env var; task execution is
  single-task only (`--task <id>`, never cascades).
- `runner.rs`/`runner_impl.rs` implement the workflow-step runner:
  it prefers the `joey` CLI (`joey /speckit-<step>`) and falls back to
  `.specify/scripts/bash/<step>.sh`, spawned out-of-process with piped
  stdio inside the staging worktree, `SPECIFY_FEATURE=<id>` in the env.
  Output lines are classified into `RunnerEvent`s; interaction payloads
  are forwarded to the subprocess stdin as JSON lines.
- `workflow.rs` builds the step catalog from the installed spec-kit
  lifecycle, derives each step's `StepState` as a pure function of artifact
  state + prerequisites + active runs, and maintains the `DependencyLink`
  graph used for stale propagation and traceability.
- Step config and project-level overrides: `GET
  /api/features/:id/workflow/:step/config`, `PUT`/`DELETE .../override`
  (stored in `AppState`, keyed `feature_id:step_id`, FR-034).
- Run lifecycle: `POST /api/features/:id/workflow/:step/run` prepares a
  `RunConfiguration` (mandatory `ChangeMode`), opens the staging area, and
  streams via `WS /api/attempts/:attempt_id/stream`. Interaction endpoints:
  `POST /api/attempts/:attempt_id/answer|approve|cancel|recover`; change
  review via `GET .../changes` and `POST .../changes/apply`. The FR-015
  conflict guard rejects a new run whose scope paths overlap an in-flight
  attempt's.
- `staging.rs`/`staging_impl.rs` back the two change modes: staged runs in
  a temp worktree at `joey/staging/<feature>/<attempt>`; direct runs in the
  primary worktree.
- `watcher.rs` debounces fs events (~500 ms) per distinct feature
  directory — one process-global debouncer per dir, reused across WebSocket
  connections. The watch socket pushes `file_changed` frames (file + fresh
  content hash) and, via the dependency graph, `stale_propagated` frames
  naming downstream artifacts affected by the change.

## REST API

46 REST routes in `api/rest.rs` plus 5 WebSocket routes in `api/ws.rs`
(51 total), grouped by prefix:

| Prefix | Routes |
|---|---|
| features (core) | `GET /api/features`; `GET /api/project` (bootstrap: repo flags, feature list, active feature from `.specify/feature.json`); `GET /api/features/:id`; `PATCH /api/features/:id/spec`; `PATCH /api/features/:id/tasks/:task_id` |
| clarify / analyze / execute | `POST /api/features/:id/clarify`; `POST .../clarify/:session_id/answer`; `POST .../analyze`; `POST .../tasks/:task_id/execute` |
| init | `POST /api/init` |
| artifacts | `GET /api/features/:id/artifacts`; `GET`+`PATCH /api/features/:id/artifacts/*path` |
| workflow | `GET /api/features/:id/workflow`; `GET /api/options`; `GET .../workflow/:step/config`; `PUT`+`DELETE .../workflow/:step/override`; `POST .../workflow/:step/run` |
| attempts | `POST /api/attempts/:attempt_id/answer | approve | cancel | recover` |
| changes | `GET /api/attempts/:attempt_id/changes`; `POST .../changes/apply` |
| history | `GET /api/features/:id/history` |
| preferences | `GET`+`PUT /api/features/:id/preferences` |
| health | `GET /api/health` |
| setup | `GET /api/setup/scan-repo`; `POST /api/setup/preview`; `POST /api/setup/commit` |
| atlas / stage-bar | `GET /api/features/:id/atlas`; `GET .../stage-bar` |
| recovery | `GET /api/features/:id/recovery-states`; `GET .../recovery-surface` |
| cst / meaning | `GET .../cst/:artifact`; `GET .../meaning/graph | tree-diff | board | coverage | clarify`; `POST .../meaning/board/:task_id/toggle`; `POST .../meaning/clarify/:marker_id/answer` |
| patch / hunks | `POST /api/features/:id/patch`; `POST .../hunks/:hunk_id/accept` |
| branch-drift | `GET /api/features/:id/branch-drift` |
| defects | `GET /api/features/:id/defects`; `POST .../defects/:defect_id/fix` |
| WebSocket | `WS /api/features/:id/watch`; `WS .../session/:session_id`; `WS /api/runs/:run_id`; `WS /api/attempts/:attempt_id/stream`; `WS .../meaning/stream` |

Error bodies are uniformly `{ "error": <code>, "message": ... }`; conflict
responses carry the current hash so the client can reload and reapply.
Contract coverage lives in the `contract_*` test files; the launcher side
(`joey speckit`) is documented in [joey-cli.md](joey-cli.md).

## Recovery & history

- `recovery.rs` decides restart recovery per attempt: statuses needing
  recovery are `running`, `awaiting_input`, `awaiting_approval`,
  `recoverable_failure`, `recovery_needed`. An attempt with a checkpoint
  whose `tree_ish` starts with `sha1:` resumes (status back to `running`,
  no replay of unconfirmed actions); otherwise it is marked
  `recovery_failed` with its effects preserved. `main.rs` runs
  `scan_all_for_recovery` over every feature's JSONL at startup.
- `history.rs` round-trips `WorkflowAttempt` records: O(1) appends,
  newest-first reads, tolerant skip of partial last lines (crash safety),
  a hard `schema_version: 1` gate (unknown versions are skipped with a
  warning), cursor-paginated reads, atomic `update_in_place` rewrites when
  a checkpoint advances, and a 90-day expiry sweep (`expires_at`) run at
  startup and hourly. Feature-012 added overlay record kinds on the same
  JSONL: legacy records (no `record_type`) decode as attempts; tagged
  records decode as `accepted_clarify` or `comment_thread`; unknown kinds
  are skipped forward-compatibly.

## Testing

18 integration test files under `crates/joey-speckit-ui/tests/` (plus
`common/mod.rs` fixtures/helpers):

- `byte_anchor_patch.rs` — per-`PatchOp`, per-node-kind byte-identity outside the edited range.
- `conflict_detection.rs` — stale-hash writes rejected, file left unmodified.
- `contract_analyze.rs` — `POST /analyze` contract.
- `contract_api_regression.rs` — every specs/001 REST/WS endpoint preserved after feature-010 routes.
- `contract_artifacts.rs` — artifact discovery/read/patch endpoints.
- `contract_clarify.rs` — `POST /clarify` (+ `/answer`) contract.
- `contract_execute.rs` — single-task `POST .../execute` returns a `run_id`.
- `contract_patch_spec.rs` — `PATCH .../spec` contract.
- `contract_patch_task.rs` — `PATCH .../tasks/:task_id` contract.
- `cst_roundtrip.rs` — `parse(p, b)?.materialize() == b` across clean + malformed fixtures.
- `history_jsonl_roundtrip.rs` — JSONL round-trip, `schema_version`, partial-line tolerance.
- `meaning_graph.rs` — 100% defect recall on the seeded fixture; scaffold round-trip.
- `parser_roundtrip.rs` — spec/plan/tasks parse is idempotent/content-equivalent.
- `project_autoload.rs` — `.specify/feature.json` active-feature read + `GET /api/project` bootstrap.
- `scale_validation.rs` — ≥500 tasks / ≥100 attempts / ≥1000 changed files stay interactive.
- `three_way_merge.rs` — fingerprint-labelled `MergeConflict`s; silent auto-merge.
- `ui_state_roundtrip.rs` — UI-state JSON round-trip + write-tree isolation.
- `ws_run_execute.rs` — POST execute → `WS /api/runs/{run_id}` → `tasks.md` write-back over a real socket.

Inline `#[cfg(test)]` unit tests also live in `conflict.rs`, `history.rs`,
`recovery.rs`, `ui_state.rs`, `watcher.rs`, `commands.rs`, and the
parser/cst/meaning/patch modules. Run scoped with
`cargo test -p joey-speckit-ui`.

## See also

- [../speckit-ui.md](../speckit-ui.md) — user-facing UI guide
- [../speckit-workflow.md](../speckit-workflow.md) — the spec-kit lifecycle in `joey`
- [../speckit-ui-launcher.md](../speckit-ui-launcher.md) — the `joey speckit` launcher
- [joey-cli.md](joey-cli.md) — the `joey` binary and its command tree
- [joey-core.md](joey-core.md) — `~/.joey` home, config, and `JOEY_HOME`
- [README.md](README.md) — docs/features index
