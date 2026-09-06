# Feature 026 — Quickstart §1–§7 Validation Results (2026-09-04)

Executor: T010 + T034 (+ T037 recording). Binary: `target/debug/joey`
(`cargo build -p joey-cli`, exit 0). Scratch repo: `/tmp/sk026` (git init,
`.specify/feature.json` → `specs/001-demo`, plus `.specify/scripts/` copied
from the repo so `check-prerequisites.sh` runs — the minimal scratch from the
brief lacks `common.sh`, which makes status fall back to an error line).

Constraint honored: no *deliberate* model-inference validation. Two
incidental inference turns occurred because piped `exit` is not a REPL exit
(`/quit` is) and because one dispatch command (`speckit.clarify`) was piped
to demonstrate the turn boundary; both are recorded below, not presented as
planned validation.

## Per-scenario outcomes

| § | Scenario | Outcome | Evidence |
|---|----------|---------|----------|
| 1 | Native command + dotted parity | PASS (real binary, non-TTY) | piped REPL, exit 0 |
| 2 | Bundled bodies (fresh machine) | PASS-via-automated-test | `bundled_floor_at_dispatch` ok |
| 3 | Project-local override precedence | PASS-via-automated-test | `resolution_precedence` ok (unit: `speckit_bodies::tests`) |
| 4 | Extension hooks | PASS-via-automated-test | `hook_notes_shapes`, `invalid_yaml_silent`, `hook_points_cover_all_twenty` ok |
| 5 | Orchestration + neurocode awareness | PASS-via-automated-test (partial; live hypercode fan-out NOT runnable without inference) | `conductor_lifecycle_block_appended_after_doctrine` (joey-omo) ok; joey-neurocode `context_enrichment` suite 9/9 ok |
| 6 | Config disable path | PASS (real binary, config round-trip) + PASS-via-automated-test for the no-lifecycle-block half | `config set/get` exit 0; `pre_feature_behavior_regression`, `legacy_policy_restores_pre_feature`, `disabled_config_*` ok |
| 7 | Automated gates | PASS | totals below |

## §1 — real-binary walkthrough (piped REPL)

Commands (cwd `/tmp/sk026`, `JOEY_HOME=/tmp/sk026-home`):

```
printf '/speckit-status\nspeckit.status\n/speckit-help\nspeckit.nonexistent\n/quit\n' \
  | target/debug/joey          # exit 0
```

Findings (transcript `/tmp/sk026/repl*.txt`):

- `joey --help` → exit 0.
- Piped REPL works without a TTY ("the TUI needs an interactive terminal —
  using the line REPL."), exit 0.
- `/speckit-status` → full status + `## Spec-Kit Lifecycle Context` block,
  `Step: Specify`, artifacts absent, `Next step: /speckit-specify ...`.
- `speckit.status` → **byte-identical** status + lifecycle output (verified
  by diffing the two captures; only trailing unrelated command output
  differed).
- `/speckit-help` → lists all 12 commands (10 workflow + status + help)
  with usage hints.
- `speckit.nonexistent` → `unknown spec-kit command: speckit-nonexistent` +
  all 12 commands listed. No `Closest:` line for this input — correct per
  `unknown_command_lists_and_suggests`, which pins that far-away inputs get
  no suggestion while near-misses (e.g. `speckit-plan` typo) do.
- Dispatch boundary observed: `/speckit-clarify` prints pre-flight +
  "starting the ... workflow (the agent will author the artifacts)…" then
  submits the turn — that is exactly the inference boundary; everything
  before it (slash parse, dotted parity, pre-flight, body load, handoff
  notes, lifecycle context) runs with no provider involvement.
- Incidental inference turns (honest record): (a) first attempt piped
  literal `exit`, which the REPL submitted as a chat turn (model replied
  "Goodbye!"); (b) the `speckit.clarify` boundary demo above ran a real
  3-iteration turn. Neither is claimed as planned end-to-end validation.

## §2/§3 — bundled floor + override precedence (tests)

```
cargo test -p joey-cli speckit_native::bundled_floor_at_dispatch   # 1 passed
cargo test -p joey-cli speckit_bodies::tests::resolution_precedence # 1 passed (unit)
cargo test -p joey-cli speckit_native::pre_feature_behavior_regression # 1 passed
```

All: `test result: ok. 1 passed; 0 failed`.

## §4 — hooks

`hook_notes_shapes` ok · `invalid_yaml_silent` ok ·
`hook_points_cover_all_twenty` ok (each `1 passed; 0 failed`).

## §5 — orchestration + neurocode

- `cargo test -p joey-omo conductor_lifecycle_block_appended_after_doctrine`
  → ok.
- joey-neurocode context enrichment: 9/9 ok (`--test context_enrichment`).
- Live conductor fan-out / implement write_set exclusivity require agent
  turns → NOT exercised end-to-end here.

## §6 — config round-trip (real binary)

```
$ joey config set speckit.enabled false   # exit 0
✓ Set speckit.enabled = false in /tmp/sk026-home/config.yaml
$ joey config get speckit.enabled         # exit 0
false
$ joey config set speckit.enabled true    # exit 0
✓ Set speckit.enabled = true in /tmp/sk026-home/config.yaml
$ joey config get speckit.enabled
true
```

The `/speckit-status`-while-disabled half (no lifecycle block) is covered
by `pre_feature_behavior_regression`, `legacy_policy_restores_pre_feature`,
`disabled_config_gates_new_paths`, `disabled_config_skips_discovery`.

## §7 — automated gates (exact totals)

| Suite | Result |
|---|---|
| `cargo test -p joey-cli` | **482 passed; 0 failed** |
| `cargo test -p joey-neurocode` | **329 passed; 0 failed** |
| `cargo test -p joey-omo` | **143 passed; 0 failed** (96+8+39 across targets) |

`cargo test --workspace` intentionally NOT run (delegation directive: scoped
checks only; orchestrator runs the full suite).

`cargo test -p joey-cli speckit_native` → 23 passed; 0 failed.

## Measured numbers (T037)

- SC-003 dispatch prep: `dispatch_prep_under_two_seconds` prints
  `prepare_step_opts(implement, Native) took 1949 ms` (first run) and
  `834 ms` (re-run) — both < 2000 ms budget. (The 488 ms figure quoted in
  the brief was not reproduced; recorded numbers are from this session's
  real runs. Debug-build variance is expected.)
- SC-005a scoped assembly timing: **CORRECTED (2026-09-03)** — a prior
  revision of this section claimed
  `crates/joey-neurocode/tests/scope_enrichment.rs` "does not exist".
  That claim was WRONG: the orchestrator verified the file exists and
  re-ran `cargo test -p joey-neurocode --test scope_enrichment
  -- --nocapture` → `5 passed; 0 failed`, printing
  `SC-005a scoped assembly on fixture: 0 ms`. The fixture is tiny, so it
  is too small to demonstrate the 10s→3s headline improvement; the
  budget assertion (< 3000 ms) holds at the measured 0 ms.

## Requires live model inference (NOT exercised end-to-end)

Artifact authoring via agent turns — `/speckit-specify|clarify|plan|
checklist|tasks|analyze|implement|converge|taskstoissues` and
`/speckit-constitution` — was validated only up to the turn-submission
boundary (slash parse, dotted parity, pre-flight, bundled/project body
resolution, hook notes, lifecycle gating, handoff notes, config gating) via
the real binary and the 23 `speckit_native` tests + `speckit_bodies` unit
tests. §5's conductor fan-out/write_set exclusivity and neurocode
feature-entity answers in a live session likewise remain covered only by
the joey-omo (143) and joey-neurocode (329) suites.

## Final Gate (T039) — 2026-09-03

- `cargo build --workspace`: **exit 0** — `Finished \`dev\` profile`
  (only warnings: pre-existing unused-code warnings).
- `cargo test --workspace`: **exit 0** — 81 test suites reported
  `test result: ok`, 0 failed suites, 0 failing tests. The gate ran the
  suite twice (filtered pass + count pass) — both green.
- Per-crate totals (recorded earlier): joey-cli 482 passed / 0 failed /
  1 ignored; joey-neurocode 329 passed / 0 failed (incl.
  `scope_enrichment` 5/5); joey-omo 143 passed / 0 failed; joey-core
  100 passed + 1 integration.
- SC-003 dispatch prep: 488 ms (initial joey-cli test run), 1949 ms
  first-run / 834 ms re-run in the validation harness (debug build) —
  all < 2000 ms budget.
- SC-005a scoped assembly: corrected — 0 ms on fixture (see correction
  above); < 3000 ms budget holds.

**T039 PASSED.**
