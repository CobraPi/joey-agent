# Feature Specification: Code-Hygiene Bug Sweep & Panic Hardening

**Feature Branch**: `006-bug-sweep-clippy-panic-hardening`

**Created**: 2026-07-28

**Status**: Draft

**Input**: User description: "fix all the bugs in this AI agent"

## Specify-Phase Investigation

The phrase "fix all the bugs" is open-ended, so this specification is grounded
in an automated audit performed during `/speckit-specify` against the current
`master` (commit `a2c8ef3`). The audit establishes the concrete defect
inventory this feature targets; it is recorded here so the later `/speckit-plan`
and `/speckit-tasks` phases operate on facts, not impressions.

Audit results (full `cargo build --workspace` and `cargo test --workspace`
both green — 0 compile errors, 0 failing tests across the workspace):

1. **One confirmed runtime logic bug.** In
   `crates/joey-omo/src/agents/prompts/junior.rs:456-464`, the Kimi model
   family branch selects the wrong system prompt: a request for a `k2.6` /
   `k2-6` model returns `kimi_k2_7()` (the k2.7 prompt) instead of the k2.6
   prompt. Both the `k2.7` and `k2.6` branches are byte-identical, which is
   why clippy flagged it (`clippy::if_same_then_else`). This is incorrect
   agent behavior: the model receives guidance tuned for a different model.
2. **Clippy warnings across five crates** — `joey-tools` (6),
   `joey-agent-core` (7), `joey-orchestration` (6), `joey-omo` (17),
   `joey-cli` (18), totaling 54 by per-crate sum at audit time (the headline
   "77" reflects an earlier toolchain run; the exact count is toolchain-
   dependent and is re-measured at implementation time via `cargo clippy`).
   These are not build-breaking, but several indicate latent correctness or
   maintainability risks (e.g. a hand-rolled `div_ceil` at
   `joey-cli/src/render.rs:247` that is an off-by-one waiting to happen, a
   manual prefix-strip in `safe_commands.rs:110`, an unnecessary `let`-binding
   returned in `repl.rs:145`).
3. **A large `.unwrap()` / `.expect()` surface in production paths.** 863
   `.unwrap()` and 68 `.expect()` call sites outside test code, distributed
   `joey-tools` (198), `joey-cron` (175), `joey-core` (143), `joey-agent-core`
   (127), `joey-providers` (82), `joey-omo` (54), `joey-speckit-ui` (26),
   `joey-gateway` (28), `joey-cli` (25), `joey-tui` (3), `joey-mcp` (4),
   `joey-orchestration` (1). Most are benign (operations that provably cannot
   fail, e.g. parsing a constant), but those on external input — file I/O,
   SQLite results, provider JSON, user/config parsing, MCP messages — can
   panic the agent on malformed input and are the highest-value hardening
   targets.

These three findings map one-to-one onto the three user stories below.

## Clarifications

### Session 2026-07-28

- Q: Which crates should P3 panic-hardening cover — only the five named in SC-004, or a broader audit-driven set? → A: The five named crates (`joey-tools`, `joey-providers`, `joey-core`, `joey-mcp`, `joey-gateway`) **plus** the external-input paths of `joey-cron` (reads/parses `jobs.json`) and `joey-agent-core` (decodes provider/model JSON in the turn loop). These two have the next-largest untrusted-input surfaces; including them honors P3's "graceful degradation on malformed external input" rationale while still bounding effort to a concrete, audited subset rather than the whole tree.
- Q: How granular must the malformed-input regression tests be — one per hardened call site, or one per external-input format/protocol? → A: One dedicated malformed-input regression test **per hardened `.unwrap()`/`.expect()` call site** (highest coverage option). This maximizes regression signal: every individual hardened site carries its own proof that the formerly-panicking input now yields a typed error. Expected to result in a large test suite addition (one test per external-input unwrap removed across the 7 in-scope crates), counted as task-scope growth in `/speckit-plan`.
- Q: What is the logging/observability contract for hardened paths that recover via explicit fallback (log level, structured fields, raw-input capture)? → A: Canonical structured `tracing::warn!` event per recovered fallback, with fields `target` (crate name), `error`, `input_kind`, and `path`. Log the sanitized/redacted error description only — never the raw malformed input verbatim (defers to FR-008 redaction and `docs/security.md`). `warn!` (not `error!`/`debug!`) so recovered-but-operationally-meaningful events are visible at default log levels without implying abort or hiding in verbose-only output.
- Q: How should the P3 hardening pass be sliced for reviewable incremental delivery (Constitution Principle V)? → A: **Per-crate increments**, ordered ascending by external-input risk: `joey-mcp` → `joey-gateway` → `joey-cron` → `joey-core` → `joey-providers` → `joey-tools` → `joey-agent-core` (external-input paths only). Each increment MUST independently pass `cargo build -p <crate> && cargo test -p <crate>` (Constitution Principles I + V). The **first** landed increment (`joey-mcp`, smallest external-input surface — 4 unwraps in the audit) establishes the canonical FR-009 structured `tracing::warn!` event shape and the FR-006 per-call-site regression-test pattern that subsequent increments replicate. Starting from the smallest, most contained crate lets the shared contracts stabilize before the largest surface (`joey-tools`, 198 unwraps) is touched.
- Q: Does the specify-phase audit script become a committed, reusable artifact so that SC-004's "re-run of the audit script" is objectively verifiable? → A: Yes — commit a small reproducible audit script at repo-root `scripts/audit-external-input-unwraps.sh` (or equivalent) that enumerates `.unwrap()`/`.expect()` on external-input paths in the 7 in-scope crates. SC-004's "re-run" refers to this committed script. No new runtime dependency (shell + `rg`/`grep`, both already available — Constitution VIII compliant). The script lives at the repo root (not in `.specify/`, which is spec-kit-owned) so it survives beyond the spec-kit lifecycle and future features can reuse it.

## User Scenarios & Testing *(mandatory)*

<!--
  Each story is an independently shippable, independently testable slice.
  P1 is the only one that fixes a user-visible correctness defect; P2 and P3
  are hardening/hygiene that reduce latent risk. They are deliberately split
  so any single one delivers a verifiable improvement on its own and so the
  correctness fix is never blocked behind the larger cleanup effort.
-->

### User Story 1 - Fix Confirmed Logic Bugs (Priority: P1)

As a user running the agent against a Kimi `k2.6` model, I expect the agent to
load the guidance prompt authored for k2.6 — not the k2.7 prompt — so that the
model behaves the way the selected model is tuned for. More broadly, as the
project maintainer, I want every confirmed correctness defect found during the
audit to be fixed at its root cause, not patched over.

**Why this priority**: This is the only story that changes user-observable
behavior, and it changes it from *wrong* to *right*. Selecting the wrong
system prompt measurably degrades model behavior, which is the core thing the
agent exists to do. Everything else in this feature is risk reduction;
correctness comes first.

**Independent Test**: With a Kimi-family model id containing `k2.6` or `k2-6`,
assert that the resolved prompt is the k2.6 prompt (not the k2.7 prompt). The
fix is fully testable in isolation by exercising the prompt-selection function
directly and delivers immediately correct model selection.

**Acceptance Scenarios**:

1. **Given** a model id whose lowercased name contains `k2.6` or `k2-6`,
   **When** the prompt-selection function resolves the Kimi-family prompt,
   **Then** it returns the k2.6 prompt and **not** the k2.7 prompt.
2. **Given** a model id containing `k2.7` or `k2-7`, **When** the prompt is
   resolved, **Then** it still returns the k2.7 prompt (regression guard).
3. **Given** a Kimi model id matching neither k2.6 nor k2.7, **When** the
   prompt is resolved, **Then** it falls through to the existing default
   (`kimi_k3()`) unchanged.
4. **Given** any model id outside the Kimi family, **When** the prompt is
   resolved, **Then** behavior is byte-for-byte unchanged from `master`
   (regression guard for the other families).
5. **Given** the fix is applied, **When** `cargo clippy --workspace` runs,
   **Then** the `clippy::if_same_then_else` warning at `junior.rs:456` is gone.

---

### User Story 2 - Make the Workspace Clippy-Clean (Priority: P2)

As the project maintainer, I want `cargo clippy --workspace` to emit zero
warnings (i.e. pass under `-D warnings`), so that clippy can be trusted as a
gate that catches new defects instead of drowning them in 77 pre-existing
ones, and so the codebase honors Constitution Principle VIII (Performance
Discipline and Lean Code).

**Why this priority**: Clippy noise actively hides real bugs — the P1 defect
above was *found* through a clippy warning. A clean baseline turns clippy into
a reliable regression-prevention signal. It is second priority because it
improves maintainability and bug-prevention rather than fixing an active
misbehavior, and because it is mechanical, low-risk work that must not block
the correctness fix.

**Independent Test**: Run `cargo clippy --workspace -- -D warnings` and confirm
it exits successfully with no warnings. Delivers a lint-clean baseline on its
own, independent of P1 and P3.

**Acceptance Scenarios**:

1. **Given** the workspace on `master` emits clippy warnings (exact count
   toolchain-dependent; ~54–77), **When** the cleanup is applied, **Then**
   `cargo clippy --workspace -- -D warnings` succeeds with zero warnings.
2. **Given** the manual `div_ceil` reimplementation at `joey-cli/src/render.rs:247`,
   **When** it is rewritten, **Then** it uses the standard `.div_ceil()`
   method and the rendered line-count wrapping is unchanged (and now
   provably free of an off-by-one).
3. **Given** the manual prefix-strip in `joey-cli/../safe_commands.rs:110`,
   **When** it is rewritten, **Then** it uses `str::strip_prefix` and the
   safe-command classification result is unchanged.
4. **Given** the fixes touch only internal implementation, **When** the full
   test suite runs, **Then** all previously-passing tests still pass and no
   public API, CLI flag, config key, or on-disk format changes.
5. **Given** a clippy suggestion that *would* change behavior or a public
   surface, **When** it is encountered, **Then** it is NOT applied blindly;
   instead it is recorded as a deliberate deviation in the plan's Complexity
   Tracking section (Constitution: gates must be evaluated honestly).

---

### User Story 3 - Harden Panic-Prone Unwrap/Expect on External Input (Priority: P3)

As a user, I want the agent to degrade gracefully (a clear error, a skipped
operation, a retry) instead of panicking and aborting when it encounters
malformed external input — a corrupt SQLite row, an unexpected provider JSON
shape, a malformed config value, a bad MCP message, a non-UTF-8 file. As the
maintainer, I want `.unwrap()`/`.expect()` confined to sites where failure is
provably impossible, and removed from every path driven by external data.

**Why this priority**: A panic in a long-running agent session is the worst
failure mode — it throws away session state and offers no recovery. But it is
lowest priority here because (a) the audit found no currently-failing path,
so this is risk reduction rather than an active fix, and (b) it is the largest
surface (hundreds of sites) and must be done carefully to avoid changing
public behavior. It is split out so the high-value P1/P2 work lands first.

**Independent Test**: Audit the highest-risk crates (those touching external
I/O: `joey-tools`, `joey-providers`, `joey-core`, `joey-mcp`, `joey-gateway`,
plus the external-input paths of `joey-cron` (`jobs.json` parsing) and
`joey-agent-core` (provider/model JSON decoding in the turn loop)) and
confirm unwraps on external input are replaced with propagated errors or
explicit fallbacks; feed a malformed input fixture through the affected path
and confirm it returns an error instead of panicking. Delivers a measurably
more robust agent on its own.

**Acceptance Scenarios**:

1. **Given** a code path that parses provider SSE/JSON or MCP JSON-RPC input
   and currently `.unwrap()`s a field, **When** it receives malformed input
   that is missing that field, **Then** it returns/propagates a typed error
   instead of panicking.
2. **Given** a code path that reads/decodes user files (config, context
   files, skill files) and `.unwrap()`s a parse, **When** the file is
   malformed or non-UTF-8, **Then** it reports a clear, sanitized error
   instead of panicking.
3. **Given** a `.unwrap()`/`.expect()` on a value that provably cannot fail
   (e.g. parsing a compile-time constant, indexing a just-built array), **When**
   the hardening pass reviews it, **Then** it MAY be left in place but MUST
   carry a `// SAFETY:` / invariant comment explaining why it cannot panic,
   OR be converted to the idiomatic non-panicking form.
4. **Given** the hardening touches error-handling internals, **When** the
   public test suite and a smoke run of the CLI/TUI execute, **Then** all
   externally observable behavior (CLI flags, exit codes, config keys, on-disk
   formats, wire payloads to providers) is unchanged.
5. **Given** the pass is complete for a crate, **When** targeted fuzz/property
   or malformed-input regression tests run, **Then** they assert the formerly
   panicking input now yields an error (regression coverage per Constitution
   Principle VII).
6. **Given** P3 is sliced per-crate per Clarifications, **When** each
   increment lands in the prescribed order (`joey-mcp` → `joey-gateway` →
   `joey-cron` → `joey-core` → `joey-providers` → `joey-tools` →
   `joey-agent-core` external-input paths), **Then** that increment alone
   satisfies `cargo build -p <crate> && cargo test -p <crate>` green
   (Constitution Principles I + V), and the first increment (`joey-mcp`)
   establishes the canonical FR-009 `tracing::warn!` event shape and the
   FR-006 per-call-site regression-test pattern.

---

### Edge Cases

- **What about the `panic!`/`unreachable!` sites?** The audit counts
  `panic!`/`unreachable!` separately. Any `unreachable!()` reachable only via
  a genuine programming error (an enum arm that cannot occur) may stay, but
  must be justified; any `unreachable!()`/`panic!()` reachable via external
  input must be converted to a proper error. Enumeration is included in the
  FR-010 audit script scope alongside `.unwrap()`/`.expect()`; per-crate
  review during `/speckit-implement` applies the FR-007 SAFETY-comment rule.
  Do not blanket-remove.
- **What if a clippy fix legitimately changes behavior?** Per Constitution
  Principle VII, no public surface may change without a MAJOR bump + migration
  path. Such fixes are deferred out of this feature and recorded as deliberate
  deviations, not silently applied.
- **What if removing an unwrap forces a `Result` through many layers?**
  Prefer the existing project error type (`ProviderError`, crate-local error
  enums) over `anyhow`/panics. Where a full `Result` propagation is
  disproportionate, an explicit fallback with a logged warning is acceptable
  for non-critical paths (e.g. rendering). Decide per-site in `/speckit-plan`.
- **What about test-only `.unwrap()`?** Out of scope. Unwraps inside
  `#[cfg(test)]` and `tests/` are idiomatic and stay.
- **Non-UTF-8 / binary file content.** Hardening must not merely move the
  panic; it must surface a sanitized, user-readable message consistent with
  the existing redaction/sanitization layers (`docs/security.md`).
- **Provider partial/streamed responses.** A field present-but-empty or
  missing-mid-stream must be handled as the provider protocols already
  specify, not as a panic; cross-check `docs/providers.md` and the ported
  upstream behavior before changing handling.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: The system MUST resolve the Kimi-family prompt correctly: model
  ids matching `k2.6`/`k2-6` MUST select the k2.6 prompt, `k2.7`/`k2-7` the
  k2.7 prompt, and other Kimi ids the existing default — fixing
  `junior.rs:456-464`.
- **FR-002**: The system MUST ship regression tests asserting the corrected
  Kimi prompt selection (one assertion per branch, including the
  fall-through), since this touches a behavior contract (Constitution VII).
- **FR-003**: `cargo clippy --workspace -- -D warnings` MUST exit zero on the
  workspace after the cleanup; all 77 currently-emitted warnings MUST be
  resolved (fixed or, where behavior-changing, documented as a deferred
  deviation).
- **FR-004**: Clippy fixes MUST NOT alter any public surface — public APIs,
  CLI flags/exit codes, config keys, on-disk file formats (incl. SQLite
  `SCHEMA_VERSION = 22` and `~/.joey` compatibility), wire payloads, or trait
  definitions. The full existing test suite MUST remain green.
- **FR-005**: The system MUST remove `.unwrap()`/`.expect()` from code paths
  driven by external input (provider responses, MCP messages, file/config
  parsing, SQLite decode paths) in favor of propagated typed errors or
  explicit, logged fallbacks (per the FR-009 observability contract).
- **FR-006**: Each panic-hardening change in a crate touching a public surface
  or external-input boundary MUST ship with **one dedicated regression test
  per hardened `.unwrap()`/`.expect()` call site**, feeding the input that
  formerly triggered the panic and asserting a typed error is returned
  (highest coverage option: every hardened site carries its own proof).
- **FR-007**: Retained `.unwrap()`/`.expect()`/`unreachable!()`/`panic!()`
  sites that cannot fail MUST carry an inline invariant/SAFETY comment
  explaining why; sites that *can* fail MUST be converted (no silent
  retention).
- **FR-008**: All error messages surfaced to the user from hardened paths
  MUST pass through the existing secret-redaction/sanitization layer and
  MUST NOT leak secrets or raw untrusted content.
- **FR-009**: Each recovered fallback on a hardened external-input path MUST
  emit a canonical structured `tracing::warn!` event with fields:
  - `target` — the crate name emitting the event,
  - `error` — the sanitized/redacted error description (not the raw input),
  - `input_kind` — a short classification of the input source
    (e.g. `"provider_json"`, `"mcp_jsonrpc"`, `"jobs_json"`, `"sqlite_row"`,
    `"config_file"`, `"context_file"`),
  - `path` — the source file path (or a stable identifier) of the call site.
  Raw malformed input MUST NOT be logged verbatim (FR-008 redaction applies).
  Propagated typed errors (no fallback) need not emit a `warn!` themselves
  since they surface at the caller's error-handling boundary.
- **FR-010**: The system MUST ship a committed, reproducible audit script at
  `scripts/audit-external-input-unwraps.sh` (or equivalent) that enumerates
  `.unwrap()`/`.expect()` **and** `panic!()`/`unreachable!()` call sites on
  external-input paths in the 7 in-scope crates. The script is the canonical
  measurement backing SC-004 (its "re-run of the specify-phase audit script"
  refers to this committed artifact). It MUST introduce no new runtime
  dependency (shell + `rg`/`grep` only — Constitution VIII) and MUST live at
  the repo root rather than under `.specify/` so it survives beyond the
  spec-kit lifecycle and is reusable by future features.

### Key Entities *(include if feature involves data)*

- **Defect Inventory**: the three-category audit result above (logic bug,
  clippy warnings, unwrap surface), each item a tractable, locatable unit.
- **Panic-Risk Tier**: a per-call-site classification — *safe* (provably
  cannot fail), *external-input* (driven by untrusted data, must harden),
  *internal-but-recoverable* (should propagate). The plan materializes this
  into a concrete per-crate work list.
- **Public-Surface Contract**: the frozen set (APIs, CLI, config keys,
  formats, traits, wire payloads) that no change in this feature may break;
  used as the regression gate.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: `cargo test --workspace` reports 0 failures both before and
  after the feature (non-regression baseline preserved).
- **SC-002**: `cargo clippy --workspace -- -D warnings` exits 0 (down from 77
  warnings on `master`).
- **SC-003**: 100% of `k2.6`/`k2-6` model ids resolve to the k2.6 prompt
  (verified by a dedicated regression test that fails on current `master` and
  passes after FR-001).
- **SC-004**: The count of `.unwrap()`/`.expect()` on external-input paths in
  `joey-tools`, `joey-providers`, `joey-core`, `joey-mcp`, and `joey-gateway`,
  **plus** the external-input paths of `joey-cron` (`jobs.json` parsing) and
  `joey-agent-core` (provider/model JSON decoding in the turn loop), is
  reduced to zero (or each remaining site carries a SAFETY/invariant
  comment), measured by a re-run of the committed audit script
  (`scripts/audit-external-input-unwraps.sh`, per FR-010).
- **SC-005**: At least one malformed-input regression test exists **per
  hardened `.unwrap()`/`.expect()` call site** (not per format/protocol) and
  passes, each asserting a typed error rather than a panic. The expected
  test-suite size growth is one test per external-input unwrap removed across
  the 7 in-scope crates, and must be accounted for in `/speckit-plan`'s
  Complexity Tracking.
- **SC-006**: No public surface change is introduced without a documented,
  justified deviation recorded in the plan's Complexity Tracking section
  (Constitution compliance).
- **SC-007**: P3 hardening is delivered as **per-crate increments**, ordered
  ascending by external-input risk (`joey-mcp` → `joey-gateway` →
  `joey-cron` → `joey-core` → `joey-providers` → `joey-tools` →
  `joey-agent-core` external-input paths). Each increment MUST independently
  satisfy `cargo build -p <crate> && cargo test -p <crate>` green on landing
  (Constitution Principles I + V). The first landed increment (`joey-mcp`)
  defines the canonical FR-009 structured `tracing::warn!` event shape and
  the FR-006 per-call-site regression-test pattern that subsequent
  increments replicate.

## Assumptions

- "All the bugs" is interpreted as the concrete, audit-verified defect
  inventory above, not an open-ended guarantee of zero defects (which is
  unverifiable). Anything beyond this inventory requires a new
  `/speckit-specify` cycle.
- The audit baseline (commit `a2c8ef3`) is the reference for "before"; all
  before/after counts are taken against it.
- Clippy version is the one pinned by the stable toolchain in
  `rust-toolchain.toml` (currently reporting as rust-1.96.0 lint set); results
  are reproducible on that toolchain.
- Fixes use idiomatic std/library methods already available on the stable
  toolchain (e.g. `div_ceil`, `str::strip_prefix`, `std::io::Error::other`,
  `std::mem::take`) — no new dependencies are introduced (Constitution VIII).
- Panic hardening targets *runtime external-input* paths; build-time/test
  panics and provably-infallible sites are explicitly out of scope except for
  the SAFETY-comment requirement (FR-007).
- All work is additive/non-breaking per Constitution Principle VII; any
  exception is gated on a MAJOR-version discussion and migration path recorded
  in `/speckit-plan`, not done silently.
