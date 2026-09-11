# Quickstart: Context Economy

Phase 1 output. Validation scenarios proving the feature end-to-end. Prerequisites: repo checked out on branch `028-please-create-feature`; cargo nightly not required (stable per rust-toolchain.toml).

**Branch**: `028-please-create-feature`

## Prerequisites
- Rust stable toolchain (rust-toolchain.toml pins it); no new dependencies.
- A scratch JOEY_HOME (use `JOEY_HOME=$(mktemp -d)` for all runs to avoid touching your real home).
- git checkout 028-please-create-feature; cargo build -p joey-tools -p joey-agent-core -p joey-core (scoped builds, not the full suite).

## V1 — Scratchpad round-trip (FR-001/002/003)
Steps: `JOEY_HOME=$(mktemp -d) cargo test -p joey-tools scratchpad` → expect: all scratchpad tests pass (roundtrip, redaction-on-write plants fake key and asserts [REDACTED] in file, tail pagination, oversized-entry rejection, registration on/off).
Outcome: scratchpad tool registered by default; file exists under scratchpads/ with sanitized dir name; secrets never persisted.

## V2 — State block (FR-004/005)
Steps: `JOEY_HOME=$(mktemp -d) cargo test -p joey-agent-core state_block` → expect: byte-identical ProviderRequest when disabled; block rendered with todos when enabled under default config; identical render on retry within turn; block never persisted to session store; ordering guard skips on tool-result tail.
Outcome: state block default-on, cache-safe, never persisted.

## V2b — Boundary cleanup & hygiene (FR-006/007/008)  
Steps (three scoped commands, one expectation group each):
- `cargo test -p joey-agent-core compression` → expect: compressor machinery (compression::* module tests) passes.
- `cargo test -p joey-agent-core hygiene` → expect: mid-turn hygiene sweep leaves tail verbatim, rewrites oldest-first, no compaction summary marker, dedup, shared budget (hygiene + boundary never double-fire), disabled parity.
- `cargo test -p joey-agent-core boundary` → expect: boundary cleanup fires once at todo-complete above threshold, never with open todos, cooldown respected, disabled parity, backstop intact.
Outcome: tool results condensed deterministically; cleanup at boundaries; backstop preserved.

## V2c — Guidance & verification nudge (FR-010/011)
Steps: `cargo test -p joey-agent-core prompt` and `-p joey-agent-core verification` → expect: guidance present under default config, absent when disabled; verify nudge includes retrieval line only when retrieval used, respecting caps.
Presence tests assert prompt contents, not bytes.
Outcome: guidance and nudge default-on, individually disableable.

## V3 — Full parity (FR-013/014, SC-004)
Steps: `JOEY_HOME=$(mktemp -d) cargo test -p joey-agent-core parity` + `-p joey-tools parity` → dedicated parity test modules assert: with every mechanism disabled, build_request output, history contents, tool registry behavior, and verify nudge are byte-identical to pre-feature golden snapshots recorded in tests.
Outcome: contractual when-disabled parity — the safety net behind default-on.

## V4 — Docs & ledger
Steps: confirm docs/context-economy.md exists and indexed; PORTING.md has the Joey-only additions ledger entry.
Outcome: living-audit duty satisfied.

## V4 NOTE: docs/context-economy.md and the PORTING.md feature-028 ledger landed with the implementation; this quickstart validates them as-is.
