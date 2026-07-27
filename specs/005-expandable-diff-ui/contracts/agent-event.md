# Contract: `AgentEvent::FileChange`

**Feature**: `specs/005-expandable-diff-ui` | **Crate**: `joey-agent-core`

This is the wire/event contract for the new additive `AgentEvent` variant
that carries a file change to the render layer. It is the single source
consumed by both the CLI renderer and the TUI state machine (constitution
Principle II: CLI/TUI parity).

## Contract

### Variant (added to the existing `pub enum AgentEvent`)

```rust
/// A file was created, edited, or deleted by the agent during a turn.
/// Emitted inline with the tool execution that caused the change, so the
/// renderer can draw an inline diff attributed to that tool call.
/// Additive variant: existing exhaustive matches gain one arm.
FileChange {
    /// Display-normalized file path.
    path: String,
    /// What kind of change this is (drives the new-file / deleted-file label).
    kind: FileChangeKind,
    /// Baseline content (from FileTracker::get_original). Empty for Create.
    before: String,
    /// Post-write on-disk content. Empty for Delete.
    after: String,
    /// The computed unified diff + counts. Reuses file_tracker::DiffResult.
    /// Empty `.diff` when `is_binary` is true.
    diff: joey_tools::file_tracker::DiffResult,
    /// True when before/after could not be decoded as UTF-8. When true the
    /// renderer MUST show a "binary file changed" placeholder (FR-016) and
    /// MUST NOT attempt to render `.diff`.
    is_binary: bool,
    /// What produced this event: an explicit file tool, a terminal snapshot,
    /// or diff-text detection in a tool result (FR-005).
    source: FileChangeSource,
},
```

### Supporting enums

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeKind {
    Create,
    Edit,
    Delete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeSource {
    /// write_file / patch.
    FileTool,
    /// A terminal command mutated the file (snapshot-diff detected).
    Terminal,
    /// Diff text detected in a tool result (FR-005). In this case `before`
    /// is empty and `after` holds the detected diff text; `kind` is Edit.
    Detected,
}
```

## Ordering guarantees

- `FileChange` is emitted by the tool execution path, immediately after the
  write lands on disk and `FileTracker::record_write` has run.
- Relative to the bracketing tool events for the **same** tool call, the
  ordering is: `ToolStart` → (`FileChange`)* → `ToolEnd`. The renderer
  treats `FileChange` as belonging to the most-recent `ToolStart` not yet
  closed by a `ToolEnd`.
- For terminal-mutation detection, all `FileChange` events for a single
  command are emitted together after the command returns, before `ToolEnd`.
- A `FileChange` is never emitted for a no-op write (`added == 0 &&
  removed == 0`); the producing layer filters these out (the existing
  `diff_for_file` already returns `None`).

## Backward compatibility (constitution Principle VII)

- **Additive only.** The `AgentEvent` enum gains one variant. No existing
  variant's fields or ordering change.
- Exhaustive `match` expressions on `AgentEvent` in consumers
  (`render_turn`, `App::apply`, gateway forwarders) gain one new arm. The
  arm is required at compile time, which is the intended regression signal:
  every surface that renders the event stream is forced to acknowledge the
  new event.
- The variant references `joey_tools::file_tracker::DiffResult`, which is
  already a public type — no new public surface in `joey-tools`.
- No CLI flag, config key, exit code, or on-disk format changes. No SQLite
  schema bump.

## Producer contract (who may emit)

Only the tool execution layer in `joey-tools` (`file_tools.rs`,
`terminal_tool.rs`) emits `FileChange`, via the agent-core event channel.
Application-level code, gateway forwarders, and the render layer MUST NOT
synthesize `FileChange` events; they only consume them. This keeps the
tracker (in `joey-tools`) the single producer of file-change truth
(Principle VI: narrow producer surface).

## Non-interactive resolution

When the CLI renderer is non-interactive (`RenderCapability::NonInteractive`,
`--quiet`, or piped stdout), it renders `FileChange` as a plain-text
unified diff to stdout, with no color and no truncation (FR-012). The full
diff text is available in `diff.diff`. No content is hidden behind an
interaction.
