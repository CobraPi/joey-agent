# Phase 0 Research: TUI Crush Parity

No `NEEDS CLARIFICATION` markers remain in the Technical Context (all resolved during
`/speckit-specify` + `/speckit-clarify`, using the reference `crush` repo and the current
`joey-tui`/`joey-tools`/`joey-mcp` code already checked out locally). The items below are the
concrete technology/pattern decisions needed to execute the plan.

## R1: Markdown rendering + syntax highlighting library for streaming transcript text

- **Decision**: Add `pulldown-cmark` (event-based Markdown parser, no rendering opinions) to
  translate assistant text into styled `ratatui::text::Line`/`Span` runs directly, plus
  `syntect` for code-block syntax highlighting (via its `highlighting::HighlightLines` +
  a bundled `.tmTheme` mapped from the new Crush-matching palette in `theme.rs`).
- **Rationale**: `pulldown-cmark`'s streaming/event API matches the feature's need to render
  markdown incrementally as assistant tokens arrive (mirrors Crush's own incremental-glamour
  approach in `internal/ui/chat/incremental_glamour_test.go`) without buffering full text first.
  `syntect` is the most mature, widely-used Rust syntax highlighter and ships Sublime-compatible
  themes that are straightforward to derive from a semantic palette (background/foreground/
  keyword/string/comment map cleanly to Crush's own chroma-style mapping in
  `internal/ui/common/chromastyle.go`).
- **Alternatives considered**: `termimad` (renders markdown but owns the terminal write path,
  making it hard to compose inside ratatui's `Buffer`-based frame model — rejected);
  hand-rolled regex-based markdown (rejected: doesn't cover nested lists/tables/code fences
  robustly, doesn't match FR-004's "fully-styled markdown" bar); `tree-sitter`-based
  highlighting (rejected: much heavier dependency footprint and grammar-download story for a
  single-crate TUI, `syntect`'s bundled syntax defs are sufficient for the common languages
  Joey Agent already edits).

## R2: Sidebar "changed files" data source

- **Decision**: Reuse `joey-tools::vcs::CheckpointManager` — specifically add a small new public
  method `CheckpointManager::changed_files_since_session_start() -> Vec<FileChangeSummary>`
  (new struct: `path: PathBuf, additions: usize, deletions: usize`) computed via `git diff
  --numstat` between the shadow repo's first checkpoint and the working tree, rather than
  building a separate file-change tracker.
- **Rationale**: `joey-tools::vcs` already tracks a shadow git repo per session
  (`crates/joey-tools/src/vcs.rs`) with `Checkpoint.files_changed` as a per-checkpoint count;
  extending it with a cumulative diff query keeps file-change tracking in the crate that
  already owns git-based session state (Principle I — don't build a second file-watcher crate
  for data `joey-tools` already has the git plumbing to answer). Crush's own `filetracker`
  package (`internal/filetracker`) does something structurally equivalent (diff against a
  baseline), confirming this is the right shape of feature, just sourced from Joey's existing
  git-shadow-repo mechanism instead of an fs-watcher.
- **Alternatives considered**: A new `notify`-crate-based live file watcher (rejected — YAGNI
  per Constitution Principle V; git-diff-on-render is sufficient since the sidebar only needs to
  reflect state at redraw time, not push live updates); parsing `git status --porcelain`
  (rejected — doesn't give addition/deletion counts needed for FR-009a's Crush-equivalent
  summary).

## R3: Sidebar "LSP status" data source

- **Decision**: A minimal `LspStatus` enum (`NotConfigured`, `Starting`, `Ready { server_count:
  usize }`, `Error { message: String }`) stored directly in `joey-tui::state::App`, defaulting
  to `NotConfigured` since Joey Agent has no LSP client today. `joey-cli`'s host loop sets it to
  `NotConfigured` unconditionally for now.
- **Rationale**: Satisfies FR-009a/FR-012 ("expose enough information... extending the existing
  state model") and the edge case requiring a stable empty/zero-state render, without building a
  real LSP client (explicitly out of scope per spec.md Assumptions and Constitution Principle V
  — no near-term consumer beyond "the sidebar has a slot for it"). When Joey Agent gains real LSP
  support, only `joey-cli`'s wiring changes; `joey-tui`'s `LspStatus` enum and its sidebar
  renderer are already forward-compatible.
- **Alternatives considered**: Omitting the LSP section entirely (rejected — contradicts the
  Clarification answer requiring full sidebar section parity, including empty-state rendering);
  building a real LSP client now (rejected — far exceeds this feature's scope, would violate
  Principle I by pulling in a whole new subsystem crate for a single sidebar line).

## R4: Sidebar "MCP status" and "skills status" data sources

- **Decision**: MCP status reuses `joey_mcp::McpClient::server_name()` /
  `initialize_result()` already exposed publicly — the sidebar renders a count of configured
  servers vs. connected servers (mirroring Crush's `mcpCount` helper in
  `internal/ui/model/sidebar.go`). Skills status reuses the existing
  `joey-tools::tools::skills_tool` listing logic (already used by the `skills_list` tool) to
  report a count of discovered skills.
- **Rationale**: Both crates already expose exactly the data needed publicly; no new public
  surface is required beyond what's already `pub` (Principle III — don't add surface area
  callers don't need). This is a read-only status query each frame/redraw, consistent with how
  the sidebar model-info section already reads `AppState` fields today.
- **Alternatives considered**: Push-based status via channels/events (rejected — YAGNI; Crush
  itself computes these counts synchronously at render time in `sidebar.go`, and Joey Agent's
  redraw cadence is already fast enough that pull-based reads are not a performance concern).

## R5: Dialog framework shape

- **Decision**: A `Dialog` trait (`fn draw(&self, frame, area, theme)`, `fn handle_key(&mut
  self, key) -> DialogOutcome`, `fn title(&self) -> &str`) plus a `DialogStack` (`Vec<Box<dyn
  Dialog>>`) owned by `App`. `app.rs`'s key-handling routes to `DialogStack::handle_key` first
  when non-empty, matching Crush's own dialog-owns-focus pattern (`internal/ui/model/ui.go`'s
  `uiFocusState` plus each dialog's own `Update`/`View` in `internal/ui/dialog/*.go`).
- **Rationale**: Directly satisfies Constitution Principle II (new dialogs = new module +
  registration, no central match growth) and matches the interaction model Crush already uses
  (a focus-owning overlay stack), which is what FR-009's "equivalent overlay dialogs" requires
  functionally, not just visually.
- **Alternatives considered**: A single giant `enum ActiveDialog { None, ModelPicker(...),
  SessionPicker(...), ... }` matched everywhere it's drawn/handled (rejected — this is exactly
  the "central match/if chain enumerating every existing variant" Principle II prohibits growing
  further; it's closer to Joey's current `Focus` enum shape and was rejected specifically because
  this feature adds ~10 variants, which is the threshold where the trait+registry pays off).

## R6: Terminal capability detection

- **Decision**: A `capabilities.rs` module with `detect_capabilities() -> Capabilities` reading
  `COLORTERM`/`TERM` env vars (truecolor detection) and `LANG`/`LC_ALL` (UTF-8 → Unicode glyph
  support) — pure functions over `&str` inputs (not env access directly) so they're unit-testable
  per FR-011/SC-004, with `detect_capabilities()` itself being the only impure wrapper that reads
  real env vars.
- **Rationale**: Matches Crush's own `internal/ui/common/capabilities.go` + `ansi16.go` pattern
  (env-var sniffing plus a color/glyph downsampling table) while keeping the downsampling logic
  itself pure and directly testable, satisfying the Clarification that only automated tests (not
  manual multi-emulator passes) are required.
- **Alternatives considered**: A runtime terminal query (e.g. sending a DA1/DA2 escape sequence
  and reading the response) — rejected as significantly more complex and not what Crush itself
  does for its primary detection path; env-var sniffing is the documented, portable convention
  crossterm-based TUIs already rely on.

## Summary of new dependencies

| Crate | New dependency | Scope |
|---|---|---|
| `joey-tui` | `pulldown-cmark` | markdown parsing (R1) |
| `joey-tui` | `syntect` | code-block syntax highlighting (R1) |

No other workspace crate requires a new dependency; `similar` (diff), `unicode-width`,
`textwrap` are already `joey-tui` dependencies.
