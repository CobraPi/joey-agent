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
    DisableBracketedPaste, EnableBracketedPaste, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
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
use ratatui::Terminal;

use crate::anim::{Activity, Clock, Equalizer, ParticleField, Pulse, Spinner};
use crate::input::Input;
use crate::state::{App, RunMode};
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
}

pub type FrameBackend = CrosstermBackend<Stdout>;
pub type FrameTerminal = Terminal<FrameBackend>;

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
    let _ = execute!(io::stdout(), DisableBracketedPaste, LeaveAlternateScreen);
}

/// The TUI controller.
pub struct Tui {
    pub app: App,
    pub theme: Theme,
    pub input: Input,
    terminal: FrameTerminal,
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    Transcript,
}

impl Tui {
    /// Enter the alternate screen and create the terminal.
    pub fn enter(app: App, theme: Theme) -> io::Result<Self> {
        install_panic_hook();
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(e) = execute!(stdout, EnterAlternateScreen, EnableBracketedPaste) {
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
        })
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
    pub fn draw(&mut self) -> io::Result<()> {
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

            // Body: transcript (left, large) + sidebar (right). The sidebar
            // yields entirely on narrow terminals.
            let show_sidebar = chunks[1].width >= 72;
            let body = if show_sidebar {
                Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Min(40), Constraint::Length(34)])
                    .split(chunks[1])
            } else {
                Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Min(1)])
                    .split(chunks[1])
            };

            let transcript_focused = *focus == Focus::Transcript;
            // When reasoning is live (and shown), split the transcript
            // vertically: conversation + reasoning.
            let show_reasoning_panel =
                app.reasoning_open && app.show_reasoning && body[0].height >= 14;
            if show_reasoning_panel {
                let convo_split = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(4), Constraint::Length(8)])
                    .split(body[0]);
                widgets::draw_transcript(f, convo_split[0], app, theme, transcript_focused, glow);
                widgets::draw_reasoning(f, convo_split[1], app, theme, spinner);
            } else {
                widgets::draw_transcript(f, body[0], app, theme, transcript_focused, glow);
            }

            if show_sidebar {
                widgets::draw_activity(f, body[1], app, theme, spinner, equalizer);
            }

            widgets::draw_input(f, chunks[2], input, app, theme, *focus == Focus::Input, glow);

            let elapsed = app.turn_started.map(|t| t.elapsed()).unwrap_or_default();
            widgets::draw_status(f, chunks[3], app, theme, elapsed);

            if *show_help {
                widgets::draw_help_overlay(f, area, theme);
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

        // Global keys.
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => {
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
            KeyCode::Char('l') if ctrl => {
                self.app.transcript.clear();
                self.app.scroll = None;
                return None;
            }
            KeyCode::F(1) => {
                self.show_help = true;
                return None;
            }
            KeyCode::Tab => {
                self.focus = if self.focus == Focus::Input {
                    Focus::Transcript
                } else {
                    Focus::Input
                };
                return None;
            }
            KeyCode::PageUp => {
                self.app.scroll_up(10);
                return None;
            }
            KeyCode::PageDown => {
                self.app.scroll_down(10);
                return None;
            }
            KeyCode::Esc => {
                if self.app.is_busy() {
                    return Some(TuiAction::Interrupt);
                }
                self.app.mode = RunMode::Quitting;
                return Some(TuiAction::Quit);
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
                    KeyCode::Char('?') => self.show_help = true,
                    KeyCode::Char('r') => self.toggle_reasoning(),
                    _ => {}
                }
                None
            }
            Focus::Input => self.handle_input_key(key),
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> Option<TuiAction> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
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
                    self.input.clear();
                    // The host records/queues; while busy this becomes a
                    // queued prompt for the next turn.
                    return Some(TuiAction::Submit(text));
                }
                None
            }
            KeyCode::Char('h') if ctrl => {
                self.input.backspace();
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
                None
            }
            KeyCode::Char('u') if ctrl => {
                self.input.kill_to_start();
                None
            }
            KeyCode::Char('w') if ctrl => {
                self.input.delete_word_back();
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
            // On a single-line prompt, ↑/↓ scroll the transcript (there is
            // nowhere for the cursor to go); in a multi-line draft they move
            // the cursor.
            KeyCode::Up => {
                if self.input.line_count() > 1 {
                    self.input.move_up();
                } else {
                    self.app.scroll_up(1);
                }
                None
            }
            KeyCode::Down => {
                if self.input.line_count() > 1 {
                    self.input.move_down();
                } else {
                    self.app.scroll_down(1);
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
                None
            }
            KeyCode::Delete => {
                self.input.delete();
                None
            }
            KeyCode::Char('?') if self.input.is_empty() && !ctrl => {
                self.show_help = true;
                None
            }
            KeyCode::Char(c) if !ctrl => {
                self.input.insert_char(c);
                None
            }
            _ => None,
        }
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.leave();
    }
}
