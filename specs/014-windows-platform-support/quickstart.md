# Quickstart: Windows Platform Support

**Feature**: 014-windows-platform-support
**Date**: 2026-08-05

End-to-end validation procedure for this feature. Run on **both** Windows
and Unix; each platform has its own section. This is the smoke test the
implementation must pass before the feature is considered complete.

---

## Prerequisites

**Windows machine** (Windows 10/11, x86_64-pc-windows-msvc):
- Rust stable toolchain (`rustup default stable`)
- The repo checked out
- Optionally: Git Bash installed (to exercise the bash-first path)
- Optionally: PowerShell available (always present on modern Windows —
  exercises the fallback path when bash is absent)

**Unix machine** (Linux or macOS):
- Rust stable toolchain
- The repo checked out
- bash (always present)

---

## Phase 1 — Build validation (P1, User Story 1)

### On Windows

```powershell
cd D:\Development\joey-agent
cargo build --workspace
```

**Expected**: exit code 0, no compile errors, `target\debug\joey.exe` exists.

**Verify**:
```powershell
cargo build -p joey-tools          # the previously-failing crate
.\target\debug\joey.exe --version  # should print version and exit 0
.\target\debug\joey.exe --help     # should print help and exit 0
```

**Baseline (before the fix)**: `cargo build --workspace` exits 101 with
7 errors in `terminal_tool.rs` (E0433: cannot find `unix` in `os`/`io`).
This is the regression you must NOT reintroduce.

### On Unix (regression gate — must not break)

```bash
cd ~/path/to/joey-agent
cargo build --workspace
```

**Expected**: exit code 0, identical to the pre-feature baseline.

---

## Phase 2 — Terminal tool foreground execution (P2, User Story 2)

### On Windows, with Git Bash installed (bash-first path)

Start the joey REPL (or use a one-shot agent invocation) and run terminal
commands through the tool:

```
joey
> run `echo hello` in the terminal
```

**Expected** in the tool result JSON:
- `"output"` contains `"hello"`
- `"exit_code"` is `0`
- `"error"` is `null`

Then:
```
> run `cargo --version`
> run `echo out; echo err >&2; exit 3`     # merged streams + exit code
> run `cd` to a subdir, then `pwd`          # cwd persistence
```

**Expected**: cargo version captured; both "out" and "err" in merged
output, exit_code 3; cwd persists across the `cd` + `pwd` calls.

### On Windows, with NO Git Bash (PowerShell fallback path)

Remove bash from PATH (or use a fresh Windows install without Git Bash),
keep PowerShell. Repeat the `echo hello` test.

**Expected**:
- The PowerShell fallback is selected (no panic, no "bash not found"
  error).
- `"output"` contains `"hello"`, `"exit_code"` 0.
- A subsequent `pwd` / `Get-Location` reflects correct cwd (CWD marker
  parsed from PowerShell output).

**Verify shell resolution did not flip**: run 3 terminal commands in a
row; confirm they all use the same shell (check via debug logging or
`joey doctor` if it surfaces shell choice — otherwise trust the cache).

### On Windows, with NO bash AND NO PowerShell

Remove both from PATH. Run a terminal command.

**Expected**: a clear error message naming `bash, pwsh, powershell`,
`exit_code: -1`, `status: "error"`. **No panic, no crash.**

### On Unix (regression gate)

Run the same `echo hello` / merged-stderr / cwd-persistence tests.

**Expected**: identical behavior to the pre-feature baseline (the Unix
code path is unchanged behind its cfg gate).

---

## Phase 3 — Background processes (P3, User Story 3)

### On Windows

```
> start a background process: sleep 30 (or timeout 30 on cmd)
  -> expect a session_id back, status "background"
> poll it immediately
  -> expect current output (maybe empty) and status still running
> kill it
  -> expect status reflects terminated
> start another that exits on its own (e.g. echo done), wait, poll
  -> expect exit_code 0 and "done" in output
```

**Expected**: ProcessRegistry lifecycle (spawn → poll → kill / exit)
works end-to-end. (This path already uses portable `Stdio::piped()`; it
should work once P1 unblocks compilation. If it fails, it's a P3 bug.)

---

## Phase 4 — Test suite (P4, User Story 4)

### On Windows

```powershell
cargo test --workspace
```

**Expected**: completes without compile errors in test code. Any
Unix-only test must be `#[cfg(unix)]`-gated and thus skipped, not
failing. Specifically verify:
- `terminal_tool::tests::exit_code_meaning_table` — if it relies on
  `/dev/null`, it must be gated OR the pure-logic `interpret_exit_code`
  unit test must cover the table on Windows.
- No test panics on `std::os::unix::fs::*` calls.

### On Unix (regression gate)

```bash
cargo test --workspace
```

**Expected**: all previously-passing tests still pass. Zero regressions
from the added cfg gates.

---

## Success criteria mapping

| SC | Validated by |
|----|--------------|
| SC-001 (build on Win) | Phase 1 Windows |
| SC-002 (build on Unix) | Phase 1 Unix |
| SC-003 (terminal works, both shells) | Phase 2 bash + PS fallback |
| SC-004 (tests green on Win) | Phase 4 Windows |
| SC-005 (no Unix symbol in non-cfg(unix) path) | grep audit — see below |
| SC-006 (background lifecycle on Win) | Phase 3 |

### SC-005 grep audit (run after implementation)

```bash
# From repo root. Should return ZERO hits outside #[cfg(unix)] blocks.
grep -rn "std::os::unix\|tokio::io::unix" crates/joey-tools/src/tools/terminal_tool.rs
```

The only acceptable hits are inside `#[cfg(unix)]` function bodies or
`use` statements that are themselves cfg-gated. Any hit in a context
compiled on Windows = a bug.
