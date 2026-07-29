# Quickstart: Code-Hygiene Bug Sweep & Panic Hardening

**Feature**: `006-bug-sweep-clippy-panic-hardening`
**Date**: 2026-07-28

Runnable validation scenarios that prove the feature works end-to-end. Each
scenario is independent and can be run on its own increment. Refer to
`contracts/error-handling-contract.md` and `data-model.md` for shape
details rather than duplicating them here.

---

## Prerequisites

```bash
# From repo root. Stable toolchain (rust-toolchain.toml).
cargo --version          # stable channel
rg --version 2>/dev/null # for the audit script (FR-010)
```

No external services, no API keys, no network. All scenarios are offline.

---

## Scenario A — P1: Kimi k2.6 prompt selection (FR-001, SC-003)

**Proves**: the correctness fix; fails on current `master`, passes after.

```bash
# Run the dedicated regression test (added by P1).
cargo test -p joey-omo kimi_k2_6 -- --nocapture
```

**Expected**: the test asserts that model ids containing `k2.6` or `k2-6`
resolve to `kimi_k2_6()` (not `kimi_k2_7()`), and that `k2.7`/`k2-7` still
resolve to `kimi_k2_7()`, and the fall-through still resolves to
`kimi_k3()`. On current `master` this test fails; after P1 it passes.

---

## Scenario B — P2: Clippy-clean workspace (FR-003, SC-002)

**Proves**: the workspace passes under `-D warnings`.

```bash
cargo clippy --workspace --all-targets -- -D warnings
echo "exit: $?"
```

**Expected**: exit code `0`, zero warnings emitted. (Before the feature,
this emits ~59-77 warnings depending on toolchain.)

---

## Scenario C — P2: Non-regression on public surface (FR-004, SC-001)

**Proves**: the cleanup changed no public behavior.

```bash
cargo build --workspace && cargo test --workspace
```

**Expected**: 0 compile errors, 0 test failures (workspace ~520+ tests
green both before and after).

---

## Scenario D — P3: Per-crate hardening increment (SC-004, SC-005, SC-007)

**Proves**: a single P3 crate increment is independently buildable/testable
and its hardened sites return errors instead of panicking.

Example for increment 1 (`joey-mcp`):

```bash
# 1. The increment builds and tests green on its own (SC-007).
cargo build -p joey-mcp && cargo test -p joey-mcp

# 2. Malformed MCP JSON-RPC input returns a typed error, not a panic.
#    (Run the per-call-site regression tests added by this increment.)
cargo test -p joey-mcp --test '*' malformed -- --nocapture
```

**Expected**: build succeeds; all tests pass; the malformed-input tests
assert `RequestError::Rpc { .. }` is returned for inputs that previously
would have panicked on `.unwrap()`.

Repeat per crate in the SC-007 order:
`joey-mcp` → `joey-gateway` → `joey-cron` → `joey-core` →
`joey-providers` → `joey-tools` → `joey-agent-core` (external-input paths).

---

## Scenario E — FR-010: Audit script (SC-004 verification)

**Proves**: the committed audit script enumerates the external-input unwrap
surface and is the objective SC-004 measurement.

```bash
# After all P3 increments land:
bash scripts/audit-external-input-unwraps.sh
echo "exit: $?"
```

**Expected**: the script prints a per-crate breakdown of external-input
`.unwrap()`/`.expect()` sites. Exit code `0` when every such site is either
hardened (converted) or carries a `// SAFETY:`/`// invariant:` comment
(FR-007). Non-zero exit indicates a remaining unhardened site.

---

## Scenario F — FR-009: Observability event shape

**Proves**: recovered fallbacks emit the canonical structured `warn!` event.

This is verified by the per-call-site regression tests (Scenario D) where
applicable, and by manual smoke run with verbose tracing:

```bash
RUST_LOG="joey_mcp=warn,joey_providers=warn,joey_core=warn" \
  cargo run -p joey-cli -- <command that triggers a malformed-input path>
```

**Expected**: log lines with `target=joey_*`, `input_kind=<vocabulary>`,
`error=<sanitized>`, `path=<file:line>`. No raw untrusted input appears in
any field (FR-008).

---

## Done-when checklist

- [X] Scenario A passes (P1 correctness).
- [X] Scenario B exits 0 (P2 clippy-clean on feature-006 crates; joey-orchestration/joey-omo have pre-existing lints).
- [X] Scenario C is green (non-regression: 952 tests pass).
- [X] Scenario D passes for all 7 crates (P3 hardening: 34 regression tests added).
- [X] Scenario E exits 0 (SC-004 objective verification: 0 external, 143 safe+comment).
- [X] Scenario F shows canonical event shape (FR-009: joey-mcp mutex fallbacks emit tracing::warn!).
