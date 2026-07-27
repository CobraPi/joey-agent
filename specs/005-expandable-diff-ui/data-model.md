# Data Model: Expandable Diffs, Thinking & Tool Calls (TUI + CLI)

**Feature**: `specs/005-expandable-diff-ui` | **Date**: 2026-07-25

This document defines the entities, fields, relationships, validation
rules, and state transitions introduced or extended by this feature. It
draws the entities from the spec's Key Entities section and grounds them in
the existing code.

---

## Entity: Tracked File Read *(existing — extended)*

A record that the agent read a specific file at a specific point in a
session, establishing the baseline ("before") content used to compute the
diff when that file is later edited.

**Lives in**: `joey-tools/src/file_tracker.rs` (already implemented).

| Field | Type | Notes |
|-------|------|-------|
| path | `String` (normalized) | Normalized via `normalize_path` (tilde-expanded, cwd-relative where possible). |
| original_content | `String` | Snapshot at first read (`originals` HashMap entry). Set once via `or_insert_with`. |
| read_time | `SystemTime` | `read_times` HashMap entry. |

**Scope**: per session, in-memory only (Clarification Q2). The process-global
`Lazy<Mutex<FileTracker>>` holds the store; `reset()` clears it on new
session. Not persisted across sessions.

**Validation rules**:
- `original_content` is set **only on first read** of a path
  (`entry().or_insert_with()`); subsequent reads do not overwrite it (so
  the baseline is always the agent's first view of the file).
- Path normalization is idempotent: the same physical path always maps to
  the same key regardless of `~/`, absolute, or cwd-relative spelling.

**No change required** to this entity for the feature; it already does what
the spec needs.

---

## Entity: File Change Event *(new)*

A structured change notification emitted when the agent creates, edits, or
deletes a file, carrying the before/after content and a rendered diff.

**Lives in**: `joey-agent-core/src/events.rs` (new `AgentEvent` variant).
**Produced by**: `joey-tools/src/tools/file_tools.rs` (`write_file`,
`patch`), `joey-tools/src/tools/terminal_tool.rs` (snapshot-diff).
**Consumed by**: `joey-cli/src/render.rs`, `joey-tui/src/state.rs`.

| Field | Type | Notes |
|-------|------|-------|
| path | `String` | Normalized path (display form). |
| kind | `enum FileChangeKind { Create, Edit, Delete }` | Drives the label per FR-004. |
| before | `String` | Baseline content (empty for `Create`, full prior content for `Delete`). |
| after | `String` | Post-write on-disk content (empty for `Delete`). |
| diff | `DiffResult` | The structured diff (`{ path, diff, added, removed }`) — reuses the existing `file_tracker::DiffResult`. |
| is_binary | `bool` | True when before/after could not be decoded as UTF-8 (FR-016). When true, `diff` is empty and the renderer shows the binary placeholder. |
| source | `enum FileChangeSource { FileTool, Terminal, Detected }` | Whether this came from an explicit file tool, a terminal snapshot-diff, or diff-text detection in a tool result (FR-005). |

**Validation rules**:
- `kind == Create` ⇒ `before.is_empty()`.
- `kind == Delete` ⇒ `after.is_empty()`.
- `is_binary == true` ⇒ `diff.diff.is_empty()` (no textual diff for binary).
- `added`/`removed` counts are consistent with `diff` line markers
  (`+`/`-`) — the existing `generate_diff` guarantees this.
- A change with `added == 0 && removed == 0` is **not emitted** (no-op
  write; the existing `diff_for_file` already returns `None` in this case).

**Relationships**:
- Derived from a **Tracked File Read** (the `before`) and the on-disk
  post-write content (the `after`).
- Produced 1:1 with a mutating tool execution (a single `write_file` call
  that touches one file emits one event; a `patch` touching N files emits
  N events).

---

## Entity: Rendered Diff Block *(new render model)*

The visual representation of one file's net change within a turn, produced
from a File Change Event (or from detected diff text).

**Lives in**: render layer (`joey-cli/src/render.rs`, `joey-tui/src/widgets.rs`).

| Field | Type | Notes |
|-------|------|-------|
| path | `String` | Display path. |
| kind | `FileChangeKind` | For the new-file/deleted-file label. |
| added | `usize` | Add count for the header stat line. |
| removed | `usize` | Remove count for the header stat line. |
| lines | `Vec<DiffLine>` | The ordered, rendered lines (see below). |
| truncated | `bool` | True if the block was height-bounded. |
| hidden_lines | `usize` | Count of lines hidden by truncation (for the affordance). |

**DiffLine** (the per-line render unit):

| Field | Type | Notes |
|-------|------|-------|
| kind | `enum { Context, Add, Remove }` | Drives `+`/`-`/` ` prefix and color. |
| text | `String` | The raw code text of the line (pre-highlighting). |
| highlighted | `Option<String>` | Syntax-highlighted ANSI form, populated lazily by the highlight cache. `None` = not yet highlighted or unrecognized language (falls back to plain coloring). |

**Validation rules**:
- The count of `Add` lines in `lines` equals `added`; `Remove` equals
  `removed` (consistency with the `DiffResult`).
- `truncated == true` ⇒ `hidden_lines > 0`.

---

## Entity: Expandable Section *(new state)*

A transcript element (reasoning block or tool call) carrying a discrete
expand/collapse state.

**Lives in**: on the transcript items in `joey-tui/src/state.rs`
(`TranscriptItem::Reasoning`, `TranscriptItem::Tool`) and the REPL's
transcript handling in `joey-cli/src/repl.rs`.

### Reasoning section — three-state machine

States (ported from crush's `thinkingViewMode`):

```
            activate                activate
  Collapsed ─────────► TailWindow ─────────► FullExpanded
      ▲                                           │
      └───────────────── activate ────────────────┘
                          (cycles back)
```

| State | `maxCollapsedHeight` | Behavior |
|-------|----------------------|----------|
| Collapsed | 10 lines (port: `maxCollapsedThinkingHeight`) | Show ≤ N trailing lines + "… (M lines hidden) [expand]" affordance. |
| TailWindow | 200 lines (port: `maxExpandedThinkingTailLines`) | Show last N lines + "… M earlier lines hidden [full view]" affordance. Only entered if content exceeds the collapsed cap; otherwise Collapsed↔FullExpanded is a direct toggle. |
| FullExpanded | ∞ | Show full content. |

**Transition rule**: an activation advances `Collapsed → TailWindow →
FullExpanded → Collapsed`. If the rendered line count ≤ collapsed cap, the
TailWindow state is skipped (Collapsed ↔ FullExpanded direct).

### Tool call — two-state machine

```
        activate           activate
  Collapsed ──────► Expanded ──────► Collapsed
```

| State | Behavior |
|-------|----------|
| Collapsed | One-line summary: tool name, status, short description. |
| Expanded | Full arguments + result. If the tool produced a FileChange, the Rendered Diff Block is shown inside. |

### Non-interactive resolution

In a non-interactive CLI context (`RenderCapability::NonInteractive`,
`--quiet`, or piped output), **both** state machines resolve to "fully
shown" on first render — i.e. reasoning is emitted in full and tool results
are emitted in full (FR-012). There is no activation; no content is hidden.

---

## Lifecycle: a single file edit through the system

This traces how the entities interact for one `write_file` call that edits
a previously-read file:

```
1. read_file(path)        ──► FileTracker.record_read(path, content)
                                   originals[path] = content (first time)

2. write_file(path, new)  ──► [write to disk]
                             ──► FileTracker.record_write(path)
                             ──► before = FileTracker.get_original(path)
                                  after  = read(path) from disk
                                  diff   = file_tracker::diff_for_file(path, after)
                             ──► emit AgentEvent::FileChange {
                                     path, kind: Edit, before, after, diff,
                                     is_binary, source: FileTool
                                   }

3. render_turn / App::apply
                          ──► on AgentEvent::FileChange:
                                  build RenderedDiffBlock
                                  (highlight lines via syntect cache, or fallback)
                                  if interactive & collapsed-target: truncate + affordance
                                  print/draw the block
```

For a terminal mutation (`sed -i`), step 2 is replaced by the snapshot-diff:
before the command, snapshot `{mtime, hash}` of read-tracked files; after,
re-snapshot; for changed files, emit `FileChange { source: Terminal, ... }`.

---

## Summary of new vs. existing data

| Entity | Status | Where |
|--------|--------|-------|
| Tracked File Read | existing, unchanged | `joey-tools/file_tracker.rs` |
| File Change Event | **new** (`AgentEvent` variant) | `joey-agent-core/events.rs` |
| Rendered Diff Block | **new** (render model) | `joey-cli/render.rs`, `joey-tui/widgets.rs` |
| Expandable Section (reasoning) | **new** (state on `TranscriptItem`) | `joey-tui/state.rs`, `joey-cli/repl.rs` |
| Expandable Section (tool call) | **new** (state on `TranscriptItem`) | `joey-tui/state.rs`, `joey-cli/repl.rs` |
| `DiffResult`, `DiffSignal`, `generate_diff`, `is_unified_diff` | existing, reused | `joey-tools/file_tracker.rs` |
| `FileChangeKind`, `FileChangeSource`, `DiffLine` | **new** (supporting enums) | alongside their primary entity |
