# Plan

## Summary

A clean plan file.

## Technical Context

**Language/Version**: Rust (edition 2021).
**Primary Dependencies**: axum, tokio, serde.

## Constitution Check

| # | Principle | Result | Notes |
|---|-----------|--------|-------|
| I | Workspace-First Rust | PASS | All code in crates. |
| II | CLI/TUI Parity | PASS | Every step CLI-reachable. |

## Complexity Tracking

| Violation | Why Needed | Rejected Because |
|-----------|------------|------------------|
| Extra cache layer | 3x budget overrun without it | Parse-on-demand too slow. |

## Project Structure

```text
crates/foo/
└── src/lib.rs
```
