# Research: Code-Hygiene Bug Sweep & Panic Hardening

**Feature**: `006-bug-sweep-clippy-panic-hardening`
**Date**: 2026-07-28

Phase 0 resolves every `NEEDS CLARIFICATION` from the Technical Context and
documents the best-practice decisions behind each design choice. No item
below is speculative — each is grounded in the actual codebase (examined
during `/speckit-plan`) and the existing constitution.

---

## R1 — The `kimi_k2_6()` prompt does not exist; it must be authored

### Finding

`for_model()` at `junior.rs:456-464` has a `k2.6`/`k2-6` branch that calls
`kimi_k2_7()`. Investigation confirms there is **no `kimi_k2_6()` function**
in the file — only `kimi_k3()` (line 366) and `kimi_k2_7()` (line 407). The
P1 fix is therefore not a one-character swap; it requires authoring the
missing K2.6 prompt and wiring the branch to call it.

### Decision

Author a new `pub fn kimi_k2_6() -> &'static str` in `junior.rs`, placed
between `kimi_k3()` and `kimi_k2_7()` (preserving the file's
ascending-generation ordering). The prompt is calibrated for Kimi K2.6 —
the predecessor generation to K2.7 — following the same structural template
as the existing Kimi variants (identity line, execution-vs-orchestration
stance, "keep going", scope discipline, verify-before-done, track
multi-step work, recover-from-failure sections).

### Rationale

- The k2.6 branch exists precisely because K2.6 warrants its own guidance.
  Collapsing it into the default `kimi_k3()` (the fall-through) would trade
  one wrong prompt for another — K3 calibration differs from K2.6.
- K2.6 predates K2.7. The K2.7 prompt emphasizes "Opus 4.8-class
  steerability with GPT-5.5 directness" — capabilities K2.6 does not have.
  The K2.6 prompt is written for K2.6's actual strengths (strong
  instruction-following, capable coding, but less steerability than K2.7).

### Alternatives considered

1. **Delete the k2.6 branch** so `k2.6` falls through to `kimi_k3()`.
   Rejected: trades wrong-prompt-k2.7 for wrong-prompt-k3; the audit intent
   is a *correct* k2.6 prompt.
2. **Make k2.6 an alias for k2.7** (keep current behavior, document it).
   Rejected: the spec (FR-001) mandates that k2.6 selects the k2.6 prompt,
   *not* the k2.7 prompt. This is the bug being fixed.

### Calibration basis for the K2.6 prompt

K2.6 is calibrated against K2.7's prompt by *reducing* the steerability
claims and *emphasizing* verification rigor (K2.6 benefits more from
explicit verify-before-done discipline, being less able to "self-correct"
mid-stream than K2.7). The prompt is shorter than K3's (K2.6 does not need
the "reasoning depth is the point" framing — that is K3's differentiator)
but retains the structural sections common to both existing variants. The
exact wording is finalized during implementation; the structural contract
(sections present, `&'static str` return, no upstream text altered) is
fixed here.

---

## R2 — Error-propagation vehicles: reuse existing types, add none

### Finding

The codebase already has typed error types in the exact crates P3 targets:

| Crate | Error type | Location | Notes |
|---|---|---|---|
| `joey-providers` | `ProviderError` (thiserror enum, 14 variants) | `src/error.rs:10` | Has `Parse(String)` for malformed provider JSON — the natural target for SSE/JSON hardening. |
| `joey-mcp` | `RequestError` (thiserror enum: `Rpc`, `Transport`) | `src/lib.rs:163` | Private to the crate; MCP JSON-RPC parse failures map to `Rpc`. |
| `joey-tools` | `LspError` (thiserror) in `src/lsp.rs:748`; `anyhow::Result` elsewhere | — | Tool execution already returns `Result`; unwrap removal propagates into the existing `Result`. |
| `joey-core` | `anyhow::Result` (config, paths, auth, session) | `src/config.rs`, `src/lib.rs`, `src/auth_store.rs` | Idiomatic for application-layer code; `.context()` adds detail. |
| `joey-gateway`, `joey-cron`, `joey-agent-core` | `anyhow::Result` / crate-local results | — | Same pattern. |

### Decision

P3 hardening propagates errors via **each crate's existing error type**. No
new error enum is introduced (Constitution VIII: avoid speculative
abstractions). Specifically:

- Provider JSON/SSE unwraps → `ProviderError::Parse(sanitized_msg)`.
- MCP JSON-RPC unwraps → `RequestError::Rpc { message: sanitized_msg }`.
- Config/file/SQLite unwraps (joey-core, joey-cron) → `anyhow::Result` with
  `.context()` for the failure detail.
- Tool unwraps → existing tool `Result`.

### Rationale

- The error types already exist and are already the propagation vehicle on
  non-panic paths in these crates. Adding a new "hardening error" type
  would be speculative generality (Constitution VIII) and would force
  coordinated edits across crate boundaries (Constitution VI).
- `anyhow` is already the convention in `joey-core` — using it for config/
  file/SQLite hardening is consistent, not a new pattern.

### Alternatives considered

1. **A shared workspace-wide `HardeningError`**. Rejected: creates
   cross-crate coupling (Constitution VI) and a new public surface
   (Constitution VII) for no concrete benefit — each crate already has a
   working error type.
2. **Convert all `anyhow` paths to `thiserror` enums**. Rejected: out of
   scope, large surface, changes public API in `joey-core`. The feature is
   hygiene, not an error-stack refactor.

---

## R3 — FR-009 canonical `tracing::warn!` event shape

### Finding

`tracing` is already used pervasively (250+ call sites across
`joey-agent-core`, `joey-cli`, `joey-core`). The existing style is
unstructured format strings, e.g. `tracing::warn!("compression ...: {}", e)`.
No canonical structured-event convention exists yet.

### Decision

FR-009Recovered fallbacks emit:

```rust
tracing::warn!(
    target: env!("CARGO_CRATE_NAME"),  // = "joey_mcp", "joey_providers", ...
    error = %sanitized_error_string,   // Display-formatted, redacted
    input_kind = "provider_json",      // see fixed vocabulary below
    path = concat!(file!(), ":", line!()), // or a stable literal
    "recovered from malformed external input via fallback"
);
```

**`input_kind` vocabulary** (fixed set, one per source class):

| Value | Source |
|---|---|
| `"provider_json"` | provider chat-completion / responses JSON |
| `"provider_sse"` | provider SSE stream chunk |
| `"mcp_jsonrpc"` | MCP JSON-RPC request/response/notification |
| `"jobs_json"` | `jobs.json` (joey-cron) |
| `"sqlite_row"` | SQLite decoded row (session store) |
| `"config_file"` | `config.yaml` / `.env` / profile config |
| `"context_file"` | context/skill file read from disk |
| `"auth_store"` | auth-store credential decode |
| `"gateway_message"` | platform-adapter message-event decode (joey-gateway) |

### Rationale

- **`warn!` level**: recovered fallbacks are not fatal (`error!` would
  imply abort) but are operationally meaningful — visible at default log
  levels, not hidden behind `debug!`/`trace!`. Chosen in Clarifications.
- **Structured fields**: enable `tracing_subscriber` filtering
  (`RUST_LOG="joey_providers=warn"` then grep `input_kind=provider_sse`)
  without grepping free-form text. Future incident triage benefits.
- **`target = env!("CARGO_CRATE_NAME")`**: a compile-time crate name, no
  runtime cost, correctly attributes the event to the emitting crate.
- **`error = %...` (Display)**: the sanitized error description only. Raw
  input is never logged (FR-008). Sanitization passes through
  `joey_core::redact::redact_sensitive_text` for any string that may
  contain secret-adjacent content (URLs with credentials, env values).

### Alternatives considered

1. **Free-form `tracing::warn!("...")`**. Rejected: no filterable fields;
  inconsistent across 100+ sites.
2. **`error!` level**. Rejected: alarm fatigue for recovered fallbacks the
  agent continued past.
3. **A dedicated `tracing::event!` macro / span per input kind**. Rejected:
  speculative generality; the structured-field convention above is
  sufficient and lighter-weight.

### Helper convention

To avoid field-name drift across sites, the first landed P3 increment
(`joey-mcp`, SC-007) establishes the canonical macro call shape. Later
increments copy it verbatim (only `input_kind` and the message string
change). A private helper macro is **not** introduced (it would be a
cross-crate shared utility — Constitution VI coupling); each site writes
the `tracing::warn!` call directly, guided by the contract in
`contracts/error-handling-contract.md`.

---

## R4 — FR-010 audit script design

### Finding

SC-004 requires a "re-run of the specify-phase audit script" to verify the
external-input unwrap count dropped to zero. The specify-phase audit was
ad hoc (run once during `/speckit-specify`); no committed script exists.

### Decision

Commit `scripts/audit-external-input-unwraps.sh` — a `bash` + `rg` script
(no new runtime dependency — Constitution VIII) that:

1. Enumerates `.unwrap()` and `.expect()` call sites in `src/` (excluding
   `tests/` and `#[cfg(test)]` modules) across the 7 in-scope crates.
2. Classifies each site heuristically: **external-input** if the call is
  within a function that touches file I/O, SQLite decode, provider/SSE
  parse, MCP JSON-RPC, or config parsing (detected via surrounding-function
  signature heuristics and a curated path allowlist); **safe** otherwise.
3. Prints: total count, per-crate breakdown, and a list of sites lacking a
   `// SAFETY:` / `// invariant:` comment on the preceding line.
4. Exits non-zero if any **external-input** site remains unhardened (i.e.
   has neither a SAFETY comment nor a typed-error conversion).

### Rationale

- `rg` is already available (AGENTS.md notes it is the repo's search tool).
- The classification heuristic is imperfect (static analysis of panic-safety
  is undecidable in general), but it is a *useful gate*: it catches
  regressions where a new `.unwrap()` lands on an external-input path
  without a justification comment. False positives (a provably-safe unwrap
  the heuristic flags) are resolved by adding a `// SAFETY:` comment —
  which is exactly FR-007's requirement.
- Lives at repo-root `scripts/` (not `.specify/scripts/`) so it survives
  beyond the spec-kit lifecycle and future features can reuse it.

### Alternatives considered

1. **A `cargo` test that asserts unwrap count**. Rejected: would require
  parsing Rust source in-test (fragile) or depending on `cargo` internals.
  A shell script wrapping `rg` is simpler and Constitution VIII-compliant.
2. **A `clippy` custom lint**. Rejected: custom lints require a nightly
  compiler and a separate lint crate — disproportionate cost and a new
  dependency/toolchain constraint.
3. **Manual `rg` count at acceptance time only**. Rejected: SC-004 becomes
  unverifiable and non-reproducible (the original ambiguity).

### Classification heuristic (concrete)

A call site at `path:line` is classified **external-input** if its enclosing
function or module path matches any of:

- `crates/joey-providers/src/**` with a `serde_json::from_*`,
  `str::from_utf8`, `.json()`, SSE `parse` in the same function.
- `crates/joey-mcp/src/**` with `serde_json::from_*` or `Value::as_*` in
  the same function.
- `crates/joey-core/src/config.rs`, `auth_store.rs`, session-store decode
  functions.
- `crates/joey-cron/src/**` jobs.json load/parse functions.
- `crates/joey-gateway/src/**` message-decode functions.
- `crates/joey-tools/src/sanitize*.rs`, `lsp.rs`, tool-result decode.
- `crates/joey-agent-core/src/**` provider/model JSON decode in the turn
  loop (curated function allowlist).

The exact function-allowlist is finalized in the script and documented in
its header comment. Sites not matching are **safe** (but still counted).

---

## R5 — Clippy deviation boundary: which warnings are NOT auto-applied

### Finding

Of the ~59-77 clippy warnings, a subset cannot be safely auto-applied
without risking a public-surface or behavior change. These must be
evaluated per-site and recorded as deviations where not applied.

### Decision — per-warning-class policy

| Clippy warning | Policy | Rationale |
|---|---|---|
| `mem::take` | **Apply** | Strictly equivalent, no behavior change. |
| `manual_div_ceil` (`div_ceil`) | **Apply** | std method, provably equivalent (removes off-by-one risk). |
| `manual_strip` (`strip_prefix`) | **Apply** | std method, equivalent. |
| `io_other_error` (`std::io::Error::other`) | **Apply** | std method, equivalent. |
| `needless_borrow` (`&ref`) | **Apply** | Equivalent, compiler-confirmed. |
| `useless_format` / `useless_conversion` | **Apply** | Equivalent. |
| `redundant_closure_for_method_calls` / `redundant_closure` | **Apply** | Equivalent. |
| `iter_cloned_collect` → `to_vec()` | **Apply** | Faster, equivalent. |
| `single_char_push_str` → `push` | **Apply** | Equivalent. |
| `iter_any` / `contains` vs `iter().any()` | **Apply** | Equivalent or faster. |
| `filter_map_next` / `map_flatten` / `unnecessary_filter_map` (`filter_map`→`map`) | **Apply** | Equivalent. |
| `let_and_return` | **Apply** | Equivalent. |
| `double_ended_iterator_last` (`Iterator::last`) | **Apply** | Equivalent, removes needless full iteration. |
| `to_string_in_format_args` | **Apply** | Equivalent. |
| `should_implement_trait` / `new_without_default` | **Evaluate** | May change public API. Deviation if so. |
| `too_many_arguments` | **Deviation** | Forcing under threshold introduces speculative structs (Constitution VIII). Record deviation, do not refactor. |
| `derivable_impls` (`impl can be derived`) | **Evaluate per-site** | Safe if the manual impl matches the derived one; deviation if semantics differ (custom `PartialEq`/`Debug`). |
| `if_same_then_else` | **Apply via P1** | The flagged site (`junior.rs:456`) is the P1 bug — fixing it (distinct k2.6 branch) resolves the warning. |

### Rationale

This table is the authoritative application policy. It honors US2
Acceptance Scenario 5: every clippy fix that *would* change behavior or a
public surface is a documented deviation, never a blind application. The
deviations land in this plan's Complexity Tracking section (already
populated above).

### Alternatives considered

1. **Apply everything `--fix` suggests**. Rejected: violates Constitution
   VII for the deviation cases and US2 Acceptance Scenario 5.
2. **Apply nothing, suppress all with `#[allow]`**. Rejected: defeats the
   purpose of P2 (a trustworthy lint gate) and hides the P1 bug class.

---

## R6 — Upstream-fidelity constraint on prompt wording

### Finding

AGENTS.md and the constitution emphasize that guidance strings are ported
verbatim from upstream and must not be "cleaned up." The new `kimi_k2_6()`
prompt is a *new* function, not a port — but it lives alongside ported
prompts and must follow the same structural conventions.

### Decision

- The new `kimi_k2_6()` prompt is **original content** (no upstream K2.6
  prompt exists to port — the bug was that the branch was a copy-paste of
  k2.7). It is authored to match the structural template of the existing
  Kimi variants (section headings, tone, length class).
- No existing prompt function's text is altered. P2 clippy cleanup does not
  touch the `&'static str` prompt bodies (they are raw string literals,
  not subject to the lint warnings in question).
- The `for_model()` fix changes only which function the k2.6 branch *calls*,
  not the text of any existing prompt.

### Rationale

Respects the upstream-fidelity hard constraint (AGENTS.md) while allowing
the bug fix to add the missing variant. The structural-template match
keeps the new prompt consistent with the model's expectations.

---

## Summary of resolved unknowns

| Item | Resolution |
|---|---|
| `kimi_k2_6()` existence | Does not exist — author it (R1). |
| Error type for hardening | Reuse each crate's existing type (R2). |
| FR-009 event shape | Structured `tracing::warn!` with fixed fields + vocabulary (R3). |
| Audit script | `scripts/audit-external-input-unwraps.sh`, `rg`-based (R4). |
| Clippy deviation boundary | Per-class policy table (R5); deviations recorded in plan. |
| Upstream fidelity | New prompt is original; existing prompts untouched (R6). |

No `NEEDS CLARIFICATION` items remain.
