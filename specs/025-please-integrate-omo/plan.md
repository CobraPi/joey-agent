# Implementation Plan: OMO-HyperCode Orchestration Integration

**Branch**: `025-please-integrate-omo` | **Date**: 2026-09-03 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/025-please-integrate-omo/spec.md`

## Summary

Integrate OMO into HyperCode orchestration: a newly-authored delegation-first orchestrator persona (atlas conductor identity, orchestration hard rules embedded) replaces the fixed orchestrator prompt whenever orchestration is enabled and the OMO registry is populated; every registered OMO agent becomes a named delegation target; role model defaults derive from OMO agent chains; the existing OMO agent switch swaps the orchestrator persona mid-session; GPT-5.6-optimized prompt variants ship for the persona and all four switchable primaries; and orchestrator guidance aligns delegation with the spec-kit lifecycle.

## Technical Context

**Language/Version**: Rust stable (rust-toolchain.toml), edition 2021.

**Primary Dependencies**: Existing workspace crates only — joey-omo, joey-cli, joey-orchestration, joey-agent-core. No new external dependencies.

**Storage**: Existing `~/.joey` YAML config via existing keys (`hypercode.enabled`, `hypercode.<role>.<provider>.*`). No on-disk format changes; no new configuration keys.

**Testing**: `cargo test` — per-crate (`-p joey-omo`, `-p joey-orchestration`, `-p joey-cli`) plus full-workspace gate.

**Target Platform**: macOS/Linux CLI and TUI (existing targets, unchanged).

**Project Type**: CLI agent workspace (library crates + binary).

**Performance Goals**: Persona prompts remain compile-time string constants; selection adds only a model-family prefix match per session/switch. The build-once-per-session system prompt discipline is preserved; persona changes occur only on explicit user switches through the existing rebuild path.

**Constraints**: Provider prompt-prefix cache warmth (no per-turn prompt rebuilds); additive-only configuration and tool-surface changes; SQLite schema, jobs.json, SKILL.md and all other pinned on-disk formats untouched; guidance strings that are ported verbatim from upstream are not reworded.

**Scale/Scope**: 11 OMO agents exposed as delegation targets; 3 role-to-chain mappings; ~6 new prompt variants (delegation-first persona default/gpt/gpt_5_6, plus gpt_5_6 for sisyphus, atlas, prometheus); wiring in 3 crates.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Rationale |
|---|---|---|
| 0. Cross-Platform | PASS | No platform-specific code; prompt and config logic only. |
| I. Workspace-First Rust | PASS | All changes in existing workspace crates; no new crates, no external deps. |
| II. CLI/TUI Parity | PASS | Persona switch rides the shared engine path (`engine_switch_agent`); both surfaces benefit identically. |
| III. Filesystem Is the Source of Truth | PASS | Spec-kit artifacts are read as state; no parallel UI state introduced. |
| IV. Test-First for New Crates | N/A | No new crates; tests added alongside changes in existing crates. |
| V. Incremental Reviewable Delivery | PASS | Tasks ordered in independently verifiable waves (prompts → wiring → roster → role defaults → guidance). |
| VI. Modularity and Decoupling | PASS | Persona library stays in joey-omo; joey-cli consumes via existing prompt-dispatch surface; joey-orchestration untouched by persona logic. |
| VII. Backward Compatibility (NON-NEGOTIABLE) | PASS | No new config keys (defaults derived at resolution time); delegation arg surface unchanged (enum widened additively, schema not closed); with orchestration off or registry empty, behavior is byte-identical. |
| VIII. Performance Discipline | PASS | Compile-time prompt constants; no runtime prompt construction beyond constant selection. |

**Post-design re-check (after Phase 1)**: Confirmed — data model and contracts introduce no new dependencies, no new configuration keys, no on-disk format changes, and no breaking surface changes; all contract deltas are additive. Gate remains PASS.

## Project Structure

### Documentation (this feature)

```text
specs/025-please-integrate-omo/
├── plan.md              # This file (/speckit-plan command output)
├── research.md          # Phase 0 output (/speckit-plan command)
├── data-model.md        # Phase 1 output (/speckit-plan command)
├── quickstart.md        # Phase 1 output (/speckit-plan command)
├── contracts/           # Phase 1 output (/speckit-plan command)
│   ├── orchestrator-persona.md
│   ├── delegation-roster.md
│   └── role-defaults.md
└── tasks.md             # Phase 2 output (/speckit-tasks command - NOT created by /speckit-plan)
```

### Source Code (repository root)

```text
crates/
├── joey-omo/src/agents/prompts/
│   ├── conductor.rs           # NEW: delegation-first persona (default/gpt/gpt_5_6 variants, for_model dispatch)
│   ├── sisyphus.rs            # ADD gpt_5_6() variant + Gpt-arm version sub-check
│   ├── atlas.rs               # ADD gpt_5_6() variant + Gpt-arm version sub-check
│   ├── prometheus.rs           # ADD Gpt arm with gpt_5_6() (+ generic gpt fallback)
│   └── mod.rs                 # EXPORT conductor dispatch (not registered in agent registry)
├── joey-cli/src/
│   ├── hypercode.rs           # Persona-aware orchestrator overlay; role-default derivation from OMO chains
│   └── engine.rs              # reapply_orchestrator_overlay carries agent name + resolved model
└── joey-orchestration/src/
    └── delegation_tool.rs     # call_omo_agent schema enum widened to full roster; unknown-type error lists valid names; role-default derivation mirror
```

**Structure Decision**: Single-workspace layout (default option). All edits land in existing crates along the dependency DAG: persona library in joey-omo (lowest), consumption in joey-cli, delegation surface in joey-orchestration; no cross-crate cycles.

## Complexity Tracking

No Constitution Check violations; nothing to justify.
