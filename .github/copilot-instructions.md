# Joey Agent repo guidance

- Rust workspace under `crates/` — the dependency graph is a strict DAG; lower crates never depend on higher ones.
- Acceptance bar: `cargo build --workspace` + `cargo test --workspace` must stay green.
- Read `PORTING.md` and `docs/README.md` before any non-trivial change; subsystem docs in `docs/` are the primary source of truth.
- Guidance strings shown to the model are ported **verbatim** from Python upstream — do not reword them.
- On-disk formats are Hermes-compatible (SQLite schema, `jobs.json`, `SKILL.md`); never bump schema/format versions casually.
