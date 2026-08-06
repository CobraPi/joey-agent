# Research: Windows Platform Support

**Feature**: 014-windows-platform-support
**Date**: 2026-08-05
**Spec**: `specs/014-windows-platform-support/spec.md`

---

## R1: Foreground streaming architecture on Windows

### Problem

`run_bash` (terminal_tool.rs:468–581) merges stdout+stderr into a single
pipe via `os_pipe`, then wraps the reader end's raw FD in
`tokio::io::unix::AsyncFd` for native async read-readiness. This is the
design landed in feature 009 (see `specs/009-terminal-async-perf/research.md`
R2) and it is **Unix-only**: `os_pipe` returns an FD, `AsyncFd` requires
`AsRawFd`, and the read loop calls `libc::read`. None of these exist on
Windows. The module-level `use std::os::unix::io::AsRawFd as _` (line 11)
fails before any function body is even type-checked.

### Decision

**Split the streaming layer into a platform trait behind a `#[cfg]` fork**,
keeping the upper layer (throttling, heartbeat, interrupt poll, temp-file
spill, output readback) shared and cross-platform.

Concretely:

1. Introduce a private enum `ReadChunk` / a small trait-shaped boundary
   that yields `Vec<u8>` chunks asynchronously until EOF. The shared
   `stream_output` body operates on this boundary, not on `AsyncFd`
   directly.

2. **Unix path (unchanged behavior, byte-for-byte):** keep the existing
   `os_pipe` + `AsyncFd` + `libc::read` implementation, gated behind
   `#[cfg(unix)]`. This preserves feature 009's perf characteristics
   exactly (Constitution Principle VII — Non-Regression).

3. **Windows path (new):** abandon `os_pipe` on Windows. Spawn the child
   with `Stdio::piped()` for stdout and stderr separately, take
   `child.stdout` and `child.stderr` (both `tokio::process::ChildStdout`
   / `ChildStderr`, which impl `AsyncRead` natively on Windows), and
   merge them into a single chunk stream via `tokio::select!`. This is
   the **same pattern the background reaper already uses successfully
   on both platforms** (feature 009, R3; `process_tool.rs::spawn_reaper`).

The merge order on Windows will not be byte-identical to Unix's
single-pipe merge (two pipes race in `select!`), but the spec's parity
requirement (FR-004, User Story 2 scenario 4) is that *both streams
appear in the single `output` field* — not byte-order fidelity. The
existing tests (`merged_stderr_and_exit_code`, terminal_tool.rs:897)
assert `contains("out") && contains("err")`, not ordering, so they pass
on both paths.

### Why this shape (not the alternatives)

- **Alternative A — make `os_pipe` cross-platform.** `os_pipe` *does*
  build on Windows, but its reader is a raw handle/FD that tokio cannot
  poll asynchronously without `AsyncFd` (Unix-only). On Windows you'd
  fall back to `spawn_blocking` on the reader, which feature 009 R2
  explicitly rejected because it stalls the turn-driving task. Rejected.

- **Alternative B — keep `os_pipe` on Windows + poll via
  `spawn_blocking`.** Rejected for the same reason 009 rejected it on
  Unix: regresses the async-streaming perf win that 009 delivered.

- **Alternative C — a single shared `Stdio::piped()` path on BOTH
  platforms.** Tempting (one code path), but it changes the Unix merge
  semantics from 009's single-pipe design, risking a regression in
  byte-order-dependent edge cases and violating Principle VII. The
  cfg-fork keeps Unix untouched. Rejected for v1; could be revisited
  later if the maintenance cost of two paths grows.

### Performance budget (Constitution Principle VIII)

- **Unix**: zero change. Same `AsyncFd` readiness loop, same 64 KB
  chunks, same 50ms throttle, same 4 KB temp-file threshold. No new
  allocations, no new syscalls.
- **Windows**: one extra `tokio::select!` branch over two `AsyncRead`
  streams vs. one. Cost: negligible (select! over two already-ready
  futures is constant overhead). Memory bound: unchanged (≤ 64 KB chunk
  buffer, same 4 KB spill threshold). Target: command output streams to
  the model with the same ≤ 50ms first-chunk latency as Unix.

No new dependency is introduced by this decision. `tokio` (already a
workspace dep) provides `ChildStdout`/`ChildStderr` async reads on
Windows natively.

---

## R2: Shell discovery — bash-first with PowerShell fallback

### Problem

`find_bash()` (terminal_tool.rs:47–62) has a Windows branch that probes
`which bash` and falls back to the literal `"bash"`. If Git Bash isn't
installed, every terminal call fails at spawn with an opaque OS error.
FR-011 (clarified) requires a PowerShell fallback so the agent works on
a bare Windows box.

### Decision

Generalize shell discovery into a `Shell` enum and a resolver:

```
enum Shell { Bash(String), PowerShell(String) }
```

Resolution order on Windows (FR-011):
1. `which bash` → `Shell::Bash(path)` (Git Bash if installed)
2. `which pwsh` → `Shell::PowerShell(path)` (PowerShell 7+)
3. `which powershell` → `Shell::PowerShell(path)` (built-in Windows PowerShell)
4. None found → return `Err` listing all three names; terminal tool
   surfaces it as a spawn-failure message (no panic).

On Unix: `find_bash()` logic unchanged → always `Shell::Bash`.

Cache the resolved `Shell` per-process in a `once_cell::sync::Lazy<Mutex<Option<Shell>>>`
(FR-013) so a session never flips between bash and PowerShell. First
terminal call resolves; subsequent calls reuse.

### Wrapper-script generation per shell

The wrapper script (terminal_tool.rs:476–480) currently appends a
`$?`-capture + `printf "$PWD"` CWD marker. This must be generated per
shell dialect (FR-012):

- **Bash** (unchanged):
  ```sh
  <command>
  __JOEY_STATUS=$?
  printf '\n{m}%s{m}' "$PWD"
  exit $__JOEY_STATUS
  ```

- **PowerShell** (new):
  ```powershell
  <command>
  $code = $LASTEXITCODE
  if ($code -eq $null) { $code = if ($?) { 0 } else { 1 } }
  Write-Output "`n{m}$PWD{m}"
  exit $code
  ```

PowerShell invocation: `pwsh -NoProfile -Command <script>` (or
`powershell -NoProfile -Command`). `-NoProfile` avoids the multi-second
profile-load penalty on every call (perf budget: profile load adds
~300–800ms on typical Windows boxes; `-NoProfile` keeps cold-start
≤ 50ms like bash).

`CWD_MARKER` (`__JOEY_CWD_MARKER__`) and `extract_cwd_marker` are
shared verbatim — the parser only cares about the marker framing, not
the shell that produced it.

### Exit-code semantics on PowerShell

- Native executables (git, cargo, etc.) set `$LASTEXITCODE`.
- Cmdlets set `$?` (`True`/`False`) and leave `$LASTEXITCODE` at its
  prior value (or `$null` if no native exe ran first).
- The wrapper normalizes: `$LASTEXITCODE` if set, else `$? ? 0 : 1`.
  Then `exit $code` makes it the process exit code, which
  `exit_code_from_status` (already `#[cfg(not(unix))]`-gated,
  terminal_tool.rs:618–621) reads via `ExitStatus::code()`.

### Alternatives considered

- **cmd.exe as a second fallback tier:** Rejected per clarification
  (2026-08-05) — weak CWD (`%CD%`) and exit-code handling, not worth a
  third wrapper dialect.
- **Always use PowerShell on Windows (drop bash):** Rejected — breaks
  parity for the common case where the user *has* Git Bash and expects
  POSIX command behavior (the agent's system prompt and tests assume
  bash semantics like `echo >&2`, `$?`, `printf`).
- **Per-call shell selection via a tool parameter:** Out of scope; the
  spec wants transparent fallback, not user-facing configuration.

---

## R3: Secondary Unix-only code paths (P4 hardening, not P1 blockers)

### Audit result

A workspace-wide grep for `std::os::unix`, `libc::`, `tokio::io::unix`,
and Unix path literals (`/tmp/`, `/dev/null`, `/bin/`) found **22 files**.
Of these:

- **Already cfg-guarded (no action):** `joey-core` (config, constants,
  utils, auth_store, lib, logging), `joey-mcp` (config, lib kill),
  `joey-cron` (jobs), `joey-cli` (doctor_cmd, main SIGPIPE,
  secret_prompt), `joey-tools` (memory_tool flock, process_tool exit
  status, vcs perms). ~15 files.

- **The sole compile blocker:** `joey-tools/src/tools/terminal_tool.rs`
  (R1 above). 7 unguarded Unix-only symbols.

- **Cosmetic / test-only (do not block build):** the `/tmp/` and
  `/dev/null` hits in `verification.rs`, `guardrails.rs`, `prompt.rs`,
  `isolation.rs`, `joey-tui/tests/smoke.rs` are inside `#[cfg(test)]`
  blocks that use them as *logical* path strings (never touching the
  real FS, or only on Unix). One test (`terminal_tool.rs:921`,
  `grep zz /dev/null`) assumes a Unix `/dev/null`; it must be gated
  `#[cfg(unix)]` or rewritten to a portable equivalent.

- **Production security data (intentional, keep):** `joey-tools/src/guards.rs`
  Unix device paths (`/dev/zero`, `/dev/null`...) and sensitive-path
  prefixes (`/etc/`, `/boot/`) are *threat-model data*, not platform
  assumptions — they should match Unix attacker strings even when joey
  runs on Windows. No change.

- **Production path candidates (narrow, defer to P4):**
  `joey-providers::copilot` hardcodes `/opt/homebrew/bin/gh`,
  `/usr/local/bin/gh` as `gh` CLI search candidates — harmless on Windows
  (the paths just don't exist; `which gh` is the primary probe).
  `joey-mcp::config` stdio command resolution assumes `/bin/sh`-style
  paths in test fixtures. `joey-cli::project_trust` has Unix path
  handling in trust-store logic. **None block compilation.** They are
  noted for P4 hardening (User Story 4 is the right place; they don't
  belong in P1–P3).

### Decision

P1–P3 scope = R1 (streaming) + R2 (shell discovery) + the one test gate
(`grep zz /dev/null`). Everything else is P4 and tracked as a follow-up
in tasks.md, not a blocker.

---

## R4: Test-gating strategy

### Decision

Two categories of test changes, both minimal:

1. **`grep zz /dev/null` (terminal_tool.rs:921):** gate the whole test
   `#[cfg(unix)]` — it asserts bash/Unix exit-code semantics (`grep`
   exit 1 = "no matches") against a Unix device. Add a Windows analogue
   that asserts the same exit-code-meaning table logic via a portable
   command, OR simply rely on the unit test for `interpret_exit_code`
   (which is pure string logic and already platform-agnostic). Prefer
   the latter: keep the integration test Unix-only, keep the pure-logic
   unit test cross-platform.

2. **Cross-platform regression tests (new, for Principle VII):**
   - A unit test asserting `find_shell()` / `Shell` resolution returns
     `Bash` on Unix (existing behavior) and does not panic on Windows.
   - A unit test asserting the PowerShell wrapper script contains the
     `$LASTEXITCODE` / `$PWD` / CWD-marker framing (string assertion,
     no subprocess needed — runs on all platforms).
   - A unit test asserting `extract_cwd_marker` parses a PowerShell-
     produced marker identically to a bash-produced one.

### Alternatives considered

- **Run the full integration suite on Windows via CI:** ideal but out
  of scope for this feature (no CI config exists in-repo per AGENTS.md).
  The manual quickstart (quickstart.md) is the validation gate.
- **Skip all terminal tests on Windows:** rejected — leaves the
  PowerShell path untested. The wrapper-string unit test covers the
  contract without needing a subprocess.

---

## R5: Dependencies — weight analysis (Principle VIII)

### No new dependencies required

| Need | Candidate | Verdict |
|------|-----------|---------|
| Async read on Windows child pipes | `tokio::process::ChildStdout` (already a transitive dep via tokio) | **Use it** — zero new weight |
| PowerShell detection | `which` crate (already a joey-tools dep) | **Reuse** — zero new weight |
| Cross-platform temp dir | `tempfile::NamedTempFile` (already used) | **Reuse** — zero new weight |
| PTY on Windows | `portable-pty` (declared, unused) | **No change** — PTY stays a stub (out of scope per spec) |

**Conclusion:** this feature adds **zero new dependencies**. It only
rearranges existing ones behind cfg gates. Compile-time and binary-size
impact: negligible (a few hundred lines of cfg-gated code, no new crate
graphs). This satisfies Principle VIII trivially — there is no new
weight to justify against alternatives.

### `os_pipe` on Windows

`os_pipe` remains a dependency (used on the Unix path). On Windows it is
simply never called — the Windows path uses `Stdio::piped()` instead.
No need to make `os_pipe` optional behind a target-specific feature; the
dead-code elimination handles it. (Confirmed: `os_pipe` builds clean on
Windows; it's only the *usage* of its FD via `AsyncFd` that breaks.)

---

## Performance-sensitive paths identified

| Path | Platform | Budget |
|------|----------|--------|
| First output chunk → progress event | Unix | ≤ 50ms (unchanged from 009) |
| First output chunk → progress event | Windows | ≤ 50ms (target parity with Unix) |
| Shell cold-start (PowerShell) | Windows | ≤ 50ms via `-NoProfile` (vs ~300–800ms with profile) |
| Shell cold-start (bash) | Windows | unchanged (existing `find_bash` behavior) |
| Chunk coalescing throttle | both | 50ms window (unchanged) |
| Silent-command heartbeat | both | 2s interval (unchanged) |
| Interrupt poll cadence | both | 100ms (unchanged) |
| Memory bound per command | both | ≤ 64KB chunk buffer + temp-file spill at 4KB (unchanged) |
