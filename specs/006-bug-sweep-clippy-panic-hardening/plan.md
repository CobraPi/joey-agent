# Implementation Plan: Code-Hygiene Bug Sweep & Panic Hardening

**Branch**: `006-bug-sweep-clippy-panic-hardening` | **Date**: 2026-07-28 | **Spec**: [spec.md](./spec.md)

**Input**: Feature specification from `specs/006-bug-sweep-clippy-panic-hardening/spec.md`

## Summary

A three-track code-hygiene feature targeting the audit-verified defect
inventory from `/speckit-specify`:

1. **P1 — Correctness fix.** The Kimi-family prompt selector at
   `joey-omo/src/agents/prompts/junior.rs:456-464` returns the wrong prompt
   for `k2.6`/`k2-6` model ids (it returns `kimi_k2_7()` — the k2.7 prompt —
   in both the k2.7 and k2.6 branches). The fix requires authoring the
   missing `kimi_k2_6()` prompt function (it does not exist yet) and wiring
   the k2.6 branch to call it. The k2.6 prompt is calibrated for Kimi K2.6,
   the predecessor generation to K2.7.

2. **P2 — Clippy-clean baseline.** Resolve all clippy warnings across the
   five emitting crates (`joey-tools`, `joey-agent-core`, `joey-orchestration`,
   `joey-omo`, `joey-cli`) so `cargo clippy --workspace -- -D warnings`
   exits zero. Fixes are mechanical (use `div_ceil`, `strip_prefix`,
   `std::mem::take`, `std::io::Error::other`, remove redundant `&`,
   `to_string()`, etc.) and touch only internal implementation — no public
   surface change. Behavior-changing suggestions are recorded as deliberate
   deviations, not applied blindly.

3. **P3 — Panic hardening.** Replace `.unwrap()`/`.expect()` on
   external-input paths with propagated typed errors or logged fallbacks,
   across 7 crates in priority order (`joey-mcp` → `joey-gateway` →
   `joey-cron` → `joey-core` → `joey-providers` → `joey-tools` →
   `joey-agent-core` external-input paths). Each hardened site ships a
   dedicated malformed-input regression test (FR-006). Recovered fallbacks
   emit a canonical structured `tracing::warn!` event (FR-009). A committed
   audit script (`scripts/audit-external-input-unwraps.sh`) makes the
   success criterion machine-verifiable (FR-010).

**Technical approach**: idiomatic std/library methods already on the stable
toolchain (no new dependencies — Constitution VIII). Existing typed error
enums (`ProviderError`, MCP `RequestError`, `anyhow::Result` in joey-core,
`thiserror` where crate-local enums already exist) are the error-propagation
vehicles; no new error type is introduced. The redaction layer
(`joey_core::redact::redact_sensitive_text`) is the canonical sanitization
path for all user-facing error strings (FR-008).

## Technical Context

**Language/Version**: Rust 2021 edition, stable channel pinned by
`rust-toolchain.toml` (currently reporting as rust-1.96.0 lint set). All
fixes use std/library methods available on that toolchain (`div_ceil`,
`str::strip_prefix`, `std::io::Error::other`, `std::mem::take`,
`Iterator::last` on double-ended iterators, etc.).

**Primary Dependencies**: existing workspace crates only. Error handling via
`thiserror` (already in `joey-providers`, `joey-mcp`, `joey-tools/lsp`,
`joey-speckit-ui`) and `anyhow` (already in `joey-core`, `joey-speckit-ui`).
Logging via `tracing` (already used pervasively in `joey-agent-core`,
`joey-cli`, `joey-core`). No new dependency introduced (Constitution VIII).

**Storage**: SQLite (bundled `rusqlite`, `SCHEMA_VERSION = 22` — frozen,
untouched by this feature), `~/.joey/` config/state files, `jobs.json`
(joey-cron). All on-disk formats are Hermes-compatible and MUST NOT change
(FR-004).

**Testing**: `cargo test --workspace` (workspace ~520+ tests, must stay
green). Per-crate isolation via `cargo test -p <crate>`. New regression
tests added per FR-006 (one malformed-input test per hardened external-input
call site). The P1 fix ships a dedicated test that fails on current `master`
and passes after the fix (SC-003).

**Target Platform**: native Rust binary (Linux/macOS), same as existing.

**Project Type**: CLI/TUI application (Cargo workspace, 12 member crates).

**Performance Goals**:
- **P1/P2**: zero runtime cost (logic fix + mechanical refactors; clippy
  fixes like `div_ceil`/`strip_prefix` are strictly equivalent or faster).
- **P3**: hardening MUST NOT measurably degrade steady-state latency,
  throughput, or memory footprint of existing functionality (Constitution
  VIII). Error-propagation paths replace panics with `Result` returns —
  zero cost on the happy path (no branch added on success). Logged
  fallbacks emit one `tracing::warn!` per recovery (off the happy path
  only). The audit script (FR-010) is a dev-time tool, not linked into
  the binary.

**Constraints**:
- No public surface change (APIs, CLI flags/exit codes, config keys,
  on-disk formats incl. SQLite `SCHEMA_VERSION = 22`, wire payloads, trait
  definitions) — Constitution VII (NON-NEGOTIABLE).
- `cargo build --workspace` and `cargo test --workspace` MUST stay green on
  every increment (Constitution VII).
- Each P3 crate increment MUST independently satisfy
  `cargo build -p <crate> && cargo test -p <crate>` (SC-007).
- Guidance strings ported from upstream MUST NOT be "cleaned up"
  (AGENTS.md upstream-fidelity constraint) — except the new `kimi_k2_6()`
  prompt which is authored fresh for the missing variant.

**Scale/Scope**:
- P1: 1 file (`junior.rs`), ~1 new prompt function + 1-line wiring fix +
  regression tests.
- P2: 5 crates, ~59-77 clippy warnings (count shifts with toolchain; the
  exact current count is measured at implementation time by running
  `cargo clippy --workspace`).
- P3: 7 crates, external-input `.unwrap()`/`.expect()` surface. Raw counts
  in `src/` (non-test): `joey-tools` (244/4), `joey-providers` (93/1),
  `joey-core` (167/18), `joey-mcp` (4/18), `joey-gateway` (35/0),
  `joey-cron` (217/2), `joey-agent-core` (136/17). The subset that is
  genuinely on external-input paths (vs. provably-infallible) is
  determined per-crate during implementation; the audit script (FR-010)
  enumerates the final in-scope set.

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Status | Evidence |
|---|---|---|
| I. Workspace-First Rust | **PASS** | All work is in existing `crates/` members; no new crate, no root-level code. Each increment tested via `cargo test -p <crate>`. |
| II. CLI/TUI Parity | **PASS** | No new UI surface. P1/P2/P3 touch internal implementation only; CLI flags/exit codes unchanged. |
| III. Filesystem Source of Truth | **PASS** | No spec-kit UI work. `.specify/` artifacts are read/written only by the spec-kit commands themselves. |
| IV. Test-First for New Crates | **N/A** | No new crate. New regression tests are added alongside the fixes (FR-002, FR-006, SC-003, SC-005). |
| V. Incremental, Reviewable Delivery | **PASS** | P1/P2/P3 are independently shippable (spec User Stories). P3 is sliced per-crate in ascending-risk order (SC-007); each crate increment builds and tests green on its own. |
| VI. Modularity and Decoupling | **PASS** | No new cross-crate coupling. Error propagation uses each crate's existing error type (`ProviderError`, `RequestError`, `anyhow::Result`). The FR-009 `warn!` event shape is a convention, not a shared dependency. |
| VII. Backward Compatibility (NON-NEGOTIABLE) | **PASS** | FR-004 explicitly freezes the public surface. Regression coverage mandated (FR-002, FR-006). Any clippy fix that would change behavior is a documented deviation, not a silent change (US2 Acceptance Scenario 5). |
| VIII. Performance Discipline and Lean Code | **PASS** | No new dependency. Fixes use std methods already on the toolchain. Audit script uses only `rg`/`grep` (FR-010). Performance budget recorded above (zero happy-path cost). |

**Post-Phase-1 re-check**: still PASS — Phase 1 design (below) introduces no
new crate, no new dependency, no public-surface change. The only addition is
the `kimi_k2_6()` prompt function (internal `pub fn` within an existing
module, not a public-API contract).

## Project Structure

### Documentation (this feature)

```text
specs/006-bug-sweep-clippy-panic-hardening/
├── plan.md              # This file
├── research.md          # Phase 0 output
├── data-model.md        # Phase 1 output
├── quickstart.md        # Phase 1 output
├── contracts/           # Phase 1 output
│   └── error-handling-contract.md   # FR-009 canonical warn! event shape
└── tasks.md             # Phase 2 output (/speckit-tasks — NOT this command)
```

### Source Code (repository root)

```text
# P1 — correctness fix (single file + tests)
crates/joey-omo/
└── src/agents/prompts/
    └── junior.rs        # author kimi_k2_6(), fix for_model() k2.6 branch

# P2 — clippy cleanup (internal-only edits, no new files)
crates/joey-tools/        # 6 warnings (mem::take, contains, strip_prefix, &ref, cast)
crates/joey-agent-core/   # 7 warnings (&ref, filter_map→map, useless conversion, closure copy)
crates/joey-orchestration/ # 6 warnings (format!, too many args [derive], impl derive)
crates/joey-omo/          # 17 warnings (if_same_then_else [P1 also fixes], io::Error::other, &ref, to_vec, push_str, Iterator::last, sort_by_key)
crates/joey-cli/          # 18 warnings (div_ceil, &ref, let-binding return, Iterator::last, to_string)

# P3 — panic hardening (per-crate increments, ascending risk order)
crates/joey-mcp/          # increment 1: MCP JSON-RPC input (4 unwrap / 18 expect)
crates/joey-gateway/      # increment 2: platform messaging input (35 unwrap)
crates/joey-cron/         # increment 3: jobs.json parsing (217 unwrap / 2 expect)
crates/joey-core/         # increment 4: config/SQLite/auth decode (167 unwrap / 18 expect)
crates/joey-providers/    # increment 5: provider SSE/JSON (93 unwrap / 1 expect)
crates/joey-tools/        # increment 6: tool/sanitize/lsp input (244 unwrap / 4 expect)
crates/joey-agent-core/   # increment 7: turn-loop provider/model decode (136 unwrap / 17 expect) — external-input paths only

# P3 — committed audit tool (repo root, survives spec-kit lifecycle)
scripts/
└── audit-external-input-unwraps.sh   # FR-010: enumerates in-scope unwraps
```

**Structure Decision**: no new directories beyond `scripts/` (repo-root,
single file) and the spec-kit `contracts/` doc folder. All source edits are
inside existing crate `src/` trees. No new crate, no new module file — the
`kimi_k2_6()` function is added to the existing `junior.rs` alongside
`kimi_k2_7()` and `kimi_k3()`.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

No Constitution violations. The following are **not** violations but are
recorded here as explicit, justified design decisions that could otherwise
look surprising:

| Decision | Why Needed | Simpler Alternative Rejected Because |
|----------|------------|-------------------------------------|
| Author a new `kimi_k2_6()` prompt instead of collapsing the k2.6 branch into the default (`kimi_k3()`) | The k2.6 branch exists precisely because K2.6 needs its own calibration; collapsing it would silently change model behavior for K2.6 users — the opposite of the fix. | "Just delete the k2.6 branch" would make `k2.6` fall through to `kimi_k3()`, trading one wrong prompt (k2.7) for another (k3). The audit intent is a correct k2.6 prompt, not a different wrong one. |
| Per-call-site regression tests (FR-006, one per hardened unwrap) rather than per-format | Chosen explicitly in Clarifications (answer A) for maximum regression signal; the implementer asked for highest coverage. | Per-format tests are smaller but leave individual hardened sites without their own proof; the implementer accepted the test-suite growth as a task-scope cost. |
| Clippy `too many arguments` warnings (joey-orchestration: 9/7, 8/7, 10/7; joey-omo: 11/7) are recorded as deviations, not blindly refactored | These are internal functions whose argument count reflects a real domain shape; forcing them under the threshold would either introduce a parameter-bag struct (new abstraction, speculative — Constitution VIII) or split the function (changes control flow). | A parameter struct is speculative generality not exercised by a concrete need; function splitting changes behavior boundaries. Both are larger changes than this hygiene feature should make. Recorded as deviations per US2 Acceptance Scenario 5. |
| Clippy `impl can be derived` warnings — evaluated per-site | Some are genuinely derivable and safe to fix; others are on types with custom `PartialEq`/`Debug` semantics where `#[derive]` would change the trait impl. | Blind `#[derive]` could change equality or debug-output semantics (a public-surface-adjacent change). Each is decided individually; unsafe ones become deviations. |

No other deviations. All remaining clippy warnings are mechanical and
safe (`mem::take`, `strip_prefix`, `div_ceil`, `io::Error::other`, redundant
`&`, `to_string()` in format args, `Iterator::last` on double-ended
iterators, `iter().cloned().collect()` → `to_vec()`, `push_str` for single
chars, `contains` vs `iter().any()`, `filter_map` → `map`, useless
`format!`/conversion, returning a `let` binding).
