# Implementation Plan: Spec-Kit Development IDE

**Branch**: `010-speckit-development-ide` | **Date**: 2026-08-03 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `/specs/010-speckit-development-ide/spec.md`

## Summary

Promote the Spec-Kit visual UI built in `specs/001-speckit-visual-ui`
(`joey-speckit-ui` Rust backend + `web/speckit-ui` Vite/TypeScript frontend)
from a *viewer* into a full authoring and execution surface. Users can edit
every artifact (spec, plan, tasks, checklists, research, data-model,
contracts, quickstart), inspect/modify/run every Spec-Kit workflow step
through the **native Joey Agent** (driven out-of-process via the `joey` CLI /
`/speckit-*` skill wrappers — never an in-process reimplementation), and
review agent-authored changes hunk-by-hunk before applying them.

The defining technical decisions, locked by the clarifications in
`spec.md` and `research.md`:

1. **Out-of-process agent execution (FR-011, Constitution VI).** The backend
   spawns `joey` (or the relevant `/speckit-*` skill wrapper) as a subprocess
   in the feature's repository context and streams progress/questions/output
   over the existing WebSocket channel — the same model `specs/001` uses for
   `/speckit-implement`. `joey-speckit-ui` never links against
   `joey-agent-core` internals.
2. **Git-backed staged changes (FR-016, Constitution VIII).** Candidate
   changes are held in the repository's Git index or a dedicated temporary
   worktree/branch, so accept/reject/recover map to native Git primitives
   (`git checkout`, `git restore`, hunk-level `git apply --reject`) and
   survive backend restarts. No out-of-tree scratch store.
3. **Append-only JSONL history (FR-018, Constitution VII/VIII).** Run
   attempts are one self-contained record per line at
   `~/.joey/speckit-ui/history/<feature-id>.jsonl`; 90-day expiry is a
   file-mtime sweep. No new database dependency or schema version.

## Technical Context

**Language/Version**: Rust (2021 edition, stable toolchain per
`rust-toolchain.toml`) for the `joey-speckit-ui` backend; TypeScript 5.5 +
Vite 5 for the `web/speckit-ui` frontend (both already present from
`specs/001`).

**Primary Dependencies**:
- *Backend (existing, reused)*: `axum` 0.7 (`ws` feature — HTTP + WebSocket),
  `tokio` (async runtime + subprocess), `serde`/`serde_json`, `pulldown-cmark`
  0.12 (Markdown outline/preview), `notify` 6 / `notify-debouncer-mini` 0.4
  (feature-dir watcher), `sha2`+`hex` (content-hash conflict model),
  `walkdir`, `chrono`, `uuid`, `tracing`.
- *Backend (new — see `research.md` for the full tradeoff analysis)*:
  `gix` 0.6x (pure-Rust libgit2 alternative, already-workspace-friendly,
  workspace-bump-free) **or** shelling out to the system `git` CLI for
  staging/recovery primitives. Decision recorded in `research.md` §3;
  `gix` is the recommended choice to keep the backend single-binary and
  avoid a runtime `git` dependency, with the CLI as a fallback path.
- *Frontend (existing)*: Vite 5, TypeScript 5.5, Playwright 1.47 (e2e).
  New frontend deps are limited to a diff/merge view component and a
  resizable-pane layout; the choice is justified in `research.md` §4.

**Storage**:
- **Canonical artifacts**: Markdown/JSON files under `.specify/` and
  `specs/<feature>/` — the source of truth (Constitution III).
- **Staged candidate changes**: the repository Git index, or a dedicated
  temporary worktree on a `joey/staging/<feature>/<attempt>` branch
  (FR-016). Lives in the user's repo, never in a separate store.
- **Run history**: append-only JSONL at
  `~/.joey/speckit-ui/history/<feature-id>.jsonl`, one self-contained
  attempt record per line (FR-018). 90-day expiry via file-mtime sweep.
- **Workspace preferences** (selected feature, open artifacts, pane layout,
  filters): a small JSON file under `~/.joey/speckit-ui/preferences.json`,
  explicitly excluding unsaved artifact content (FR-026).
- **Safe recovery checkpoints**: co-located with the JSONL attempt record
  (a `checkpoint` field referencing a Git tree-ish and the last confirmed
  interaction id) so a restart resumes from the latest safe point (FR-033).

**Testing**: `cargo test -p joey-speckit-ui` (Rust unit + integration +
contract/round-trip, mirroring the existing `tests/contract_*.rs` and
`tests/parser_roundtrip.rs` pattern); `npm run test:e2e` (Playwright) for
the frontend; `cargo build --workspace && cargo test --workspace` as the
workspace-wide acceptance bar. New on-disk formats (JSONL record schema,
checkpoint format) carry round-trip + migration tests (Constitution VII).

**Target Platform**: Local desktop browser (Chrome/Edge/Firefox/Safari
latest) consuming a backend bound to `127.0.0.1` on macOS, Linux, and
Windows. A mobile-optimized authoring experience is explicitly out of scope
(spec Assumptions).

**Project Type**: Desktop-class web application = local Rust backend
(`crates/joey-speckit-ui`) + browser frontend (`web/speckit-ui`). This is
an **extension of an existing project**, not a new one — see Project
Structure.

**Performance Goals** (derive from SC-004, SC-007, SC-010):
- Workflow-step readiness derived and stale-propagated in **< 3 s** after
  an upstream artifact save (SC-007).
- Agent interaction events (progress/question/approval/cancel) surfaced in
  the workspace in **< 2 s** under normal local conditions (SC-004).
- A feature with 500 tasks + 100 attempts + 1 000 changed files stays
  interactive: open-artifact / filter-tasks / inspect-run **< 2 s** for
  ≥ 95 % of interactions (SC-010). This mandates virtualized lists in the
  frontend and lazy/streamed JSONL reads in the backend (research.md §5).

**Constraints**:
- Out-of-process agent only (FR-011): no `joey-agent-core` link in
  `joey-speckit-ui`'s `Cargo.toml`; communication is the CLI contract +
  WebSocket streaming.
- Git-backed staging (FR-016): no overlay FS, no scratch store outside the
  repo.
- JSONL history (FR-018): no SQLite/DB for history.
- Reject-on-conflict writes (from `specs/001`): every artifact write
  carries `based_on_hash`; 409 on external change (FR-020).
- Single conflicting run per feature when repository effects would overlap
  (FR-015); independent features may run concurrently.
- Existing safety/approval boundaries remain in force; no new permission
  model (FR-029).

**Scale/Scope**: Per-feature ceilings that must remain interactive
(FR-031): ≥ 500 tasks, ≥ 100 workflow attempts, ≥ 1 000 changed files in a
single change set. Backend is single-user/local (no multi-tenant concerns);
concurrent-edit collaboration is out of scope (rely on external-change
detection, FR-020).

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

Governance baseline: `.specify/memory/constitution.md` v1.1.0 (eight
principles). Evaluation against this feature:

| # | Principle | Result | Notes |
|---|-----------|--------|-------|
| I | Workspace-First Rust | **PASS** | All backend code lives in the existing `crates/joey-speckit-ui` crate (already a workspace member at `Cargo.toml:13`). No code is added to the workspace root. New concerns become new modules (`workflow`, `runner`, `staging`, `history`, `editor`, `validation`) behind narrow `lib.rs` re-exports, each independently buildable via `cargo build -p joey-speckit-ui`. |
| II | CLI/TUI Parity | **PASS** | Every workflow step the IDE exposes is already reachable through the `joey` CLI / `/speckit-*` skills (specify/clarify/plan/checklist/tasks/analyze/implement/converge/task-to-issue). The IDE is an *additive* control surface over the same CLI contract (FR-011 explicitly drives the agent out-of-process). No IDE-only capability is introduced that hides a file-backed or CLI-reachable action. |
| III | Filesystem Is the Source of Truth (NON-NEGOTIABLE) | **PASS** | Canonical artifacts stay on disk under `.specify/` / `specs/<feature>/`; every edit writes back synchronously via the conflict-checked writer (`writer.rs`). Run history and workspace preferences are *supporting metadata* under `~/.joey/speckit-ui/`, never a fork of canonical content (spec Assumptions; FR-018). |
| IV | Test-First for New Crates | **PASS** | New parsers/serializers (JSONL record, checkpoint, multi-artifact editor) ship with round-trip + contract tests alongside implementation, mirroring the existing `tests/contract_*.rs` and `tests/parser_roundtrip.rs` pattern. Tasks (Phase 2) will name these explicitly per the constitution's regression-coverage mandate. |
| V | Incremental, Reviewable Delivery | **PASS** | Decomposed into independently shippable increments (see Project Structure / Phasing): (a) multi-artifact editor + validation, (b) workflow-step catalog + readiness, (c) out-of-process runner + interaction stream, (d) Git-backed staging + change review, (e) JSONL history + restart recovery, (f) workspace layout + search/navigation. Each increment must build and pass tests on its own. |
| VI | Modularity and Decoupling | **PASS** | The agent is driven **out-of-process** via the `joey` CLI contract (FR-011) — `joey-speckit-ui` depends only on the CLI/stdin/stdout/exit-code interface, never on `joey-agent-core` internals. New modules expose narrow traits (`WorkflowRunner`, `StagingArea`, `HistoryStore`) so a change in one does not force edits to siblings. No new logic is threaded through shared core paths. |
| VII | Backward Compatibility and Non-Regression (NON-NEGOTIABLE) | **PASS (with mandatory regression coverage)** | REST/WS API additions are strictly additive — existing endpoints from `specs/001` (`GET /api/features`, `GET /api/features/{id}`, `PATCH .../spec`, `PATCH .../tasks/{taskId}`, `POST .../clarify*`, `POST .../analyze`, `POST .../tasks/{taskId}/execute`, `POST /api/init`, `WS .../watch`, `WS .../session/{id}`, `WS /api/runs/{run_id}`) are preserved unchanged. The JSONL attempt-record schema is declared a **versioned on-disk public format**: a `schema_version` field is mandatory and any breaking change requires a MAJOR bump + documented migration + round-trip tests (FR-018). Regression tests asserting prior behavior are mandated for every touched public surface. |
| VIII | Performance Discipline and Lean Code | **PASS** | Three deliberate lean-code choices, each justified in `research.md`: (1) append-only JSONL over SQLite for history (sequential append/read, mtime expiry — no query engine needed); (2) Git primitives over a custom overlay/scratch FS for staging; (3) zero-copy / streamed JSONL reads + virtualized frontend lists to hit the SC-010 scale budget. Any new dependency (`gix` or diff-view lib) is recorded with binary-size/compile-time cost vs. alternatives. Performance-sensitive paths (readiness derivation, history streaming, diff rendering) carry explicit budgets below. |

**Gate result: PASS — no violations.** No Complexity Tracking entries are
required. The feature is strictly additive over `specs/001` and respects
all eight principles, including the two NON-NEGOTIABLE ones (III, VII).

### Post-design re-check (after Phase 1)

Re-evaluated against the generated `research.md`, `data-model.md`, and
`contracts/*`. Result unchanged — **PASS, no new violations**:

- **III** — design confirms canonical artifacts stay on disk; the JSONL
  record (`contracts/history-jsonl.md`) stores a *summary* change set and
  resolves full diffs on demand from Git, never forking artifact content.
- **VI** — `WorkflowRunner` / `StagingArea` / `HistoryStore` are exposed
  as narrow traits (`contracts/workflow-runner.md`, `staging-api.md`); the
  agent is driven solely via the out-of-process CLI contract.
- **VII** — `schema_version: 1` is mandatory on every JSONL record and the
  contract names the MAJOR-bump + migration + round-trip-test rule for any
  breaking change; all REST/WS additions are additive over the `specs/001`
  contract.
- **VIII** — performance budgets recorded above; every new dependency
  (`gix`, `diff`, `split.js`) carries a cost-vs-alternatives table in
  `research.md` (§3, §4).

Proceed to `/speckit-tasks` (Phase 2).

### Performance budgets (Constitution VIII mandate)

| Path | Budget | Rationale / source |
|------|--------|--------------------|
| Readiness derivation after upstream save | < 3 s p99 | SC-007 |
| Agent-event → workspace render | < 2 s p99 | SC-004 |
| Open artifact / filter tasks / inspect run @ scale (500 tasks / 100 attempts / 1 000 files) | < 2 s for ≥ 95 % of interactions | SC-010 / FR-031 |
| JSONL history append | O(1) per attempt (single line) | FR-018 append-only design |
| 90-day history expiry | O(n) file-mtime sweep, no reindex | FR-018 |
| External-change detection before overwrite | 100 % (content-hash compare) | SC-005 / FR-020 |

## Project Structure

### Documentation (this feature)

```text
specs/010-speckit-development-ide/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output — tradeoffs for gix vs git-CLI,
│                        #   diff-view lib, JSONL schema, restart recovery
├── data-model.md        # Phase 1 output — entities + JSONL record schema
├── quickstart.md        # Phase 1 output — end-to-end validation guide
├── contracts/           # Phase 1 output — additive REST/WS + JSONL schema
│   ├── speckit-ui-api.md        # deltas over specs/001 contract
│   ├── workflow-runner.md       # out-of-process runner contract
│   ├── staging-api.md           # Git-backed staging/recovery contract
│   └── history-jsonl.md         # versioned on-disk history format
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT this command)
```

### Source Code (repository root)

This feature extends two existing trees from `specs/001-speckit-visual-ui`.
**No new crate is introduced** (Constitution I — `joey-speckit-ui` already
exists); the work is additive modules + frontend pages. Existing modules
(`parser`, `model`, `conflict`, `writer`, `watcher`, `commands`, `api`) are
preserved and extended, not rewritten.

```text
crates/joey-speckit-ui/
├── Cargo.toml                       # +gix (or git CLI fallback), deps justified in research.md
└── src/
    ├── lib.rs                       # re-exports new modules
    ├── main.rs                      # unchanged bind to 127.0.0.1
    ├── model.rs                     # EXTEND: +WorkflowStep, +RunConfig, +Attempt,
    │                                #         +AgentInteraction, +ChangeSet/Hunk,
    │                                #         +ValidationFinding, +DependencyLink,
    │                                #         +WorkspacePreference  (spec Key Entities)
    ├── parser/                      # EXTEND: tolerant parsers for plan/tasks already exist;
    │   ├── mod.rs                   #         +checklist/research/data-model/contracts/quickstart
    │   ├── spec.rs                  #         discovery; +stale-marker/dependency extraction
    │   ├── plan.rs
    │   └── tasks.rs
    ├── editor.rs                    # NEW: multi-artifact validation + targeted writes
    │                                #      (extends writer.rs beyond single-line replace)
    ├── validation.rs                # NEW: required-structure + unresolved-marker checks,
    │                                #      findings anchored to artifact locations (FR-007)
    ├── workflow.rs                  # NEW: step catalog (constitution/specify/clarify/plan/
    │                                #      checklist/tasks/analyze/implement/converge/
    │                                #      task-to-issue + extensions), availability, readiness
    │                                #      derivation from artifact state (FR-008/009/021/022)
    ├── runner.rs                    # NEW: out-of-process Joey Agent lifecycle — spawn joey CLI
    │                                #      /skill wrapper, stream progress/questions/approvals/
    │                                #      output, cancel, terminal status (FR-011/012/013/014)
    ├── staging.rs                   # NEW: Git-backed staged/direct change mode — index or temp
    │                                #      worktree, hunk/file accept-reject, dependency warnings,
    │                                #      recovery primitives (FR-010/015/016/017/020)
    ├── history.rs                   # NEW: append-only JSONL at
    │                                #      ~/.joey/speckit-ui/history/<feature>.jsonl,
    │                                #      90-day expiry sweep, schema_version gate (FR-018/019/033)
    ├── recovery.rs                  # NEW: safe-checkpoint recording + restart resume (FR-033)
    ├── conflict.rs                  # unchanged (content-hash model reused)
    ├── writer.rs                    # unchanged API; editor.rs composes it
    ├── watcher.rs                   # unchanged (reused for external-change push)
    ├── commands.rs                  # EXTEND: +run_workflow_step wrapper (out-of-process),
    │                                #         +project-override read/write (FR-034)
    └── api/
        ├── mod.rs                   # EXTEND: merge new routes
        ├── rest.rs                  # EXTEND (additive only — existing routes preserved):
        │                            #   +GET .../artifacts, +PATCH .../artifacts/{path},
        │                            #   +GET .../workflow, +PATCH .../workflow/{step}/config,
        │                            #   +POST .../workflow/{step}/run, +POST .../attempts/{id}/
        │                            #      {answer|approve|cancel}, +GET .../attempts/{id}/changes,
        │                            #   +POST .../attempts/{id}/changes/{file}/{hunk}/{accept|reject},
        │                            #   +POST .../attempts/{id}/recover, +GET .../history,
        │                            #   +GET .../preferences, +PUT .../preferences  (Constitution VII)
        └── ws.rs                    # EXTEND: +WS /api/attempts/{id}/stream (run/interaction stream)

web/speckit-ui/
├── package.json                     # +diff-view +resizable-layout deps (research.md §4)
└── src/
    ├── app.ts                       # EXTEND: unified resizable workspace shell (FR-002)
    ├── services/api.ts              # EXTEND: typed client for new endpoints
    ├── views/
    │   ├── explorer.ts              # NEW: feature/artifact navigator by workflow phase (FR-003)
    │   ├── editor.ts                # NEW: source + rendered reading, outline nav (FR-006)
    │   ├── workflow.ts              # NEW: step list, states, run config, override mgmt (FR-008/009/010/034)
    │   ├── run-panel.ts             # NEW: streamed progress/questions/approvals/cancel (FR-012/013/014)
    │   ├── review.ts                # NEW: change review, hunk/file accept-reject, warnings (FR-016/017)
    │   ├── readiness.ts             # NEW: lifecycle/readiness summary, stale propagation (FR-021/022/032)
    │   └── search.ts                # NEW: cross-artifact/task/run search + filter (FR-025)
    ├── components/
    │   ├── diff-view.ts             # NEW: additions/removals, dependency markers
    │   ├── pane-layout.ts           # NEW: resizable/collapsible/reorderable panes (FR-002/026)
    │   └── status-badges.ts         # NEW: ready/blocked/running/attention/succeeded/failed/stale/unavailable
    └── a11y/                        # NEW: keyboard nav + focus management + labels (FR-027/SC-011)

# Tests mirror each new module (Constitution IV):
crates/joey-speckit-ui/tests/
├── contract_*.rs                    # existing (preserved)
├── parser_roundtrip.rs              # existing (extended to new artifact types)
├── editor_validation.rs             # NEW
├── workflow_readiness.rs            # NEW
├── runner_stream.rs                 # NEW (subprocess hermetic test harness)
├── staging_git.rs                   # NEW (temp bare repo fixtures)
├── history_jsonl_roundtrip.rs       # NEW (schema_version + migration)
└── recovery_resume.rs               # NEW
web/speckit-ui/tests/
└── *.spec.ts                        # Playwright: authoring/run/review/recovery journeys (SC-001..016)
```

**Structure Decision**: Option 2 (web application) — `backend` =
`crates/joey-speckit-ui` (Rust, existing), `frontend` = `web/speckit-ui`
(Vite/TS, existing). Both trees pre-exist from `specs/001`; this feature is
strictly additive modules + views. The backend stays a single workspace
crate (Constitution I); the frontend stays a single Vite app. No new crate,
no new top-level project, no new runtime stack (Constitution "Additional
Constraints": the rendering approach is unchanged — local browser frontend —
so no new-stack justification is needed).

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

*No entries.* The Constitution Check gate passed all eight principles with
no violations; no deviations require justification.
