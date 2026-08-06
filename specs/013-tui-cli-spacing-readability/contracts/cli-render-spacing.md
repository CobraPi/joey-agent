# Contract: CLI `render_turn` Inter-Element Spacing

**Feature**: `013-tui-cli-spacing-readability` | **Crate**: `joey-cli`
**Function**: `render_turn(...)` (render.rs:366+) — the streaming CLI
transcript renderer.

This is the presentation contract for uniform vertical rhythm in the CLI
streaming transcript. It is additive to the spec 008 block-layout contract;
the crush block structures (`┌─ Reasoning`/`└─ Thought for Ns`, `$ command`
headers, icon+emoji+name tool headers, `(exit N)` badges) are unchanged.

## §1 — The `pending_separator` flag (FR-009/015, Clarification Q1)

A single local flag drives all inter-element spacing:

```rust
// True when a previous element rendered and the next renderable element
// must be preceded by exactly one blank line. Drained (one println!())
// before the next element's first line; set true after an element renders.
let mut pending_separator: bool = false;
```

**Drain helper** (conceptual; may be inlined or a closure):

```rust
// Call before printing the first line of a new distinct element.
// Prints one blank line iff a previous element set the flag, then resets.
if pending_separator {
    println!();
    pending_separator = false;
}
```

**Invariants**:
- INV-1 (no double-blank): draining resets the flag, so two consecutive
  renderable elements produce exactly one blank.
- FR-015 (no dangling blanks): a suppressed element (quiet/gate-hidden)
  neither drains nor sets the flag, so it contributes no spacing.
- Edge (no leading blank): at turn start `pending_separator` is `false`, so
  the first element renders with no preceding blank.

## §2 — Element classification (which arms drain / set the flag)

| `AgentEvent` arm | Drain before? | Set after? | Notes |
|---|---|---|---|
| `TurnStart` | no | no | turn delimiter; first element starts fresh |
| `ApiCallStart` (spinner) | no | no | spinner is transient; the following element separates |
| `ApiCallEnd` (usage line `↪ N in · M out`) | **no** (tight before) | **yes** | TRAILING METADATA (Clarification Q3, FR-012) |
| `ReasoningDelta` (box open/body) | no (box open prints its own leading `\n`) | no | box open at render.rs:597 already leads with `\n` |
| `ContentDelta` (stream start) | **yes** | no (streaming; per-delta) | drains before first streamed char; separator set on Done/AssistantMessage |
| `AssistantMessage` | **yes** | **yes** | distinct element |
| `ToolStart` | **yes** (BEFORE `tool_row` capture) | no | see §3 — captures row after drain |
| `ToolEnd` | no (rewrite path) | **yes** | see §3 — no drain; body appends below rewrite |
| `ToolProgress` (`┊` streaming line) | no | no | transient progress, not a distinct block |
| `Notice`, `RetryAttempt`, `Compression{Start,End}`, `FallbackActivated` | **yes** | **yes** | lifecycle events (FR-012) |
| `SubagentSpawn`, `SubagentComplete`, `SubagentFailed`, `DelegationBatchComplete` | **yes** | **yes** | lifecycle events (FR-012) |
| `FileChange` (diff block) | **yes** | **yes** | distinct block (FR-012) |
| `AgentModeChanged`, `CategoryDelegation`, `BoulderWork*`, `GoalSet/Cleared`, `WisdomAccumulated` | **yes** | **yes** | OMO lifecycle events |
| `Done` | **yes** (before summary) | no | turn end; no trailing flag (next turn starts fresh) |
| `Failed` | **yes** | no | turn end |

**Trailing-metadata exception (Clarification Q3)**: `ApiCallEnd` is the
SOLE element that does NOT drain before itself — it attaches tightly to
whatever preceded it (usually the `ApiCallStart` spinner or a tool block),
then SETS the flag so the next distinct element is preceded by one blank.
This matches "tight before, one blank after" (FR-012).

## §3 — In-place tool-line rewrite interaction (FR-014)

The rewrite (spec 008 T016/T022) captures `tool_row = cursor::position().1`
at `ToolStart` (render.rs:709) and rewrites that exact row at `ToolEnd`
(render.rs:776-790). The separator must integrate without corrupting it:

```
ToolStart (animations on):
  1. drain separator  →  println!() if pending   (blank lands ABOVE the tool line)
  2. capture tool_row = cursor::position().1      (render.rs:709, AFTER drain)
  3. print spinner + name + summary                (render.rs:695-700)
  4. println!()  → move to next row                (render.rs:723)
  5. do NOT set pending_separator here (ToolEnd owns the block close)

ToolEnd (animations on):
  1. NO drain  (the rewrite targets tool_row; a drain would print a blank
                on a DIFFERENT row, but we avoid it to keep the block tight)
  2. cursor::MoveTo(0, tool_row) + Clear(CurrentLine)   (render.rs:776-780)
  3. print resolved header                              (render.rs:782)
  4. print body lines via println!                       (render.rs:793-795)
  5. set pending_separator = true                        (next element drains)
```

**Why this is safe**: `tool_row` is captured AFTER the drain, so it points
at the spinner/header row (post-blank). The `ToolEnd` rewrite clears and
re-draws exactly that row; body lines append below via normal `println!`;
the post-block flag ensures the next element's drain prints the blank below
the body. No cursor-row math shifts. The NonInteractive / animations-off
path (no `tool_row`, plain `println!` at render.rs:733-741) applies the same
drain-before/set-after with no rewrite.

## §4 — Reasoning→content separation (FR-010)

`close_reasoning` (render.rs:486-510) prints the `└─ Thought for {:.1}s`
footer. After it returns, the caller sets `pending_separator = true`. The
next element (`ContentDelta` stream start, `AssistantMessage`, or
`ToolStart`) drains, printing one blank between the footer and the content.
This holds whether content follows immediately or a tool call follows
(spec Edge Case: reasoning→tool with no assistant text).

## §5 — All-modes & gates (FR-013/015/016)

- The drain is plain `println!()` — no cursor control, no ANSI. Renders
  identically in Full / Reduced / NonInteractive (spec 008 FR-015 parity).
- Each arm's drain/set calls sit AFTER the existing `if opts.quiet` /
  `tool_progress`/`show_reasoning` guards, INSIDE the printing path. A
  suppressed arm skips both rendering and flag mutation (FR-015/016).

## §6 — Acceptance examples (rendered, CLI)

Reasoning → assistant text (FR-010):

```
┌─ Reasoning ──────────────────────
 │ thinking...
└─ Thought for 3.2s
                                              ← one blank (§4)
<assistant answer text>
```

Consecutive tool calls (FR-011):

```
  ✓ 🔧 write_file  path/to/file  3.2s
    result line 1
                                              ← one blank (§3 step 5)
$ ls -la crates  3.2s
    drwxr-xr-x  ...
```

Token-usage trailing metadata (FR-012, Clarification Q3):

```
  ⟳ querying model...
  ↪ 1.2k in · 340 out           ← tight before (no blank above), one blank after
                                              ← (this blank precedes the next block)
  ✓ 🔧 read_file  ...
```
