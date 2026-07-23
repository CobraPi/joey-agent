# Contract: `Dialog` trait and `DialogStack`

**Crate/module**: `crates/joey-tui/src/dialog/mod.rs`

**Purpose**: The single extension point every overlay UI surface (FR-009) plugs into, satisfying
Constitution Principle II (new dialog = new module implementing this trait + one registration
call, no central match/if chain growth).

## Trait

```rust
pub trait Dialog {
    /// Human-readable title shown in the dialog chrome.
    fn title(&self) -> &str;

    /// Render the dialog's content into the given area (already sized/positioned
    /// as an overlay by the caller — the Dialog does not compute its own screen
    /// position).
    fn draw(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect, theme: &Theme);

    /// Handle a key event while this dialog has focus. Returns an outcome telling
    /// the DialogStack whether to keep this dialog open, close it, or close it
    /// and emit a TuiAction for the host to act on.
    fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> DialogOutcome;
}

pub enum DialogOutcome {
    /// Stay open; no state change needed beyond what handle_key already applied.
    Continue,
    /// Close this dialog with no further action.
    Close,
    /// Close this dialog and ask the host to perform this action
    /// (e.g. apply a selected model, submit a confirmed prompt).
    CloseWith(TuiAction),
}

pub struct DialogStack {
    stack: Vec<Box<dyn Dialog>>,
}

impl DialogStack {
    pub fn is_empty(&self) -> bool;
    pub fn push(&mut self, dialog: Box<dyn Dialog>);
    pub fn top(&self) -> Option<&dyn Dialog>;
    pub fn handle_key(&mut self, key: crossterm::event::KeyEvent) -> Option<TuiAction>;
    pub fn draw(&self, frame: &mut ratatui::Frame, screen: ratatui::layout::Rect, theme: &Theme);
}
```

## Behavioral contract

1. `DialogStack::handle_key` MUST route the key to the topmost dialog (`stack.last_mut()`) only;
   dialogs lower in the stack MUST NOT receive the key while a dialog is on top of them (matches
   Crush's overlay focus model — `internal/ui/dialog/dialog.go`).
2. On `DialogOutcome::Close` or `CloseWith`, the topmost dialog MUST be popped before the next
   `handle_key` call; on `CloseWith(action)`, the returned `TuiAction` MUST be surfaced to the
   host's action queue in the same event cycle (no dropped actions).
3. `App`'s top-level key handling (`app.rs`) MUST check `!self.dialogs.is_empty()` and delegate
   to `DialogStack::handle_key` BEFORE any other key handling (editor input, scroll, etc.) —
   this is what gives dialogs modal focus, matching FR-009's "keyboard-only navigation" bar.
4. Adding a new dialog type MUST NOT require editing `DialogStack`, `app.rs`'s key routing, or
   any other existing dialog's code — only: (a) a new file implementing `Dialog`, (b) one new
   `DialogKind` variant, (c) one constructor arm mapping `DialogKind` → `Box<dyn Dialog>`. Item
   (c) is the one small, intentionally-necessary match (constructing a new box from a kind enum)
   — it is not the same as re-implementing draw/key-handling logic per dialog inline, and is
   exempted from Principle II's "central chain" prohibition because it does nothing but
   construct.

## Seam-level test (Constitution Principle IV)

`crates/joey-tui/tests/dialog_seam.rs` MUST:
- Define two trivial fake `Dialog` implementations locally in the test file (not in `src/`).
- Push both onto a `DialogStack`, assert `handle_key` only reaches the top one.
- Assert popping via `Close` restores focus/handling to the next one down.
- Assert `CloseWith` surfaces the wrapped `TuiAction` exactly once.

This proves the registry mechanism itself works generically, independent of any concrete
dialog's business logic.
