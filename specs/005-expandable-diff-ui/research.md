# Research: Expandable Diffs, Thinking & Tool Calls (TUI + CLI)

**Feature**: `specs/005-expandable-diff-ui` | **Date**: 2026-07-25

This document resolves the Phase 0 unknowns flagged in `plan.md`'s
Technical Context and Constitution Check (Principles VII and VIII). It is
organized as a sequence of decisions, each with rationale and alternatives
considered.

---

## Decision 1: Syntax-highlighting engine for diff code lines

**Context**: Clarification Q5 chose to ship syntax highlighting in v1.
FR-003 (strengthened) requires per-language syntax highlighting of the code
content of each diff line, in addition to add/remove/context coloring.
This is a new dependency on a hot path (streaming diff rendering), so
constitution Principle VIII mandates a recorded cost/benefit analysis.

### Options considered

| Option | Binary-size cost | Compile-time cost | Coverage | Verdict |
|--------|------------------|-------------------|----------|---------|
| **`syntect` 5.x** (Sublime Text grammars) | ~2–4 MB (with default grammar/theme sets; can be trimmed by selecting only the grammars we ship) | moderate (proc-macro grammar compilation, one-time) | 100+ languages via `.sublime-syntax`; mature; used by `bat` | **Chosen** |
| `bat` (the binary as a library) | larger surface; pulls `clap`/`git2` transitively | heavier | excellent (wraps `syntect`) | Rejected: it's a binary-first crate; we only need the highlighting, and we already have `syntect` as a lighter, more focused dependency. |
| tree-sitter family (`tree-sitter` + per-language crates) | grammar .so/.wasm per language; C compilation | heavy (each grammar is a build step) | excellent, structural | Rejected: over-engineered for line-level coloring; the per-language crate plumbing is far more invasive than `syntect`'s single dependency, and the build cost violates Principle VIII's lean constraint for marginal visual gain over grammar-based highlighting. |
| Hand-rolled regex highlighter per language | minimal (a few KB) | negligible | poor (regex highlighting misses multi-line constructs, strings, etc.) | Rejected: coverage too weak to satisfy "syntax highlighting" in good faith; would produce visibly wrong results that look worse than the plain fallback. |
| No highlighting (defer) | 0 | 0 | 0 | Rejected: explicitly overridden by Clarification Q5. |

### Decision

**Use `syntect = "5"`** as a new workspace dependency, declared in
`joey-tools` and exposed via a new `joey-tools/src/highlight.rs` helper
module. The helper is *invoked* from the render layer
(`joey-cli/src/render.rs` and `joey-tui/src/widgets.rs`) but *lives* in
`joey-tools` because that is the only DAG-valid shared ancestor of both
render crates (neither depends on the other). See the C1 resolution in
`plan.md` (Structure Decision).

### Rationale

- `syntect` is the de-facto Rust syntax highlighter (used by `bat`,
  `gitui`, `zed`'s early highlighter). It is mature, well-maintained, and
  carries no surprising transitive surface beyond `regex`/`fancy-regex`
  (both already in the workspace's transitive closure) and `plist`.
- It is the closest Rust analog to crush's `chroma` (which crush uses for
  the same purpose), preserving behavioral parity with the reference UI.
- Its default theme/grammar bundle can be trimmed at build time to control
  binary size; the plan tasks will ship a curated subset (the languages
  joey already lints in `syntax_gate`: `.py`, `.json`, `.yaml/.yml`,
  `.toml`, plus `.rs`, `.go`, `.js/.ts`, `.md`, `.sh`) rather than the full
  100+ set.

### Cost / benefit (Principle VIII)

- **Binary size**: `syntect` with a curated grammar subset adds an
  estimated **2–4 MB** to the release binary (current binary is already
  several MB; this is proportional). The full default set would add more;
  we trim. This is recorded here as the accepted cost.
- **Compile time**: one-time proc-macro grammar compilation; estimated
  < 10 s incremental, < 60 s clean. Acceptable.
- **Runtime**: naive per-line highlighting of a 200-line diff is the hot
  path. Mitigated by a **per-line syntax cache** keyed by
  `(content_hash, language)` — directly mirroring crush's
  `syntaxCache map[string]string` in `diffview/diffview.go`. First render
  pays the highlight cost; subsequent renders of the same line are a
  hashmap lookup. Target: **< 5 ms p95** for a 200-line block (warm cache).
- **Graceful degradation**: unrecognized languages fall back to plain
  add/remove/context coloring with no error (FR-003 assumption). The
  highlighter is wrapped so a panic/parse failure in a grammar never
  crashes the render path.

### Alternatives for cache keying (chosen: content hash)

- Per-(content, language) `HashMap<String, String>` — chosen. Matches
  crush exactly; collisions impossible (we hash the raw line content).
- Per-file cache — rejected; a diff shares lines across files, and
  per-file caches duplicate work for repeated edits to the same file.
- No cache (re-highlight every frame) — rejected; would blow the 12 fps
  streaming budget for large diffs.

---

## Decision 2: How file-change data reaches the renderer

**Context**: Clarification Q1 chose "both structured file-change tracking
AND diff-text detection." The existing `FileTracker` already records reads
and writes and can compute diffs, but its results are only surfaced today
via the deferred `/changes` slash command (REPL `repl.rs:1453`,
TUI `tui.rs:757`). Inline, per-tool rendering requires the diff to reach
the renderer **at the moment the tool completes**, attributed to that tool.

### Options considered

| Option | Mechanism | Parity | Attribution | Verdict |
|--------|-----------|--------|-------------|---------|
| **New `AgentEvent::FileChange` variant emitted by the tool layer** | `write_file`/`patch`/terminal build a `DiffResult` from the tracker and emit it as an event through the existing `AgentEvent` channel | ✅ both CLI and TUI consume the same stream | ✅ event is emitted inline with the tool call | **Chosen** |
| Renderer polls `FileTracker` each frame | TUI/CLI ask `diffs_for_all_modified()` on every render | ✅ same data | ❌ cannot tell which tool caused which change; multiple changes between frames collapse | Rejected: breaks per-tool attribution and single-stream parity. |
| Stuff diff text into `ToolEnd.result_preview` | overload the existing one-line preview | ✅ same stream | partial | Rejected: `result_preview` is a truncated one-liner by contract; overloading it breaks that contract and can't carry before/after content. |

### Decision

Add **`AgentEvent::FileChange { path, before, after, diff, added, removed, kind }`**
as a new additive variant, emitted by the file/terminal tools immediately
after `record_write`, using the existing `file_tracker::diff_for_file` to
compute the diff from the recorded baseline.

### Rationale

- The `AgentEvent` channel is already the single source consumed by both
  the CLI renderer (`render_turn`) and the TUI state machine (`App::apply`).
  Adding a variant keeps parity automatic (Principle II).
- It is **additive**: exhaustive `match` arms in consumers gain one new
  arm; no existing variant changes. This is non-breaking under Principle VII.
- Attribution is automatic: the event is emitted in the tool's execution
  path, so it lands in the stream adjacent to the corresponding
  `ToolStart`/`ToolEnd` pair.
- The existing `FileTracker` does all the heavy lifting (baseline
  snapshot + LCS diff + count). The new variant just carries its output.

### What `before`/`after` carry

- `before`: the baseline content from `FileTracker::get_original` (empty
  for new files, full prior content for deletions).
- `after`: the post-write on-disk content (read back after the write
  completes), so the diff reflects exactly what landed.
- `diff`: the unified-diff text (for the non-interactive CLI plain-text
  path, FR-012) and the structured line list (for the colored render path).
- `kind`: `Create` | `Edit` | `Delete` (for FR-004 labels).

---

## Decision 3: Terminal-mutation detection (FR-017)

**Context**: Clarification Q3 chose to cover terminal commands that mutate
files (e.g. `sed -i`, `>file`, `mv`), in addition to `write_file`/`patch`.

### Options considered

| Option | Mechanism | Accuracy | Cost | Verdict |
|--------|-----------|----------|------|---------|
| **Snapshot-diff known file set** | Before running a terminal command, snapshot mtime+hash of files read this session; after, re-snapshot and emit `FileChange` for any that changed | high (content-true) | O(files-read-in-session) per command | **Chosen** |
| Command-pattern detection (regex for `sed -i`, `>`, etc.) | Parse the command string for mutation patterns | low (misses `tee`, `perl -i`, pipes, heredocs) | O(1) per command | Rejected: too many false negatives; the user's expectation (Q3) is "anything that changed a file." |
| `inotify`/`kqueue` filesystem watcher | OS-level file watcher on the cwd | highest | heavy: new thread, cross-platform complexity, resource cost | Rejected: violates Principle VIII's lean constraint for a local single-user agent. |

### Decision

**Snapshot-diff the known file set.** The terminal tool already has access
to `FileTracker::read_files()`. Before executing a foreground command,
snapshot `{mtime, sha256}` for every read-tracked file; after the command
returns, re-snapshot and, for any file whose mtime or hash changed, emit
a `FileChange` event (using the recorded baseline as `before` and the new
on-disk content as `after`).

### Rationale

- Reuses the existing baseline data (`FileTracker::read_files()` /
  `get_original()`), so no new tracking store.
- Cost is bounded to files actually read in-session (Clarification Q2's
  scope), keeping it proportional to read volume (Principle VIII).
- Catches `sed -i`, `tee`, `>`, `mv`, `perl -i`, and anything else that
  actually writes — which is exactly the user's stated expectation.
- Hash check guards against mtime-only changes (e.g. `touch`) producing
  spurious diffs.

### Edge case: files changed by terminal that were never read

If a terminal command edits a file the agent never read in-session, there
is no baseline to diff against. Per Clarification Q2's scope (baselines are
read-bounded), such a change is reported as a **Create** (full content as
additions) rather than skipped — the user still sees that the file changed.
This is documented in `data-model.md`.

---

## Decision 4: Expand/collapse state model (FR-006…011, FR-018)

**Context**: Clarification Q4 chose per-item expand/collapse (no global
toggle), matching crush.

### Decision

Adopt crush's proven model directly, adapted to Rust:

- A three-state machine for long content (`thinkingViewMode` in crush's
  `assistant.go`): **Collapsed → TailWindow → FullExpanded**, cycling on
  activation. Short content (fits within `maxCollapsedThinkingHeight = 10`
  in crush) toggles in two states: **Collapsed ↔ FullExpanded**, skipping
  the tail-window step.
- A two-state machine for tool calls: **Collapsed ↔ Expanded** (crush's
  `Expandable.ToggleExpanded`).
- State lives **on the transcript item**, not in a global registry. This
  matches joey's existing `TranscriptItem` enum in `joey-tui/src/state.rs`
  and the REPL's transcript handling.
- In the **non-interactive CLI**, there is no activation: every state
  resolves to "fully shown" (FR-012), so a `--quiet`/piped run emits
  reasoning and tool output in full plain text.

### Rationale

- Per-item state is the simplest model that satisfies "per-item
  expand/collapse" (Q4) and keeps focus/keybinding semantics local to the
  item (Principle VI: no global state threaded through shared paths).
- The three-state cycle is crush's exact behavior; copying it gives
  behavioral parity with the reference ("just like the crush UI") without
  inventing a new model.
- Putting state on the transcript item means a render is a pure function
  of `(item, width)`, which keeps the TUI's per-frame render cacheable
  (crush's `cachedMessageItem` pattern, already echoed in joey's
  `TranscriptItem`).

### Constants (ported from crush, to be finalized in tasks.md)

- `maxCollapsedThinkingHeight = 10` (lines shown before truncating in
  collapsed view).
- `maxExpandedThinkingTailLines = 200` (tail-window cap before promoting
  to full expansion).
- `responseContextHeight` for tool-result truncation (crush's value;
  carried over).

---

## Open items deferred to tasks.md (implementation phase)

- Exact set of `syntect` grammars to ship (curated subset vs. full).
- The config key / feature flag name for disabling syntax highlighting
  (escape hatch if the dep proves too heavy in practice).
- Whether the `FileChange` event is emitted before or after the
  corresponding `ToolEnd` (ordering detail; either is consistent as long
  as it's documented).
- Mouse-click hit-testing regions for expand/collapse in the TUI (crush
  uses click on the item; joey-tui's mouse model to be confirmed in
  tasks).
