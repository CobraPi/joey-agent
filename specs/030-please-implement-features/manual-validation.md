# Manual Validation — Feature 030 Quickstart M1/M2 (T031 v2)

- **Date:** 2026-09-13
- **Environment:** macOS 26.6.2, provider zai / model glm-5.3 (real `~/.joey` config: `.env` + `config.yaml`), branch `028-please-create-feature`
- **Method:** `crates/joey-orchestration/tests/manual_m1m2.rs` (new) — two `#[ignore]`d `#[tokio::test]`s run against the REAL provider (no mock server, no `JOEY_HOME` override — real home is intentional for M1). Built with `cargo test -p joey-orchestration --test manual_m1m2 --no-run`; the dep binary was copied to `/tmp/t031b.bin` (EDR-safe) and run as `cd crates/joey-orchestration && /tmp/t031b.bin --ignored --test-threads=1 [--nocapture]`.

## Results

### M1 — `m1_end_to_end_feel` (governance enabled, `data_dir: None` → real `~/.joey/delegation`)

- Manager literal under test: `GovernanceConfig { enabled: true, task_timeout_secs: 120, data_dir: None, ..Default::default() }`
- 3 concurrent identical dispatches, goal "Reply with exactly: OK", `max_turns = Some(2)` → **3/3 success**
- Resource-records delta: **exactly 3** (asserted `after - before == 3`)
- Persistent cache created at `~/.joey/delegation/result-cache.json`: **yes** (asserted `exists()`)
- Per-dispatch (from `--nocapture` output; second run, after the first run had already populated the result cache — so dispatches 1–2 were single-flight followers of the cache-hit leader and all three shared one wall clock):
  - dispatch 0: success, wall_clock=2.549s, iterations=1, model=glm-5.3
  - dispatch 1: success, wall_clock=2.549s, iterations=1, model=glm-5.3
  - dispatch 2: success, wall_clock=2.549s, iterations=1, model=glm-5.3
  - last-3 records: outcome=CacheHit compute_ms=0 (×3) — cache-hit records have zero-shape compute by contract (`admitted_at None`); the first run's completed/compute_ms values were not captured (first run was executed without `--nocapture`)

### M2 — `m2_governance_off_parity` (governance disabled, tempdir `data_dir`)

- 2 dispatches same goal shape → **2/2 success**
- Zero files in tempdir: asserted `!resource-records.jsonl.exists() && !result-cache.json.exists()` — passed
- Real-home record count unchanged: asserted `home_after == home_before` — passed
- Per-dispatch: dispatch 0 wall_clock=2.299s iterations=1 model=glm-5.3; dispatch 1 wall_clock=2.281s iterations=1 model=glm-5.3

## Verdict

**PASS** — M1: 3/3 success, records delta 3, cache created; M2: 2/2 success, zero tempdir files, home count unchanged. Test binary result line: `test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`.

Rerun command (from repo root, after `--no-run` build):

```
cp target/debug/deps/manual_m1m2-<hash> /tmp/t031b.bin
cd crates/joey-orchestration && perl -e 'alarm shift; exec @ARGV' 300 /tmp/t031b.bin --ignored --test-threads=1 --nocapture
```
