//! The TUI runtime: owns the terminal, the animation timers, and the mapping
//! from crossterm key events to [`TuiAction`]s.
//!
//! Architecture (Elm-like, single source of truth):
//!   - [`App`] (`state.rs`) is the model.
//!   - [`Tui`] owns the terminal + animation timers.
//!   - The host (joey-cli) runs the loop: it polls crossterm input, drains
//!     agent events into the model, and calls [`Tui::tick_animations`] +
//!     [`Tui::draw`] each frame.

use std::io::{self, Stdout};
use std::sync::Once;
use std::time::Duration;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use ratatui::Terminal;

use crate::anim::{Activity, Clock, Equalizer, ParticleField, Pulse, Spinner};
use crate::input::Input;
use crate::state::{App, RunMode, TranscriptItem};
use crate::theme::Theme;
use crate::widgets;

/// A request emitted by the TUI to the host (the REPL) to act on user input.
#[derive(Debug)]
pub enum TuiAction {
    /// Submit this prompt to the agent (host queues it if a turn is running).
    Submit(String),
    /// The user wants to interrupt the current turn.
    Interrupt,
    /// The user wants to quit the session.
    Quit,
    /// Switch to a different agent (T033, BC-015).
    SwitchAgent(String),
    /// Copy the text of transcript item `usize` to the clipboard (the `y`
    /// key in transcript mode). Host owns the clipboard access.
    CopyItem(usize),
}

pub type FrameBackend = CrosstermBackend<Stdout>;
pub type FrameTerminal = Terminal<FrameBackend>;

/// Expanded-feed layout split (NeuroCode expanded mode): how the main body
/// height divides between the transcript strip (top — keeps live streaming
/// visible) and the expanded context feed (bottom, the majority). The
/// transcript is bottom-anchored, so the strip always shows the newest
/// lines including the in-flight streaming tail.
///
/// Returns `(transcript_rows, feed_rows)`. Caller guarantees `total >= 12`.
fn split_expanded_feed(total: u16) -> (u16, u16) {
    debug_assert!(total >= 12, "caller guards total >= 12");
    let transcript = ((total as f32 * 0.3).round() as u16).clamp(4, 10);
    (transcript, total - transcript)
}

/// Render the body region: transcript (left) + sidebar (right), including
/// the NeuroCode expanded-mode takeover of the main area. Extracted from
/// `Tui::draw` so tests can drive the REAL layout against a TestBackend
/// (which `Tui::draw`'s io-bound excludes) and assert on the hit-test
/// rects the user's mouse would actually see.
///
/// `glow` is the pulse animation value; `spinner`/`equalizer` are the
/// shared animation widgets.
fn render_body(
    f: &mut Frame,
    area: Rect,
    app: &App,
    theme: Theme,
    focused: bool,
    glow: f32,
    spinner: &crate::anim::Spinner,
    equalizer: &crate::anim::Equalizer,
) {
    // Body: transcript (left, large) + sidebar (right). The sidebar
    // yields entirely on narrow terminals.
    let show_sidebar = area.width >= 72;
    let body = if show_sidebar {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(40), Constraint::Length(34)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1)])
            .split(area)
    };

    // When reasoning is live (and shown), split the transcript
    // vertically: conversation + reasoning.
    let show_reasoning_panel =
        app.reasoning_open && app.show_reasoning && body[0].height >= 14;
    // NeuroCode expanded mode (click the docked feed to toggle): the
    // context feed takes over the main screen — the transcript (with its
    // live streaming tail) keeps a strip at the top so streaming stays
    // visible, and the expanded feed fills the rest.
    if app.neurocode_expanded && app.neurocode_active && body[0].height >= 12 {
        let (transcript_h, feed_h) = split_expanded_feed(body[0].height);
        let main = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(transcript_h),
                Constraint::Min(feed_h),
            ])
            .split(body[0]);
        widgets::draw_transcript(f, main[0], app, theme, focused, glow);
        widgets::draw_neurocode_panel(f, main[1], app, theme);
    } else if show_reasoning_panel {
        let convo_split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(8)])
            .split(body[0]);
        widgets::draw_transcript(f, convo_split[0], app, theme, focused, glow);
        widgets::draw_reasoning(f, convo_split[1], app, theme, spinner);
    } else {
        widgets::draw_transcript(f, body[0], app, theme, focused, glow);
    }

    if show_sidebar {
        // NeuroCode live feed (feature 015 follow-up): when the engine
        // is active, split the sidebar vertically — OMO panel on top,
        // context feed anchored at the BOTTOM of the sidebar. The feed
        // gets up to 40% of the sidebar (min 6 rows) and yields
        // entirely when the sidebar is too short. While the feed is
        // EXPANDED onto the main screen, the docked copy is hidden
        // (one live view at a time).
        if app.neurocode_active && !app.neurocode_expanded && body[1].height >= 16 {
            let feed_h =
                ((body[1].height as f32 * 0.4).round() as u16).clamp(6, body[1].height - 8);
            let side = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(8), Constraint::Length(feed_h)])
                .split(body[1]);
            widgets::draw_omo_panel(f, side[0], app, theme, spinner, equalizer);
            widgets::draw_neurocode_panel(f, side[1], app, theme);
        } else {
            widgets::draw_omo_panel(f, body[1], app, theme, spinner, equalizer);
        }
    }
}

/// Restore the terminal even if we panic mid-frame: a raw-mode alternate
/// screen would otherwise swallow the panic message and wreck the shell.
fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let orig = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            orig(info);
        }));
    });
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
}

/// The TUI controller. Generic over the ratatui backend so tests can drive
/// `handle_key` against a `TestBackend` without a real TTY; the default
/// type parameter keeps the crossterm/stdout surface unchanged for hosts.
pub struct Tui<B: ratatui::backend::Backend = FrameBackend> {
    pub app: App,
    pub theme: Theme,
    pub input: Input,
    terminal: Terminal<B>,
    // animation state
    activity: Activity,
    clock: Clock,
    spinner: Spinner,
    orbit_spinner: Spinner,
    field: ParticleField,
    equalizer: Equalizer,
    pulse: Pulse,
    show_help: bool,
    focus: Focus,
    restored: bool,
    /// Smart-completion engine (@-context / path words). Stale-tolerant:
    /// file listings refresh in a background thread, never blocking draws.
    completion_engine: joey_tools::completion::CompletionEngine,
    /// Working directory for @-context project-file search.
    completion_cwd: std::path::PathBuf,
    /// Set after accepting a completion: suppresses re-opening the popup
    /// until the next real edit (typing/backspace clears it).
    completion_suppressed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    Transcript,
}

/// Raw-mode enter path: only meaningful for the real crossterm/stdout
/// backend (the default type parameter), so it lives on the concrete type.
impl Tui<FrameBackend> {
    /// Enter the alternate screen and create the terminal.
    pub fn enter(app: App, theme: Theme) -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(e) = execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture
        ) {
            // Leave the shell usable for the caller's line-REPL fallback.
            let _ = disable_raw_mode();
            return Err(e);
        }
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        let size = terminal.size()?;
        Ok(Self {
            app,
            theme,
            input: Input::new(),
            terminal,
            activity: Activity::idle(),
            clock: Clock::start(),
            spinner: Spinner::dots(),
            orbit_spinner: Spinner::orbit(),
            field: ParticleField::new(size.width as usize, size.height as usize),
            equalizer: Equalizer::new(28),
            pulse: Pulse::new(),
            show_help: false,
            focus: Focus::Input,
            restored: false,
            completion_engine: joey_tools::completion::CompletionEngine::new(),
            completion_cwd: std::env::current_dir().unwrap_or_default(),
            completion_suppressed: false,
        })
    }
}

impl<B: ratatui::backend::Backend> Tui<B> {
    /// Test-only constructor: build a controller over an arbitrary backend
    /// (e.g. ratatui's `TestBackend`) so `handle_key` semantics can be
    /// unit-tested without a real TTY. Never enters raw mode.
    #[cfg(test)]
    pub fn new_for_test(app: App, theme: Theme, terminal: Terminal<B>) -> Self {
        let size = terminal
            .size()
            .map(|s| (s.width, s.height))
            .unwrap_or((80, 24));
        Self {
            app,
            theme,
            input: Input::new(),
            terminal,
            activity: Activity::idle(),
            clock: Clock::start(),
            spinner: Spinner::dots(),
            orbit_spinner: Spinner::orbit(),
            field: ParticleField::new(size.0 as usize, size.1 as usize),
            equalizer: Equalizer::new(28),
            pulse: Pulse::new(),
            show_help: false,
            focus: Focus::Input,
            restored: true, // never touch the real terminal from tests
            completion_engine: joey_tools::completion::CompletionEngine::new(),
            completion_cwd: std::env::current_dir().unwrap_or_default(),
            completion_suppressed: false,
        }
    }

    /// Restore the terminal. Idempotent; also runs on Drop.
    pub fn leave(&mut self) -> io::Result<()> {
        if !self.restored {
            self.restored = true;
            restore_terminal();
        }
        Ok(())
    }

    /// Compute the active-agent target for animation pacing.
    fn target_agents(&self) -> usize {
        if self.app.is_busy() {
            // The base agent counts as 1; each concurrent tool adds more.
            let mut n = 1;
            for a in &self.app.active_agents {
                if !matches!(a.phase, crate::state::AgentPhase::Idle) {
                    n += 1;
                }
            }
            n
        } else {
            0
        }
    }

    /// Advance all animation state by the elapsed dt.
    pub fn tick_animations(&mut self) {
        let dt = self.clock.dt();
        let target = self.target_agents();
        self.activity.update(target, dt);
        let speed = self.activity.speed();
        self.spinner.tick(dt, speed);
        self.orbit_spinner.tick(dt, speed);
        self.field.tick(dt, self.activity, self.theme);
        self.equalizer.tick(dt, self.activity);
        self.pulse.tick(dt, self.activity);
    }

    /// How long the host should sleep/poll between frames. Scales with
    /// activity so an idle dashboard doesn't spin the CPU at 60fps.
    pub fn frame_budget(&self) -> Duration {
        let fps = u64::from(self.activity.target_fps().clamp(10, 60));
        Duration::from_millis(1000 / fps)
    }

    /// Render one frame to the terminal.
    pub fn draw(&mut self) -> io::Result<()>
    where
        std::io::Error: From<B::Error>,
    {
        let Self {
            app,
            theme,
            input,
            terminal,
            spinner,
            orbit_spinner,
            field,
            equalizer,
            pulse,
            show_help,
            focus,
            ..
        } = self;
        let theme = *theme;
        let glow = pulse.value();

        terminal.draw(|f| {
            let area = f.area();

            // Tiny-terminal fallback: the full layout needs room.
            if area.width < 24 || area.height < 9 {
                let msg = Paragraph::new(Line::from("⚠ terminal too small"))
                    .style(Style::default().fg(theme.warning.to_color()));
                f.render_widget(msg, area);
                return;
            }

            // 1. Background fill (deep void).
            f.render_widget(
                Block::default().style(Style::default().bg(theme.bg_void.to_color())),
                area,
            );
            // 2. Particle backdrop.
            widgets::draw_particles(f, field, theme, area);

            // 3. Layout: header / body / input / status. The input grows with
            // its content (1 visible row minimum, up to 5).
            let input_h = (input.line_count() as u16 + 2).clamp(3, 7);
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),        // header
                    Constraint::Min(5),           // body
                    Constraint::Length(input_h),  // input
                    Constraint::Length(1),        // status
                ])
                .split(area);

            widgets::draw_header(f, chunks[0], app, theme, orbit_spinner, pulse);

            let transcript_focused = *focus == Focus::Transcript;
            render_body(
                f,
                chunks[1],
                app,
                theme,
                transcript_focused,
                glow,
                spinner,
                equalizer,
            );

            widgets::draw_input(f, chunks[2], input, app, theme, *focus == Focus::Input, glow);

            // Slash-command popup floats above the input box.
            if app.slash_menu_open {
                widgets::draw_slash_popup(f, area, app, &input.text(), theme);
            }
            // Generic completion popup (@-context / paths).
            if app.completion_menu_open {
                widgets::draw_completion_popup(f, area, app, theme);
            }

            let elapsed = app.turn_started.map(|t| t.elapsed()).unwrap_or_default();
            widgets::draw_status(f, chunks[3], app, theme, elapsed);

            if *show_help {
                widgets::draw_help_overlay(f, area, theme);
            }

            if app.agent_picker_open {
                widgets::draw_agent_picker(f, area, app, &theme);
            }

            if app.search_open {
                widgets::draw_search_bar(f, area, app, &theme);
            }
        })?;
        Ok(())
    }

    /// Resize the internal buffers (call on terminal resize events).
    pub fn resize(&mut self, w: u16, h: u16) {
        let _ = self.terminal.resize(Rect::new(0, 0, w, h));
        self.field.resize(w as usize, h as usize);
    }

    /// Borrow the application state.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// Mutably borrow the application state.
    pub fn app_mut(&mut self) -> &mut App {
        &mut self.app
    }

    /// Toggle the help overlay (also reachable via `?` / F1).
    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    fn toggle_reasoning(&mut self) {
        self.app.show_reasoning = !self.app.show_reasoning;
        if !self.app.show_reasoning {
            // Drop the live block so the panel doesn't linger with stale text.
            self.app.reasoning_open = false;
            self.app.streaming_reasoning.clear();
        }
    }

    /// Handle a single crossterm key event. Returns an action for the host.
    ///
    /// Design: printable characters ALWAYS reach the input box when it has
    /// focus — global shortcuts are limited to control-modified keys and
    /// keys that can't collide with typing (Esc, Tab, F1, PgUp/PgDn).
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<TuiAction> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        // Help overlay swallows keys until dismissed.
        if self.show_help {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('q') | KeyCode::Enter => {
                    self.show_help = false;
                }
                _ => {}
            }
            return None;
        }

        // Search overlay: route all keys to search input.
        if self.app.search_open {
            return self.handle_search_key(key);
        }

        // Agent picker overlay swallows keys until dismissed (BC-014).
        if self.app.agent_picker_open {
            let roster_len = self.app.agent_roster.len();
            match key.code {
                KeyCode::Esc => {
                    self.app.agent_picker_open = false;
                    return None;
                }
                KeyCode::Enter => {
                    // Select the highlighted agent (BC-015).
                    let idx = self.app.agent_picker_cursor;
                    self.app.agent_picker_open = false;
                    if idx < roster_len {
                        let agent_name = self.app.agent_roster[idx].name.clone();
                        self.app.active_agent_index = idx;
                        return Some(TuiAction::SwitchAgent(agent_name));
                    }
                    return None;
                }
                KeyCode::Tab | KeyCode::Down => {
                    if roster_len > 0 {
                        self.app.agent_picker_cursor =
                            (self.app.agent_picker_cursor + 1) % roster_len;
                    }
                    return None;
                }
                KeyCode::Up => {
                    if roster_len > 0 {
                        if self.app.agent_picker_cursor == 0 {
                            self.app.agent_picker_cursor = roster_len - 1;
                        } else {
                            self.app.agent_picker_cursor -= 1;
                        }
                    }
                    return None;
                }
                _ => return None,
            }
        }

        // Global keys.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            // Ctrl+C escalation (engine-actor model): when a turn is busy,
            // the 1st press interrupts cooperatively and the 2nd (within 2s,
            // host-side) force-kills + restarts the engine — the GUI never
            // dies with the compute. When idle, Ctrl+C quits (parity with
            // the line REPL).
            KeyCode::Char('c') if ctrl => {
                if self.app.is_busy() {
                    return Some(TuiAction::Interrupt);
                }
                self.app.mode = RunMode::Quitting;
                return Some(TuiAction::Quit);
            }
            // Esc interrupts the agent when busy; otherwise closes
            // overlays / returns focus / quits.
            KeyCode::Esc => {
                if self.app.search_open {
                    self.app.search_open = false;
                    self.app.search_query.clear();
                    return None;
                }
                if self.app.agent_picker_open {
                    self.app.agent_picker_open = false;
                    return None;
                }
                // Dock the expanded NeuroCode feed back to its bottom-right
                // panel before any lower-priority Esc behavior.
                if self.app.neurocode_expanded {
                    self.app.toggle_neurocode_expanded();
                    return None;
                }
                if self.focus == Focus::Transcript {
                    self.focus = Focus::Input;
                    return None;
                }
                if self.app.is_busy() {
                    return Some(TuiAction::Interrupt);
                }
                self.app.mode = RunMode::Quitting;
                return Some(TuiAction::Quit);
            }
            KeyCode::Char('d') if ctrl => {
                // EOF on an empty idle prompt quits; otherwise delete-forward.
                if !self.app.is_busy() && self.focus == Focus::Input && self.input.is_empty() {
                    self.app.mode = RunMode::Quitting;
                    return Some(TuiAction::Quit);
                }
                if self.focus == Focus::Input {
                    self.input.delete();
                }
                return None;
            }
            KeyCode::Char('r') if ctrl => {
                self.toggle_reasoning();
                return None;
            }
            // Feature 005 (T023): Ctrl+E cycles the most-recent reasoning
            // block through the three-state expand cycle.
            KeyCode::Char('e') if ctrl => {
                self.app.cycle_focused_reasoning_expand();
                return None;
            }
            // Feature 005 (T028): Ctrl+G toggles the most-recent tool call's
            // expanded state (full args/result view).
            KeyCode::Char('g') if ctrl => {
                self.app.toggle_focused_tool_expand();
                return None;
            }
            KeyCode::Char('l') if ctrl => {
                self.app.transcript.clear();
                self.app.scroll = None;
                return None;
            }
            KeyCode::F(1) => {
                self.show_help = true;
                return None;
            }
            // ── Focus switching ──────────────────────────────────────
            // Ctrl+T toggles between Input and Transcript focus, giving full
            // vim-style navigation (j/k/g/G) over the conversation history.
            KeyCode::Char('t') if ctrl => {
                self.focus = match self.focus {
                    Focus::Input => Focus::Transcript,
                    Focus::Transcript => Focus::Input,
                };
                return None;
            }
            // When in Input focus, Shift+Up switches to Transcript scroll mode.
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.focus = Focus::Transcript;
                self.app.scroll_up(1);
                return None;
            }
            // T031/T146: Plain Up (no modifier) on single-line input switches
            // focus to the transcript, restoring the behavior Tab used to have
            // before it was repurposed for agent switching.
            KeyCode::Up
                if self.focus == Focus::Input
                    && !key.modifiers.contains(KeyModifiers::SHIFT)
                    && self.input.line_count() == 1 =>
            {
                // History recall — reedline/CLI parity. Plain Up on a
                // single-line draft walks backward through the shared
                // ~/.joey/.joey_history (same file the line REPL reads).
                // Multi-line drafts keep cursor movement (handled below);
                // transcript scrolling has Shift+Up / Ctrl+T / PgUp.
                if let Some(text) = self.app.history_prev(&self.input.text()) {
                    self.input.set_text(&text);
                    self.refresh_completion_menus();
                }
                return None;
            }
            KeyCode::Tab => {
                // Tab opens the agent picker overlay (BC-013). If busy, the
                // switch is deferred to the next turn (BC-016).
                if self.app.is_busy() {
                    // Deferred: queue for next turn (host handles the queue).
                    // For now, just ignore — the host can check on next turn.
                } else {
                    self.app.agent_picker_open = true;
                }
                return None;
            }
            KeyCode::BackTab => {
                // Shift+Tab: if picker is open, cycle backward. Otherwise no-op.
                if self.app.agent_picker_open && !self.app.agent_roster.is_empty() {
                    if self.app.agent_picker_cursor == 0 {
                        self.app.agent_picker_cursor = self.app.agent_roster.len() - 1;
                    } else {
                        self.app.agent_picker_cursor -= 1;
                    }
                }
                return None;
            }
            KeyCode::PageUp => {
                self.app.scroll_up(10);
                // Switch to transcript focus so the user sees j/k are available.
                if self.focus == Focus::Input {
                    self.focus = Focus::Transcript;
                }
                return None;
            }
            // NeuroCode feed scrolling (Alt+Up / Alt+Down) — scrolls the
            // bottom-right live context panel without stealing input focus.
            KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) && self.app.neurocode_active => {
                self.app.neurocode_scroll = self.app.neurocode_scroll.saturating_add(1);
                return None;
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) && self.app.neurocode_active => {
                self.app.neurocode_scroll = self.app.neurocode_scroll.saturating_sub(1);
                return None;
            }
            KeyCode::PageDown => {
                self.app.scroll_down(10);
                // If we've reached the bottom, return focus to input.
                if self.app.scroll.is_none() {
                    self.focus = Focus::Input;
                }
                return None;
            }
            // Half-page scrolling (Ctrl+u / Ctrl+d style, but using Ctrl+b/f
            // to avoid clobbering the input editor's kill commands).
            KeyCode::Char('b') if ctrl => {
                let half = 15usize;
                self.app.scroll_up(half);
                if self.focus == Focus::Input {
                    self.focus = Focus::Transcript;
                }
                return None;
            }
            KeyCode::Char('f') if ctrl => {
                let half = 15usize;
                self.app.scroll_down(half);
                if self.app.scroll.is_none() {
                    self.focus = Focus::Input;
                }
                return None;
            }
            _ => {}
        }

        // Focus-dependent keys.
        match self.focus {
            Focus::Transcript => {
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => self.app.scroll_up(1),
                    KeyCode::Down | KeyCode::Char('j') => self.app.scroll_down(1),
                    KeyCode::Char('g') | KeyCode::Home => self.app.scroll_to_top(),
                    KeyCode::Char('G') | KeyCode::End => self.app.scroll_to_bottom(),
                    // Space / x: expand-toggle the item at the TOP of the
                    // viewport (mouse clicks also toggle, via hit-testing).
                    // Scroll so the tool/terminal block you want is the top
                    // visible item, then press Space/x.
                    KeyCode::Char(' ') | KeyCode::Char('x') => {
                        // Expand the item under the viewport — reuse the
                        // mouse hit-test resolution at the CENTER row of
                        // the transcript (same machinery clicks use, so
                        // keyboard and mouse always agree). When the center
                        // lands on a non-expandable item, fall back to the
                        // first expandable item at or below the top of the
                        // view (the common "whole transcript fits" case:
                        // Space expands the tool output you can see).
                        let (_tx, ty, _tw, th) = self.app.last_text_area.get();
                        let center_row = ty + th / 2;
                        let center_col = 4; // inside the text area
                        let idx = widgets::transcript_hit_test(
                            &self.app, self.theme, center_row, center_col,
                        );
                        let resolved = match idx {
                            Some(i) if self.app.item_is_expandable(i) => Some(i),
                            _ => {
                                let top = widgets::transcript_item_at_top(&self.app, self.theme);
                                match top {
                                    Some(t0) => (t0..self.app.transcript.len())
                                        .find(|&i| self.app.item_is_expandable(i)),
                                    None => None,
                                }
                            }
                        };
                        if let Some(i) = resolved {
                            self.app.toggle_item_expand_by_index(i);
                        }
                    }
                    KeyCode::Enter => {
                        self.focus = Focus::Input;
                        return None;
                    }
                    KeyCode::Char('?') => self.show_help = true,
                    KeyCode::Char('r') => self.toggle_reasoning(),
                    KeyCode::Char('/') => {
                        // Enter search mode.
                        self.app.search_open = true;
                        self.app.search_query.clear();
                    }
                    KeyCode::Char('n') => {
                        // Find next match.
                        self.app.search_next(true);
                    }
                    KeyCode::Char('N') => {
                        // Find previous match.
                        self.app.search_next(false);
                    }
                    // `y` copies the last assistant message to the clipboard
                    // (host handles the clipboard); `Y` copies the last user
                    // message. Works regardless of scroll position.
                    KeyCode::Char('y') => {
                        let idx = self
                            .app
                            .transcript
                            .iter()
                            .rposition(|i| matches!(i, TranscriptItem::Assistant { .. }));
                        if let Some(idx) = idx {
                            return Some(TuiAction::CopyItem(idx));
                        }
                    }
                    KeyCode::Char('Y') => {
                        let idx = self
                            .app
                            .transcript
                            .iter()
                            .rposition(|i| matches!(i, TranscriptItem::User { .. }));
                        if let Some(idx) = idx {
                            return Some(TuiAction::CopyItem(idx));
                        }
                    }
                    // Any other printable character returns focus to input
                    // and is injected there, so the user doesn't have to
                    // press Ctrl+T before typing.
                    KeyCode::Char(c) => {
                        self.focus = Focus::Input;
                        self.input.insert_char(c);
                        return None;
                    }
                    _ => {}
                }
                None
            }
            Focus::Input => self.handle_input_key(key),
        }
    }


    /// Refresh both completion popups after an input edit: the slash popup
    /// (command / subcommand stages) and, for non-slash input, the @-context
    /// / path completion popup (host-computed via the shared engine).
    fn refresh_completion_menus(&mut self) {
        let text = self.input.text();
        self.completion_suppressed = false; // a real edit re-enables the popup
        self.app.update_slash_menu(&text);
        // Completion popup only for non-slash, single-word-tail input.
        let first_line = text.lines().next().unwrap_or("");
        if first_line.starts_with('/') {
            self.app.set_completion_items(Vec::new());
            return;
        }
        let head = &first_line[..]; // cursor assumed at end for popup purposes
        if let Some(word) = joey_tools::completion::extract_context_word(head) {
            let files = self.completion_engine.project_files_stale_ok(&self.completion_cwd);
            let items = self.completion_engine.context_completions(&word, &self.completion_cwd, &files, 12);
            self.app.set_completion_items(items);
        } else if let Some(word) = joey_tools::completion::extract_path_word(head) {
            let items = joey_tools::completion::path_completions(&word, 12);
            self.app.set_completion_items(items);
        } else {
            self.app.set_completion_items(Vec::new());
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> Option<TuiAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        // Completion popup (@/path words, host-fed): navigation first.
        if self.app.completion_menu_open {
            match key.code {
                KeyCode::Down | KeyCode::Tab => {
                    self.app.completion_menu_move(true);
                    return None;
                }
                KeyCode::Up | KeyCode::BackTab => {
                    self.app.completion_menu_move(false);
                    return None;
                }
                KeyCode::Enter => {
                    // Accept the selected completion: replace the word under
                    // the cursor with the replacement text.
                    if let Some(repl) = self.app.completion_selected() {
                        self.input.delete_word_back();
                        self.input.insert_str(&repl);
                        self.app.set_completion_items(Vec::new());
                        // Accepted — close the completion popup and keep it
                        // closed until the next edit (upstream semantics:
                        // accepting a completion dismisses the menu).
                        self.completion_suppressed = true;
                        self.app.update_slash_menu(&self.input.text());
                    }
                    return None;
                }
                KeyCode::Esc => {
                    self.app.set_completion_items(Vec::new());
                    return None;
                }
                _ => {} // fall through to normal editing (re-filters on change)
            }
        }
        // Slash popup is open: navigation keys act on the menu first.
        if self.app.slash_menu_open {
            let sub_stage = self.app.slash_subcommand_stage;
            match key.code {
                KeyCode::Down | KeyCode::Tab => {
                    if sub_stage {
                        let len = self.app.slash_subcommand_matches(&self.input.text()).len();
                        if len > 0 {
                            self.app.slash_menu_cursor = (self.app.slash_menu_cursor + 1) % len;
                        }
                    } else {
                        self.app.slash_menu_move(&self.input.text(), true);
                    }
                    return None;
                }
                KeyCode::Up | KeyCode::BackTab => {
                    if sub_stage {
                        let len = self.app.slash_subcommand_matches(&self.input.text()).len();
                        if len > 0 {
                            self.app.slash_menu_cursor = (self.app.slash_menu_cursor + len - 1) % len;
                        }
                    } else {
                        self.app.slash_menu_move(&self.input.text(), false);
                    }
                    return None;
                }
                KeyCode::Enter => {
                    if self.app.slash_subcommand_stage {
                        // Accept the selected subcommand: replace the arg word.
                        let text = self.input.text();
                        let subs = self.app.slash_subcommand_matches(&text);
                        if let Some(sel) = subs.get(self.app.slash_menu_cursor) {
                            let first_line = text.lines().next().unwrap_or("");
                            let (base, _arg) = first_line.split_once(' ').unwrap_or((first_line, ""));
                            self.input.set_text(&format!("{base} {sel}"));
                            self.app.slash_menu_cursor = 0;
                            self.refresh_completion_menus();
                        }
                        return None;
                    }
                    // Exact command already typed (e.g. "/quit", "/help"):
                    // submitting is what the user means — the popup's only
                    // remaining value is argument hints, and re-offering
                    // itself on every Enter would trap the input.
                    let first_line = self.input.text().lines().next().unwrap_or("").to_string();
                    let exact = first_line.starts_with('/')
                        && !first_line.contains(' ')
                        && self
                            .app
                            .slash_commands
                            .iter()
                            .any(|c| c.name == first_line[1..] || c.aliases.iter().any(|a| *a == &first_line[1..]));
                    if exact {
                        self.app.slash_menu_open = false;
                        let text = self.input.text();
                        if !text.trim().is_empty() {
                            self.app.history_record(&text);
                            self.input.clear();
                            return Some(TuiAction::Submit(text));
                        }
                        return None;
                    }
                    // Accept the selected command into the input box.
                    if let Some(name) = self.app.slash_selected(&self.input.text()) {
                        self.input.set_text(&format!("/{}", name));
                        self.app.slash_menu_cursor = 0;
                        self.refresh_completion_menus();
                    }
                    return None;
                }
                KeyCode::Esc => {
                    self.app.slash_menu_open = false;
                    return None;
                }
                _ => {} // fall through to normal editing
            }
        }
        match key.code {
            KeyCode::Enter if alt => {
                self.input.insert_newline();
                None
            }
            KeyCode::Char('j') if ctrl => {
                self.input.insert_newline();
                None
            }
            KeyCode::Enter => {
                let text = self.input.text();
                if !text.trim().is_empty() {
                    // Record into history before clearing (the host also
                    // persists to the shared history file on Submit).
                    self.app.history_record(&text);
                    self.input.clear();
                    self.app.slash_menu_open = false;
                    // The host records/queues; while busy this becomes a
                    // queued prompt for the next turn.
                    return Some(TuiAction::Submit(text));
                }
                None
            }
            KeyCode::Char('h') if ctrl => {
                self.input.backspace();
                self.refresh_completion_menus();
                None
            }
            KeyCode::Char('a') if ctrl => {
                self.input.move_line_start();
                None
            }
            KeyCode::Char('e') if ctrl => {
                self.input.move_line_end();
                None
            }
            KeyCode::Char('b') if ctrl => {
                self.input.move_left();
                None
            }
            KeyCode::Char('f') if ctrl => {
                self.input.move_right();
                None
            }
            KeyCode::Char('k') if ctrl => {
                self.input.kill_to_end();
                self.refresh_completion_menus();
                None
            }
            KeyCode::Char('u') if ctrl => {
                self.input.kill_to_start();
                self.refresh_completion_menus();
                None
            }
            KeyCode::Char('w') if ctrl => {
                self.input.delete_word_back();
                self.refresh_completion_menus();
                None
            }
            KeyCode::Char('b') if alt => {
                self.input.move_word_left();
                None
            }
            KeyCode::Char('f') if alt => {
                self.input.move_word_right();
                None
            }
            KeyCode::Backspace if alt => {
                self.input.delete_word_back();
                self.refresh_completion_menus();
                None
            }
            KeyCode::Left if ctrl => {
                self.input.move_word_left();
                None
            }
            KeyCode::Right if ctrl => {
                self.input.move_word_right();
                None
            }
            KeyCode::Left => {
                self.input.move_left();
                None
            }
            KeyCode::Right => {
                self.input.move_right();
                None
            }
            // History recall on a draft boundary; cursor movement inside a
            // multi-line draft. While the slash popup is open these keys are
            // consumed above (menu navigation). Single-line Up is also caught
            // by the early handler above; this covers multi-line drafts:
            // Up at the first line recalls older history, Down at the last
            // line recalls newer (readline/reedline semantics).
            KeyCode::Up => {
                let (cursor_line, _) = self.input.cursor();
                if cursor_line > 0 {
                    self.input.move_up();
                } else if let Some(text) = self.app.history_prev(&self.input.text()) {
                    self.input.set_text(&text);
                    self.refresh_completion_menus();
                }
                None
            }
            KeyCode::Down => {
                let (cursor_line, _) = self.input.cursor();
                if cursor_line + 1 < self.input.line_count() {
                    self.input.move_down();
                } else if let Some(text) = self.app.history_next() {
                    self.input.set_text(&text);
                    self.refresh_completion_menus();
                }
                None
            }
            KeyCode::Home => {
                self.input.move_line_start();
                None
            }
            KeyCode::End => {
                self.input.move_line_end();
                None
            }
            KeyCode::Backspace => {
                self.input.backspace();
                self.refresh_completion_menus();
                None
            }
            KeyCode::Delete => {
                self.input.delete();
                self.refresh_completion_menus();
                None
            }
            KeyCode::Char('s') if ctrl => {
                // Ctrl+S opens transcript search (the `/` key in input mode
                // now opens the slash-command popup instead).
                self.app.search_open = true;
                self.app.search_query.clear();
                None
            }
            KeyCode::Char('?') if self.input.is_empty() && !ctrl => {
                self.show_help = true;
                None
            }
            // '/' on an empty input opens the slash-command popup; a second
            // '/' (already typed) is just a character.
            KeyCode::Char('/') if self.input.is_empty() && !ctrl => {
                self.input.insert_char('/');
                self.refresh_completion_menus();
                None
            }
            KeyCode::Char(c) if !ctrl => {
                self.input.insert_char(c);
                self.refresh_completion_menus();
                None
            }
            _ => None,
        }
    }

    /// Handle keys in the search bar.
    fn handle_search_key(&mut self, key: KeyEvent) -> Option<TuiAction> {
        match key.code {
            KeyCode::Esc => {
                self.app.search_open = false;
                self.app.search_query.clear();
                None
            }
            KeyCode::Enter => {
                // Confirm search and jump to first match.
                self.app.run_search();
                // Keep search open so n/N can cycle.
                None
            }
            KeyCode::Backspace => {
                self.app.search_query.pop();
                self.app.run_search();
                None
            }
            KeyCode::Char('n') => {
                self.app.search_next(true);
                None
            }
            KeyCode::Char('N') => {
                self.app.search_next(false);
                None
            }
            KeyCode::Char(c) => {
                self.app.search_query.push(c);
                self.app.run_search();
                None
            }
            _ => None,
        }
    }

    /// Handle a mouse event for scroll wheel support.
    ///
    /// Call this from the host when a MouseEvent is received. Enables mouse
    /// wheel scrolling in the transcript area. When the pointer is over the
    /// NeuroCode context panel (docked or expanded), the wheel scrolls the
    /// feed instead of the transcript.
    pub fn handle_mouse_scroll(&mut self, row: u16, col: u16, delta_up: bool) {
        if self.neurocode_panel_hit(row, col) {
            // Wheel over the context feed: scroll the feed itself.
            if delta_up {
                self.app.neurocode_scroll = self.app.neurocode_scroll.saturating_add(3);
            } else {
                self.app.neurocode_scroll = self.app.neurocode_scroll.saturating_sub(3);
            }
            return;
        }
        if delta_up {
            self.app.scroll_up(3);
            if self.focus == Focus::Input {
                self.focus = Focus::Transcript;
            }
        } else {
            self.app.scroll_down(3);
            if self.app.scroll.is_none() {
                self.focus = Focus::Input;
            }
        }
    }

    /// True when `(row, col)` falls inside the NeuroCode context panel as
    /// drawn by the last frame (docked bottom-right or expanded main view).
    fn neurocode_panel_hit(&self, row: u16, col: u16) -> bool {
        if !self.app.neurocode_active {
            return false;
        }
        let (x, y, w, h) = self.app.last_neurocode_rect.get();
        w > 0 && h > 0 && row >= y && row < y + h && col >= x && col < x + w
    }

    /// Feature 007 (T026): handle a left-click on the transcript. Uses
    /// per-item hit-testing (`transcript_hit_test`) to resolve the clicked
    /// row to a transcript item index, then focuses the transcript and toggles
    /// that item's expand state. Clicks outside the text area or on
    /// non-expandable items are no-ops (focus still switches to Transcript).
    ///
    /// Clicking the NeuroCode context feed (docked bottom-right panel or the
    /// expanded main-screen view) toggles it between the two — the content
    /// moves onto the main screen, or docks back to its previous state.
    pub fn handle_mouse_click(&mut self, row: u16, col: u16) {
        // NeuroCode feed first: a click inside the panel (any part of it,
        // including borders) toggles docked ↔ expanded and is consumed.
        if self.neurocode_panel_hit(row, col) {
            self.app.toggle_neurocode_expanded();
            return;
        }
        // Focus the transcript on any click within it.
        if self.focus == Focus::Input {
            self.focus = Focus::Transcript;
        }
        // Resolve the clicked item via per-item hit-testing, then toggle.
        if let Some(item_idx) = widgets::transcript_hit_test(&self.app, self.theme, row, col) {
            self.app.toggle_item_expand_by_index(item_idx);
        }
    }
}

impl<B: ratatui::backend::Backend> Drop for Tui<B> {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}

#[cfg(test)]
mod key_tests {
    //! End-to-end key-handling tests for input-history recall (CLI parity:
    //! Up/Down walk the shared ~/.joey/.joey_history exactly like reedline
    //! does in the line REPL). Drives Tui::handle_key against a
    //! ratatui TestBackend — no real TTY needed.
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, KeyEventKind, KeyEventState};
    use ratatui::backend::TestBackend;

    fn tui_with_history(entries: &[&str]) -> Tui<TestBackend> {
        let mut app = App::new("s", "m");
        for e in entries {
            app.history_record(e);
        }
        let terminal = ratatui::Terminal::new(TestBackend::new(80, 24)).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    fn up() -> KeyEvent {
        KeyEvent { code: KeyCode::Up, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn down() -> KeyEvent {
        KeyEvent { code: KeyCode::Down, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn shift_up() -> KeyEvent {
        KeyEvent { code: KeyCode::Up, modifiers: KeyModifiers::SHIFT, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    fn char_evt(c: char) -> KeyEvent {
        KeyEvent { code: KeyCode::Char(c), modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    #[test]
    fn up_recalls_newest_then_older_history() {
        let mut t = tui_with_history(&["one", "two", "three"]);
        // First Up: newest entry.
        t.handle_key(up());
        assert_eq!(t.input.text(), "three");
        // Second Up: older.
        t.handle_key(up());
        assert_eq!(t.input.text(), "two");
        // Third Up: oldest.
        t.handle_key(up());
        assert_eq!(t.input.text(), "one");
        // Fourth Up: stays at oldest (no wraparound).
        t.handle_key(up());
        assert_eq!(t.input.text(), "one");
    }

    #[test]
    fn down_after_up_walks_back_and_restores_draft() {
        let mut t = tui_with_history(&["one", "two"]);
        t.handle_key(char_evt('d'));
        t.handle_key(char_evt('r'));
        t.handle_key(char_evt('a'));
        t.handle_key(char_evt('f'));
        t.handle_key(char_evt('t'));
        assert_eq!(t.input.text(), "draft");
        // Up: newest, draft saved.
        t.handle_key(up());
        assert_eq!(t.input.text(), "two");
        t.handle_key(up());
        assert_eq!(t.input.text(), "one");
        // Down: back toward newer.
        t.handle_key(down());
        assert_eq!(t.input.text(), "two");
        // Down past newest: restores the in-progress draft.
        t.handle_key(down());
        assert_eq!(t.input.text(), "draft");
        // Recall state reset: next Up starts from newest again.
        t.handle_key(up());
        assert_eq!(t.input.text(), "two");
    }

    #[test]
    fn editing_a_recalled_entry_then_down_restores_original_draft() {
        // bash/reedline parity: edits made to a recalled history entry are
        // NOT kept as the draft — Down past the newest entry restores the
        // text that was in the box before the first Up.
        let mut t = tui_with_history(&["alpha", "beta"]);
        t.handle_key(char_evt('w'));
        t.handle_key(char_evt('i'));
        t.handle_key(char_evt('p'));
        assert_eq!(t.input.text(), "wip");
        t.handle_key(up()); // "beta", draft saved = "wip"
        assert_eq!(t.input.text(), "beta");
        // Type a suffix onto the recalled entry.
        t.handle_key(char_evt('-'));
        t.handle_key(char_evt('x'));
        assert_eq!(t.input.text(), "beta-x");
        // Down past the newest → original draft restored (edit discarded,
        // same as bash).
        t.handle_key(down());
        assert_eq!(t.input.text(), "wip");
    }

    #[test]
    fn empty_history_up_is_noop() {
        let mut t = tui_with_history(&[]);
        t.handle_key(char_evt('x'));
        t.handle_key(up());
        assert_eq!(t.input.text(), "x", "no history → input untouched");
    }

    #[test]
    fn multiline_draft_up_moves_cursor_not_history() {
        let mut t = tui_with_history(&["one"]);
        t.handle_key(char_evt('a'));
        t.handle_key(KeyEvent { code: KeyCode::Enter, modifiers: KeyModifiers::ALT, kind: KeyEventKind::Press, state: KeyEventState::NONE });
        t.handle_key(char_evt('b'));
        assert_eq!(t.input.text(), "a\nb");
        // Cursor on line 2 → Up moves within the draft.
        t.handle_key(up());
        assert_eq!(t.input.cursor().0, 0);
        // Cursor now on line 1 (index 0) → another Up recalls history.
        t.handle_key(up());
        assert_eq!(t.input.text(), "one");
    }

    #[test]
    fn submit_records_and_resets_recall() {
        let mut t = tui_with_history(&["one"]);
        // Recall and edit.
        t.handle_key(up());
        t.handle_key(char_evt('!'));
        assert_eq!(t.input.text(), "one!");
        // Submit.
        let action = t.handle_key(KeyEvent { code: KeyCode::Enter, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE });
        assert!(matches!(action, Some(TuiAction::Submit(text)) if text == "one!"));
        assert!(t.input.is_empty());
        // Next Up starts from the newest ("one!").
        t.handle_key(up());
        assert_eq!(t.input.text(), "one!");
    }

    #[test]
    fn shift_up_still_switches_to_transcript_focus() {
        let mut t = tui_with_history(&["one", "two"]);
        t.handle_key(shift_up());
        // Focus moved to transcript; input not touched by history recall.
        assert_eq!(t.input.text(), "");
        // j/k scrolling now; typing 'g' shouldn't insert into input.
        t.handle_key(char_evt('g'));
        assert!(t.input.is_empty());
    }
}

#[cfg(test)]
mod completion_key_tests {
    //! End-to-end key tests for smart completions: subcommand popup stage
    //! and the @-context completion popup, driven through handle_key on a
    //! TestBackend (no real TTY).
    use super::*;
    use crate::state::SlashCommandInfo;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::backend::TestBackend;

    fn cmd(name: &str, hint: &str) -> SlashCommandInfo {
        SlashCommandInfo {
            name: name.to_string(),
            aliases: vec![],
            description: format!("{name} command"),
            args_hint: hint.to_string(),
            implemented: true,
        }
    }

    fn tui() -> Tui<TestBackend> {
        let mut app = App::new("s", "m");
        app.slash_commands = vec![
            cmd("timestamps", "[on|off|status]"),
            cmd("model", "[model] [--global]"),
        ];
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    fn k(c: char) -> KeyEvent {
        KeyEvent { code: KeyCode::Char(c), modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }
    fn enter() -> KeyEvent {
        KeyEvent { code: KeyCode::Enter, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }
    fn down() -> KeyEvent {
        KeyEvent { code: KeyCode::Down, modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }

    #[test]
    fn subcommand_popup_flow_type_and_accept() {
        let mut t = tui();
        for c in "/timestamps o".chars() {
            t.handle_key(k(c));
        }
        // Popup is open in subcommand stage offering on/off.
        assert!(t.app.slash_menu_open);
        assert!(t.app.slash_subcommand_stage);
        let subs = t.app.slash_subcommand_matches(&t.input.text());
        assert_eq!(subs, vec!["on".to_string(), "off".to_string()]);
        // Enter accepts "on".
        t.handle_key(enter());
        assert_eq!(t.input.text(), "/timestamps on");
        // Popup closed (exact subcommand typed).
        assert!(!t.app.slash_menu_open);
    }

    #[test]
    fn subcommand_popup_down_then_enter_accepts_second() {
        let mut t = tui();
        for c in "/timestamps ".chars() {
            t.handle_key(k(c));
        }
        assert!(t.app.slash_menu_open);
        t.handle_key(down());
        t.handle_key(enter());
        assert_eq!(t.input.text(), "/timestamps off");
    }

    #[test]
    fn command_popup_flow_still_works() {
        let mut t = tui();
        for c in "/tim".chars() {
            t.handle_key(k(c));
        }
        assert!(t.app.slash_menu_open);
        assert!(!t.app.slash_subcommand_stage);
        t.handle_key(enter());
        assert_eq!(t.input.text(), "/timestamps");
    }

    #[test]
    fn at_context_popup_offers_static_refs() {
        let mut t = tui();
        for c in "look @".chars() {
            t.handle_key(k(c));
        }
        assert!(t.app.completion_menu_open);
        let repls: Vec<&str> = t.app.completion_items.iter().map(|i| i.replacement.as_str()).collect();
        assert!(repls.contains(&"@diff"), "refs: {repls:?}");
        assert!(repls.contains(&"@url:"));
        // Enter accepts the first (@diff).
        t.handle_key(enter());
        assert_eq!(t.input.text(), "look @diff");
        assert!(!t.app.completion_menu_open);
    }

    #[test]
    fn path_completion_popup_flow() {
        // Type a path-like word in a temp dir with known files.
        let dir = std::env::temp_dir().join("joey_tui_path_completion");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("zebra_test.txt"), "x").unwrap();

        let mut t = tui();
        t.completion_cwd = dir.clone();
        // Path-like word (contains '/'); leading "see " keeps the line
        // non-slash so the path stage triggers (upstream parity: a line
        // starting with '/' is a command).
        let word = format!("see {}/z", dir.to_string_lossy());
        for c in word.chars() {
            t.handle_key(k(c));
        }
        assert!(t.app.completion_menu_open, "path popup should open");
        let repls: Vec<&str> = t.app.completion_items.iter().map(|i| i.replacement.as_str()).collect();
        assert!(repls.iter().any(|r| r.contains("zebra_test.txt")), "got {repls:?}");
        t.handle_key(enter());
        assert!(t.input.text().contains("zebra_test.txt"));
        assert!(!t.app.completion_menu_open);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plain_text_no_popup() {
        let mut t = tui();
        for c in "hello world".chars() {
            t.handle_key(k(c));
        }
        assert!(!t.app.completion_menu_open);
        assert!(!t.app.slash_menu_open);
    }
}

#[cfg(test)]
mod expand_tests {
    //! Keyboard + render expansion of tool/terminal/diff items.
    use super::*;
    use crate::state::{ToolStatus, TranscriptItem};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::backend::TestBackend;

    fn tui_with_tool(full_result: &str, is_terminal: bool) -> Tui<TestBackend> {
        let mut app = App::new("s", "m");
        app.push_item(TranscriptItem::Tool {
            name: if is_terminal { "terminal".into() } else { "read_file".into() },
            emoji: "💻".into(),
            summary: if is_terminal { "seq 1 300".into() } else { "path=/tmp/x".into() },
            status: ToolStatus::Done,
            duration_secs: Some(0.5),
            result_preview: "1\n2\n3".into(),
            expanded: false,
            full_args: None,
            full_result: Some(full_result.to_string()),
            is_terminal,
            exit_code: if is_terminal { Some(0) } else { None },
        });
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        // Simulate the first frame recording the text-area geometry.
        let theme = Theme::aurora();
        let mut t = Tui::new_for_test(app, theme, terminal);
        // (geometry recorded by new_for_test's initial size)
        t
    }


    fn space() -> KeyEvent {
        KeyEvent { code: KeyCode::Char(' '), modifiers: KeyModifiers::NONE, kind: KeyEventKind::Press, state: KeyEventState::NONE }
    }


    #[test]
    fn space_in_transcript_focus_toggles_top_tool_item() {
        let mut t = tui_with_tool("full output line 1\nfull output line 2", false);
        // Geometry is recorded by a real draw; set it directly (established
        // test pattern — see widgets hit-test tests).
        t.app.last_text_area.set((0, 0, 98, 28));
        // Enter transcript focus (Shift+Up), then Space toggles the top item.
        t.handle_key(KeyEvent { code: KeyCode::Up, modifiers: KeyModifiers::SHIFT, kind: KeyEventKind::Press, state: KeyEventState::NONE });
        let expanded_before = matches!(
            t.app.transcript.back(),
            Some(TranscriptItem::Tool { expanded: true, .. })
        );
        assert!(!expanded_before);
        t.handle_key(space());
        let expanded_after = matches!(
            t.app.transcript.back(),
            Some(TranscriptItem::Tool { expanded: true, .. })
        );
        assert!(expanded_after, "Space toggled the top tool item");
        // Toggle back.
        t.handle_key(space());
        let collapsed = matches!(
            t.app.transcript.back(),
            Some(TranscriptItem::Tool { expanded: false, .. })
        );
        assert!(collapsed);
    }

    /// Render the transcript through the real widget into a TestBackend
    /// and return the joined buffer text (smoke-test pattern).
    fn render_transcript(app: &crate::state::App, theme: crate::theme::Theme) -> String {
        use ratatui::Terminal;
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                crate::widgets::draw_transcript(f, area, app, theme, false, 0.5);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect()
    }

    #[test]
    fn terminal_expanded_view_shows_full_result_not_preview() {
        let full = (1..=300).map(|i| format!("line-{i}")).collect::<Vec<_>>().join("\n");
        let mut app = App::new("s", "m");
        app.push_item(TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "seq 1 300".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.5),
            result_preview: "1\n2\n3".into(),
            expanded: true,
            full_args: None,
            full_result: Some(full),
            is_terminal: true,
            exit_code: Some(0),
        });
        let text = render_transcript(&app, crate::theme::Theme::aurora());
        // The viewport shows the tail of the expanded block (which contains
        // the FULL result — far more than the 3-line preview ever had).
        assert!(text.contains("line-300"), "expanded terminal shows full result tail");
    }

    #[test]
    fn top_item_resolver_finds_the_tool_when_scrolled_to_it() {
        let full = (1..=300).map(|i| format!("line-{i}")).collect::<Vec<_>>().join("\n");
        let mut app = App::new("s".to_string(), "m".to_string());
        app.record_user("run: seq 1 300");
        app.push_item(TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "seq 1 300".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.5),
            result_preview: "1\n2\n3".into(),
            expanded: false,
            full_args: None,
            full_result: Some(full),
            is_terminal: true,
            exit_code: Some(0),
        });
        app.last_text_area.set((0, 0, 98, 28));
        // Live mode (scroll None, bottom-anchored): the whole transcript
        // fits, so the TOP item is the user message (index 0) — correct.
        let idx = crate::widgets::transcript_item_at_top(&app, crate::theme::Theme::aurora());
        assert_eq!(idx, Some(0), "top item is the user message, got {idx:?}");
        // The Space handler's fallback then toggles the FIRST expandable
        // item at or below the top — the tool (index 1).
        assert!(app.item_is_expandable(1));
        assert!(!app.item_is_expandable(0));
    }

    #[test]
    fn terminal_collapsed_view_stays_bounded() {
        let full = (1..=300).map(|i| format!("line-{i}")).collect::<Vec<_>>().join("\n");
        let mut app = App::new("s".to_string(), "m".to_string());
        app.push_item(TranscriptItem::Tool {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "seq 1 300".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.5),
            result_preview: "1\n2\n3".into(),
            expanded: false,
            full_args: None,
            full_result: Some(full),
            is_terminal: true,
            exit_code: Some(0),
        });
        let text = render_transcript(&app, crate::theme::Theme::aurora());
        assert!(!text.contains("line-250"), "collapsed stays bounded");
    }

    #[test]
    fn file_diff_toggle_expands_hidden_lines() {
        let mut app = App::new("s", "m");
        let diff_lines: Vec<String> = (0..120).map(|i| format!("+line {i}")).collect();
        app.push_item(TranscriptItem::FileDiff {
            path: "big.rs".into(),
            stat: "+120 -0".into(),
            lines: diff_lines,
            is_binary: false,
            expanded: false,
        });
        // Collapsed hides early lines; expanded shows them.
        app.toggle_item_expand_by_index(0);
        if let Some(TranscriptItem::FileDiff { expanded, .. }) = app.transcript.back() {
            assert!(*expanded);
        } else {
            panic!("no diff item");
        }
    }
}

#[cfg(test)]
mod neurocode_expand_tests {
    //! Click-to-expand behavior for the NeuroCode context feed (docked
    //! bottom-right panel ↔ main-screen takeover). Drives the real draw
    //! layout through `Tui::draw` on a TestBackend so hit-testing rects are
    //! exactly what the user would see.
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use joey_agent_core::events::AgentEvent;
    use ratatui::backend::TestBackend;

    fn esc() -> KeyEvent {
        KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    /// Draw the REAL body layout (render_body) for an 80x30 terminal
    /// through a TestBackend, recording hit-test rects exactly as the
    /// production draw does. Mirrors Tui::draw's chunk math: header(2) +
    /// body(24) + input(3) + status(1) = 30.
    fn draw_body(t: &mut Tui<TestBackend>) {
        let area = Rect::new(0, 2, 80, 24);
        let spinner = crate::anim::Spinner::dots();
        let equalizer = crate::anim::Equalizer::new(28);
        t.terminal
            .draw(|f| {
                render_body(
                    f,
                    area,
                    &t.app,
                    t.theme,
                    false,
                    0.5,
                    &spinner,
                    &equalizer,
                );
            })
            .unwrap();
    }

    /// Build a Tui (80x30 — wide enough for the sidebar), NeuroCode active
    /// with a recognizable context blob, and a streaming tail in flight.
    fn tui_with_feed() -> Tui<TestBackend> {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::NeuroCodeActive { active: true });
        app.apply(AgentEvent::NeuroCodeContext {
            tier: "Frontier".into(),
            token_estimate: 4321,
            expanded_nodes: 12,
            cold_mode: false,
            formatted_context: "FEED_MARKER_ expand me".into(),
        });
        app.streaming_assistant = "STREAMING_MARKER_ still flowing".into();
        let terminal = ratatui::Terminal::new(TestBackend::new(80, 30)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);
        draw_body(&mut tui); // records last_neurocode_rect (docked)
        tui
    }

    fn buffer_text(t: &Tui<TestBackend>) -> String {
        t.terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect()
    }

    #[test]
    fn click_on_docked_panel_expands_to_main_screen() {
        let mut t = tui_with_feed();
        assert!(!t.app.neurocode_expanded, "starts docked");

        // Hit-test must resolve inside the docked panel first.
        let (x, y, w, h) = t.app.last_neurocode_rect.get();
        assert!(w > 0 && h > 0, "docked panel rect recorded by draw");
        t.handle_mouse_click(y + h / 2, x + w / 2);
        assert!(t.app.neurocode_expanded, "click expanded the feed");

        // Re-draw: the feed now renders in the MAIN area (left of the
        // sidebar), and live streaming is still visible.
        draw_body(&mut t);
        let text = buffer_text(&t);
        assert!(text.contains("FEED_MARKER_"), "feed content on main screen");
        assert!(
            text.contains("STREAMING_MARKER_"),
            "transcript streaming strip still live"
        );

        // The expanded rect should now be a wide main-area rect.
        let (ex, _ey, ew, _eh) = t.app.last_neurocode_rect.get();
        assert!(ew > w, "expanded feed is wider than the docked panel");
        assert!(ex <= x, "expanded feed starts at/left of the old panel");
    }

    #[test]
    fn second_click_docks_back() {
        let mut t = tui_with_feed();
        let (x, y, w, h) = t.app.last_neurocode_rect.get();
        // Expand…
        t.handle_mouse_click(y + h / 2, x + w / 2);
        assert!(t.app.neurocode_expanded);
        draw_body(&mut t);
        // …then click the EXPANDED panel anywhere to dock back.
        let (ex, ey, ew, eh) = t.app.last_neurocode_rect.get();
        t.handle_mouse_click(ey + eh / 2, ex + ew / 2);
        assert!(!t.app.neurocode_expanded, "second click docked the feed");
        // Rect reverts to the sidebar width class after redraw.
        draw_body(&mut t);
        let (nx, _ny, nw, _nh) = t.app.last_neurocode_rect.get();
        assert_eq!((nx, nw), (x, w), "docked rect restored");
    }

    #[test]
    fn esc_docks_expanded_feed() {
        let mut t = tui_with_feed();
        t.app.toggle_neurocode_expanded();
        assert!(t.app.neurocode_expanded);
        let _ = t.handle_key(esc());
        assert!(!t.app.neurocode_expanded, "Esc docks the feed");
    }

    #[test]
    fn click_outside_panel_is_untouched() {
        let mut t = tui_with_feed();
        // Click the far top-left of the transcript area (never the panel).
        t.handle_mouse_click(1, 1);
        assert!(!t.app.neurocode_expanded, "transcript click does not expand");
    }

    #[test]
    fn wheel_over_panel_scrolls_feed_not_transcript() {
        let mut t = tui_with_feed();
        let scroll_before = t.app.neurocode_scroll;
        let (x, y, w, h) = t.app.last_neurocode_rect.get();
        t.handle_mouse_scroll(y + h / 2, x + w / 2, true);
        assert_eq!(
            t.app.neurocode_scroll,
            scroll_before + 3,
            "wheel over feed scrolls the feed"
        );
        // Wheel far from the panel still scrolls the transcript (focus
        // moves off input — the established signal).
        t.focus = Focus::Input;
        t.handle_mouse_scroll(1, 1, true);
        assert!(
            !matches!(t.focus, Focus::Input),
            "wheel elsewhere hits transcript"
        );
    }

    #[test]
    fn deactivate_while_expanded_resets_state() {
        let mut t = tui_with_feed();
        t.app.toggle_neurocode_expanded();
        assert!(t.app.neurocode_expanded);
        t.app.apply(AgentEvent::NeuroCodeActive { active: false });
        assert!(!t.app.neurocode_expanded, "expanded reset on deactivate");
        assert_eq!(t.app.last_neurocode_rect.get(), (0, 0, 0, 0));
        // Click where the panel used to be is now a plain transcript click.
        t.handle_mouse_click(25, 70);
        assert!(!t.app.neurocode_expanded);
    }

    #[test]
    fn split_expanded_feed_bounds() {
        // Caller guards >= 12; check the split for a range of heights.
        assert_eq!(split_expanded_feed(12), (4, 8));
        assert_eq!(split_expanded_feed(20), (6, 14));
        assert_eq!(split_expanded_feed(40), (10, 30));
        // Transcript strip is clamped at 10 even for tall terminals.
        let (t40, _f40) = split_expanded_feed(60);
        assert_eq!(t40, 10);
    }
}
