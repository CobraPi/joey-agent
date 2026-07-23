# Phase 1 Data Model: TUI Crush Parity

All entities below live in `crates/joey-tui` (`state.rs` unless noted) and are plain Rust data
types — no new persistence, no ambient globals (Constitution Principle III).

## TranscriptItem (extended)

Existing enum in `state.rs` (`User`, `Assistant`, `Tool`, `Notice`, `Error` — see current
definition). No variants are removed; rendering for each variant moves behind the new
`TranscriptRenderer` trait (contracts/transcript-renderer-trait.md) rather than a single
`draw_transcript` match. Fields are unchanged except:

- `Tool { name, status, summary, result_preview, duration_secs }` — `result_preview` may now
  carry a `DiffPreview` variant (see below) in addition to plain `String`, so diff-producing
  tools (FR-006) render as a unified diff view instead of raw text.

```rust
pub enum ToolResultPreview {
    Text(String),
    Diff(DiffPreview),
}

pub struct DiffPreview {
    pub path: String,
    pub hunks: Vec<DiffHunk>, // reuses joey-tools::difflib output shape
}
```

**Validation**: `Tool.status` transitions MUST follow `Running -> {Done, Failed}` only (already
enforced by `App::apply`'s existing `AgentEvent::ToolEnd` handling — no change to that state
machine, only to what's rendered).

## SidebarSection (new)

One instance per section shown in the sidebar (FR-009a). Not a single struct — four independent
data slots on `App`, matching Crush's independent `modelInfo`/`filesInfo`/`lspInfo`/`mcpInfo`/
`skillsInfo` render functions (`internal/ui/model/sidebar.go`):

```rust
pub struct SidebarFileChange {
    pub path: String,
    pub additions: usize,
    pub deletions: usize,
}

pub enum LspStatus {
    NotConfigured,
    Starting,
    Ready { server_count: usize },
    Error { message: String },
}

pub struct McpStatusSummary {
    pub configured: usize,
    pub connected: usize,
}

pub struct SkillsStatusSummary {
    pub discovered: usize,
}
```

Added to `App` as:

```rust
pub struct App {
    // ...existing fields...
    pub sidebar_files: Vec<SidebarFileChange>,
    pub sidebar_lsp: LspStatus,
    pub sidebar_mcp: McpStatusSummary,
    pub sidebar_skills: SkillsStatusSummary,
}
```

**Validation**: Each section MUST render even when its data is empty/zero (edge case in spec.md)
— `sidebar_files: vec![]` renders "No changes"; `McpStatusSummary { configured: 0, connected: 0
}` renders "0/0"; `LspStatus::NotConfigured` renders a fixed "Not configured" line. These empty
states are asserted in the sidebar widget tests (`tests/smoke.rs` extension).

**Relationships**: Populated each frame/redraw by `joey-cli/src/tui.rs` host loop by querying
`joey-tools::vcs::CheckpointManager::changed_files_since_session_start()` (research.md R2),
`joey_mcp::McpClient` instances the host already holds (R4), and
`joey-tools::tools::skills_tool`'s existing discovery listing (R4). `joey-tui` itself does not
depend on `joey-mcp`/`joey-tools` — these are plain-data pushes from the host, preserving the
crate boundary (Principle I): `joey-tui`'s `Cargo.toml` gains no new crate dependency for this.

## Theme / Palette (rewritten)

`theme.rs`'s existing `Theme` struct (background/foreground/accent/status Rgb constants +
`gradient()`/`sample_gradient_spans()` methods) is retained structurally; **values** are updated
to Crush's aurora-synthwave defaults (electric cyan, hot orchid-pink, acid lime, jewel-toned dark
background — transcribed from `internal/ui/styles/themes.go` reference values) and the semantic
role set is expanded to match Crush's `styles.Theme` fields 1:1 (FR-007):

```rust
pub struct Theme {
    pub bg_base: Rgb,
    pub bg_panel: Rgb,
    pub bg_elevated: Rgb,
    pub bg_highest: Rgb,
    pub fg_base: Rgb,
    pub fg_muted: Rgb,
    pub fg_subtle: Rgb,
    pub primary: Rgb,     // cyan
    pub secondary: Rgb,   // orchid
    pub accent: Rgb,      // lime
    pub gold: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
    pub error: Rgb,
    pub info: Rgb,
    pub busy: Rgb,
    pub separator: Rgb,
    pub grad_0: Rgb,
    pub grad_1: Rgb,
    pub grad_2: Rgb,
    pub grad_3: Rgb,
}
```

**Validation**: A unit test asserts each field resolves to Crush's documented hex value (no
implementation-detail leak into the spec — this is a plan/code-level test, not a spec
requirement) and that `gradient()` interpolation is perceptual/luminance-aware (FR-008), matching
`internal/ui/styles/grad.go`'s `lerp_u8` + luminance-clamp approach.

## Dialog / Overlay (new)

See contracts/dialog-trait.md for the full trait contract. Data-model-relevant shape:

```rust
pub enum DialogKind {
    ModelPicker,
    SessionPicker,
    Permission { tool_name: String, description: String },
    Confirm { prompt: String, on_confirm: ConfirmAction },
    Completions { query: String },
    Quit,
    ReasoningPicker,
    Notification { message: String, kind: NoticeKind },
    Question(QuestionForm),
}

pub enum QuestionForm {
    Single { options: Vec<String> },
    Multi { options: Vec<String> },
    FreeText,
    Confirm,
    Editor { initial: String },
}
```

`App.dialogs: DialogStack` holds zero or more active `Box<dyn Dialog>` instances (a stack so a
confirm dialog can be nested over e.g. a permission dialog, matching Crush's overlay behavior in
`internal/ui/dialog/dialog.go`).

## Capabilities (new)

```rust
pub struct Capabilities {
    pub truecolor: bool,
    pub unicode: bool,
}
```

Pure detection function signature: `pub fn detect(colorterm: Option<&str>, term: Option<&str>,
lang: Option<&str>) -> Capabilities` (env-var reads happen in a thin `detect_capabilities()`
wrapper in `capabilities.rs`, not in this pure function — keeps FR-011/SC-004 unit-testable
without mocking the process environment).

**Validation**: Table-driven unit tests (research.md R6) cover: `COLORTERM=truecolor` →
`truecolor: true`; absent `COLORTERM` + `TERM=xterm` → `truecolor: false`; `LANG=en_US.UTF-8` →
`unicode: true`; `LANG=C` → `unicode: false`.
