# Implementation Plan: Oh My OpenAgent Orchestration

**Branch**: `003-omo-orchestration` | **Date**: 2026-07-23 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/003-omo-orchestration/spec.md`

## Summary

Port oh-my-openagent's full multi-agent orchestration system into joey-agent as
a 1-to-1 re-implementation: all 11 built-in agents (Sisyphus, Hephaestus,
Prometheus, Atlas, Oracle, Librarian, Explore, Multimodal-Looker, Metis, Momus,
Sisyphus-Junior), 11 delegate-task categories, model fallback chains with
family-level fuzzy matching, IntentGate/ultrawork working modes, the three-layer
plan→execute→worker pipeline, Tab-based agent switching, and an elegant
bottom-right agent activity panel. The existing joey-agent default agent is
prepended to the Tab cycle (5 entries) for backward compatibility. The design
adds one new crate (`joey-omo`) on top of the existing `joey-orchestration`
delegation engine, with additive extensions to `joey-tui`, `joey-cli`, and
`joey-agent-core`. No new external dependencies — everything composes from
existing workspace crates.

## Technical Context

**Language/Version**: Rust 2021 edition, stable 1.80+ (workspace-standard)

**Primary Dependencies**: tokio (async runtime, already workspace dep),
serde/serde_json (serialization), ratatui (TUI, already workspace dep),
crossterm (input, already workspace dep), the existing joey-core /
joey-providers / joey-tools / joey-agent-core / joey-orchestration /
joey-tui / joey-cli crates. No new external dependencies — the entire feature
is composed from existing workspace crates.

**Storage**: File-based under `.omo/` (plans, notepads, boulder state, goal
state). The `.omo/boulder.json` file tracks active work. `.omo/notepads/
{plan}/` stores wisdom accumulation markdown. `.omo/plans/{name}.md` stores
Prometheus-generated plans. `.omo/goals.json` stores per-session goal state.
All filesystem operations use std::fs (synchronous, infrequent writes) — no
database needed.

**Testing**: `cargo test -p <crate>` (workspace-standard). Unit tests alongside
each module. Contract tests for: agent registry enumeration, model fallback
chain resolution with fuzzy matching, category delegation routing, Tab picker
state machine, boulder state persistence round-trip. The existing `Transport`
trait in agent.rs enables mocked-provider tests.

**Target Platform**: macOS / Linux native binary (same as existing workspace)

**Project Type**: library (new crate `joey-omo`) + additive integration into
`joey-tui` (Tab picker + activity panel), `joey-cli` (slash commands + CLI
parity), and `joey-agent-core` (event variants)

**Performance Goals**:
- Agent registry build at startup: <50ms (11 agents × model resolution)
- Tab picker render: <16ms (single frame, no perceived lag)
- Activity panel update per AgentEvent: <1ms (event application to App state)
- Model fallback chain resolution per agent: <5ms (set lookups, no I/O)
- IntentGate keyword detection: <100µs (regex on short strings)
- Boulder state read/write: <5ms (small JSON file)
- SC-003: Activity panel updates at least once per second, zero perceptible lag
- SC-009: Parallel delegation wall-clock ≈ slowest single subagent (not sum)

**Constraints**:
- Constitution Principle VIII: no new runtime dependencies; zero-copy where
  feasible; minimal allocations on hot paths (event processing, panel render)
- Constitution Principle VII: strictly additive — existing turn loop, CLI
  flags, TUI keybindings (except enhanced Tab), config keys unchanged
- Constitution Principle VI: `joey-omo` exposes narrow public API via traits;
  depends only on sibling crates' public surfaces; acyclic dependency graph
- `cargo build --workspace` and `cargo test --workspace` green on every increment
- Tab key currently toggles focus (Input ↔ Transcript) — this must be repurposed
  for agent switching while preserving a way to focus the transcript (see
  Complexity Tracking)

**Scale/Scope**: 1 new crate (`joey-omo`), 3 crates modified additively
(joey-tui, joey-cli, joey-agent-core), ~12 new modules, ~25-35 new tests.
Estimated 2,500-4,000 lines of Rust.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Notes |
|-----------|--------|-------|
| I. Workspace-First Rust | **PASS** | All new code lives in a new `crates/joey-omo` crate. Independently buildable (`cargo build -p joey-omo`) and testable (`cargo test -p joey-omo`). No root-level code. |
| II. CLI/TUI Parity | **PASS** | Tab-based agent switching works in both TUI (overlay picker) and CLI (numbered menu). Agent activity panel (TUI) has CLI parity (inline event summaries). Slash commands (`/start-work`, `/goal`) work in both surfaces. |
| III. Filesystem Source of Truth | **PASS** | `.omo/` artifacts (plans, notepads, boulder state, goals) are the source of truth, written/read synchronously. No UI-only state that can drift from disk. |
| IV. Test-First for New Crates | **PASS** | Each module in `joey-omo` ships with unit tests alongside implementation. Contract tests prove: 11-agent enumeration, model fallback resolution, category routing, boulder state round-trip. |
| V. Incremental, Reviewable Delivery | **PASS** | Four-increment delivery (see Project Structure): Inc 1 = agent registry + model chains + Tab switching; Inc 2 = activity panel + CLI parity; Inc 3 = orchestration pipeline (boulder state, notepads, slash commands); Inc 4 = ultrawork/IntentGate + team mode. Each builds and tests green independently. |
| VI. Modularity and Decoupling | **PASS** | `joey-omo` exposes a narrow public API: `AgentRegistry`, `OmoAgent`, `CategoryConfig`, `ModelRequirement`, `BoulderState`, `IntentGate`. It depends on `joey-agent-core` (AgentConfig, AgentEvent), `joey-orchestration` (SubagentManager, DelegationRequest), `joey-providers` (ProviderClient), and `joey-core` (Config) via public traits. No sibling-internal access. Acyclic: joey-omo → joey-orchestration → joey-agent-core → joey-core/joey-providers. Nothing depends back on joey-omo except the integration glue in joey-tui/joey-cli. |
| VII. Backward Compatibility (NON-NEGOTIABLE) | **PASS** | Strictly additive: new AgentEvent variants (enum extension is non-breaking), new TUI panel (additional region), new slash commands (new entries in registry). The Tab key behavior changes — this is the one intentional modification, justified in Complexity Tracking. The existing default agent remains the 0th Tab entry, so users who never press Tab see zero behavioral change. |
| VIII. Performance Discipline | **PASS** | No new dependencies. Agent registry build is O(11) with set lookups for model availability. Model fallback chain resolution uses pre-built provider/model availability sets (HashSet lookups, O(1) per entry). Event application to TUI state is match-arm dispatch (no allocation beyond the event). Activity panel renders from borrowed App state each frame. Boulder state I/O is infrequent (small JSON, std::fs). tokio::task::JoinSet for parallel delegation (already used by joey-orchestration). Performance budget recorded below. |

**Gate Result**: ALL CLEAR. One justified deviation: Tab key repurposing (see
Complexity Tracking).

## Project Structure

### Documentation (this feature)

```text
specs/003-omo-orchestration/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   ├── agent-registry.md
│   ├── model-fallback.md
│   ├── category-delegation.md
│   ├── tab-picker.md
│   ├── activity-panel.md
│   ├── slash-commands.md
│   └── orchestration-pipeline.md
└── tasks.md             # Phase 2 output (NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-omo/                     # NEW CRATE — the OMO orchestration system
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs                # Public API: AgentRegistry, OmoAgent, etc.
│       ├── agents/               # 11 agent definitions + prompt builders
│       │   ├── mod.rs
│       │   ├── registry.rs       # AgentRegistry: build, register, resolve models
│       │   ├── sisyphus.rs       # Sisyphus agent + prompt variants
│       │   ├── hephaestus.rs     # Hephaestus agent + GPT-family variants
│       │   ├── prometheus.rs     # Prometheus agent (read-only planner)
│       │   ├── atlas.rs          # Atlas agent (conductor) + variants
│       │   ├── oracle.rs         # Oracle subagent
│       │   ├── librarian.rs      # Librarian subagent
│       │   ├── explore.rs        # Explore subagent
│       │   ├── multimodal.rs     # Multimodal-Looker subagent
│       │   ├── metis.rs          # Metis subagent (gap analyzer)
│       │   ├── momus.rs          # Momus subagent (reviewer)
│       │   ├── junior.rs         # Sisyphus-Junior subagent + 9 variants
│       │   └── prompts/          # Prompt text (ported from OMO markdown)
│       │       ├── mod.rs
│       │       ├── sisyphus/     # default, glm, gpt, kimi-k3, gemini
│       │       ├── atlas/        # default, gpt, gemini, kimi, kimi-k3, etc.
│       │       ├── hephaestus/   # gpt, gpt-5-4, gpt-5-5, gpt-5-6
│       │       ├── junior/       # default, kimi, gpt, gemini, glm (9 variants)
│       │       ├── prometheus/   # default
│       │       └── ultrawork/    # default, gpt, gemini, glm, planner
│       ├── models.rs             # ModelRequirement, FallbackEntry, AGENT/CATEGORY chains
│       ├── categories.rs         # 11 categories + CategoryConfig + fuzzy resolution
│       ├── intent_gate.rs        # Keyword detection: ultrawork, hyperplan, team
│       ├── mode.rs               # AgentMode (Primary/Subagent), DisplayAgent
│       ├── boulder.rs            # BoulderState: .omo/boulder.json persistence
│       ├── notepad.rs            # NotepadStore: .omo/notepads/{plan}/ wisdom files
│       ├── goal.rs               # GoalState: .omo/goals.json persistence
│       ├── plan_parser.rs        # Parse .omo/plans/{name}.md task lists
│       └── tests.rs              # Integration + contract tests
│
├── joey-tui/src/
│   ├── app.rs                    # MODIFIED — Tab → agent picker, draw layout with new panel
│   ├── state.rs                  # MODIFIED — agent roster state, active agent mode
│   ├── widgets.rs                # MODIFIED — draw_omo_panel (bottom-right split panel)
│   └── omo_panel.rs              # NEW — agent activity panel widget (split: pinned + roster/activity)
│
├── joey-cli/src/
│   ├── slash.rs                  # MODIFIED — /start-work, @plan registered
│   ├── repl.rs                   # MODIFIED — agent mode switching (Tab parity), /goal handler
│   └── omo_render.rs             # NEW — CLI inline agent activity summaries
│
├── joey-agent-core/src/
│   └── events.rs                 # MODIFIED — add OMO event variants (additive)
│
└── (existing crates unchanged)
```

**Structure Decision**: A new dedicated crate `crates/joey-omo` holds the entire
OMO orchestration system (agent registry, model requirements, categories,
prompts, IntentGate, boulder/notepad/goal state). It builds on top of the
existing `joey-orchestration` crate (SubagentManager, DelegationRequest) —
extending, not replacing it. The TUI and CLI receive additive integration
modules. The only modifications to existing crates are: (1) new AgentEvent
variants in `joey-agent-core/src/events.rs` (additive enum extension), (2) Tab
key handling + new panel in `joey-tui`, (3) slash command entries + agent
switching in `joey-cli`. This follows Constitution Principles I (workspace-
first), VI (narrow interfaces, acyclic coupling), and VII (strictly additive).

**Dependency graph (acyclic)**:
```
joey-cli ──→ joey-tui ──→ joey-omo ──→ joey-orchestration ──→ joey-agent-core ──→ joey-core
   │              │           │                                         ──→ joey-providers
   │              │           ──→ joey-agent-core
   │              ──→ joey-agent-core
   ──→ joey-agent-core
```

### Incremental Delivery Plan

| Increment | Scope | Deliverable |
|-----------|-------|-------------|
| **Inc 1** | Agent registry + model fallback chains + categories + Tab switching (TUI + CLI) | User can press Tab to cycle Default → Sisyphus → Hephaestus → Prometheus → Atlas; each agent's prompt/model is activated |
| **Inc 2** | Agent activity panel (bottom-right TUI) + CLI parity rendering | Live subagent activity visible in elegant split panel; CLI prints inline summaries |
| **Inc 3** | Orchestration pipeline: boulder state, notepad store, plan parser, slash commands (`/start-work`, `@plan`, `/goal`) | Full plan→execute workflow; Atlas reads plans, delegates, accumulates wisdom |
| **Inc 4** | IntentGate + ultrawork + hyperplan keyword detection + team mode (optional) | `ulw` activates ultrawork; team mode available but OFF by default |

### Performance Budget

| Path | Budget | Measurement Method |
|------|--------|--------------------|
| Agent registry build (11 agents, model resolution) | <50ms | `Instant::now()` in registry build test |
| Model fallback chain resolution (per agent) | <5ms | HashSet availability lookups, timed in tests |
| Tab picker render (overlay) | <16ms | Single frame render, measured via debug timing |
| Activity panel update per AgentEvent | <1ms | Match-arm dispatch to App state, timed |
| IntentGate keyword detection | <100µs | Regex scan on short strings |
| Boulder state read (`.omo/boulder.json`) | <5ms | Small JSON parse, std::fs |
| Boulder state write | <5ms | Small JSON serialize + fs::write |
| Parallel delegation dispatch (5 subagents) | ≈ slowest single (not sum) | JoinSet, measured in quickstart |

## Complexity Tracking

> **Tab key repurposing** — the single intentional deviation from strict
> backward compatibility on an existing TUI keybinding.

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| Tab key behavior changes (was: toggle focus Input ↔ Transcript; now: cycle agent mode) | The user explicitly requested Tab-based agent switching ("I can switch between the different agent mode by pressing tab"). This is the core UX of the feature. | Alternative: use a different key (e.g., F2, Ctrl+T) for agent switching, preserve Tab for focus. **Rejected because**: the user explicitly said Tab. Mitigation: transcript focus moves to a new key (Up arrow from input, or Ctrl+T) and the existing scroll keys (j/k, PageUp/Down) work regardless of focus. The help overlay is updated to document the change. |
