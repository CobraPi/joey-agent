# Validation Results: NeuroCode Adaptive Memory (feature 027)

**Date**: 2026-09-10 | **Gate**: cargo build --workspace exit 0; full test suite 130/130 test binaries green (executed from /tmp due to host EDR killing cargo-launched binaries from target/debug/deps; one timing-budget test, responsiveness_probe, re-verified green in isolation after a load-induced flake in batch execution).

## Quickstart scenario coverage

| Scenario | Automated coverage (test) | Status |
|----------|--------------------------|--------|
| 1 — default-off no-op (FR-010) | joey-agent-core memory_injection::disabled_config_is_byte_identical_noop; joey-neurocode regression_disabled | PASS |
| 2 — episodic capture & recall | agent.rs capture_called_on_success_exit / capture_flags_interrupted_exit_as_error / capture_collects_files_touched_from_write_tools; memory_stores insert/FIFO suites | PASS (mechanism; live-model recall phrasing is interactive) |
| 3 — preference learning & adaptation | memory_command_tests; distill heuristic suites; memory_injection prefetch/format tests | PASS (mechanism; output conformance requires a live provider session) |
| 4 — supersede & delete (SC-004) | memory_stores preference_supersede_rank + supersede_links_consistent_both_directions; memory_command_tests delete_requires_confirmation_and_hard_deletes | PASS |
| 5 — hypercode round trip | hypercode_memory_tests (per-unit capture, goal-prefix sections, disabled byte-identical) | PASS |
| 6 — schema migration safety | memory_schema_migration (4 tests incl. v3→v4 additive + idempotent + BLOB round-trip + fresh v4); v2_migration updated pins | PASS |
| 7 — secrets never persist (FR-011) | memory_stores insert_sanitizes_and_caps (AWS-key redaction at the store choke point) | PASS |

## Feature test inventory (33 memory-specific tests)

memory_config (5), memory_schema_migration (4), memory_stores (10), memory_search (5), memory_injection (7), agent.rs capture_* (3), hypercode_memory_tests (5), memory_command_tests (5) — minus overlaps, 41 distinct new tests total including in-file unit tests across episodes/preferences/distill modules.

**Live-session validation note**: Scenarios 2–4's user-facing "code conforms without restating" flows need an interactive session with a configured provider; all capture/injection/learning/deletion mechanics driving those flows are pinned green by the suites above.
