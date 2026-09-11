# Quickstart: NeuroCode Adaptive Memory validation guide

Prerequisites: workspace builds (`cargo build --workspace`); a scratch git project directory; `JOEY_HOME` may be pointed at a temp home to avoid touching real `~/.joey`.

## Scenario 1 — default-off is a no-op (FR-010)

1. `cargo test -p joey-agent-core memory_injection` and `cargo test -p joey-cli neurocode_memory_command` — expect: with `neurocode.memory.enabled` absent/false, no capture, no injection, byte-identical prompt/behavior assertions pass.
2. Expected: green; `/neurocode memory status` prints disabled overview.

## Scenario 2 — episodic capture and recall (FR-001, SC-005)

1. Enable: `/neurocode memory enable` (writes config).
2. In a scratch project, run a small task to completion; end the turn.
3. `/neurocode memory list episodes` — expect one `kind=task` episode with outcome, lessons, timestamp.
4. Ask the agent "what did we try so far?" — expect the episode summarized from retrieved memory.

## Scenario 3 — preference learning and adaptation (FR-002..FR-004, SC-001)

1. State a preference explicitly ("always use constructor injection").
2. `/neurocode memory list preferences` — expect an `origin=explicit` active preference (no approval step — Q1).
3. Request related code without restating the preference — expect conformance and the preference visible in the injected block (`/neurocode memory status` shows injection stats).

## Scenario 4 — supersede and delete (FR-008/FR-009, SC-004)

1. State a contradicting preference; expect the old one `superseded` (link shown via `/neurocode memory show <id>`).
2. `/neurocode memory delete <id>` (confirm) — expect removal; later output never applies it and it never reappears.

## Scenario 5 — hypercode round trip (FR-006)

1. With memory enabled, run a small `/hypercode` goal to completion.
2. `/neurocode memory list episodes` — expect `kind=workstream`, `source=hypercode` episodes with summaries and evidence ids.
3. Run a second related goal — expect child goal prefixes contain the preferences/episodes sections (see contracts/neurocode-memory-injection.md), and output conforms to the learned preference.

## Scenario 6 — schema migration safety (FR-010, Principle VII)

1. `cargo test -p joey-neurocode memory_schema_migration` — expect: hand-crafted v3 DB opens, keeps all data, gains empty memory tables, version reads 4, double-open idempotent, vector round-trip byte-exact, delete cascades.

## Scenario 7 — secrets never persist (FR-011)

1. Complete a task whose transcript contains a fake token matching the redaction patterns; inspect the stored episode (Scenario 2 step 3) — expect the token absent.

Full suites: `cargo build --workspace && cargo test --workspace` must be green (constitution bar).
