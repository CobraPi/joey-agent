//! The TUI runtime: owns the terminal, the animation loop, and the bridge
//! between crossterm input / agent events and the rendered frame.
//!
//! Architecture (Elm-like, single source of truth):
//!   - [`App`] (`state.rs`) is the model.
//!   - [`Tui`] owns the terminal + animation timers.
//!   - The run loop polls: crossterm input events, agent events, and a fixed
//!     animation tick. On each, it updates the model and redraws.

use std::io::{self, Stdout};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Block;
use ratatui::Terminal;
use tokio::sync::mpsc;

use joey_agent_core::AgentEvent;

use crate::anim::{Activity, Clock, Equalizer, ParticleField, Pulse, Spinner};
use crate::input::Input;
use crate::state::{App, RunMode};
use crate::theme::Theme;
use crate::widgets;

/// A request emitted by the TUI to the host (the REPL) to act on user input.
#[derive(Debug)]
pub enum TuiAction {
    /// Submit this prompt to the agent.
    Submit(String),
    /// The user wants to interrupt the current turn.
    Interrupt,
    /// The user wants to quit the session.
    Quit,
}

pub type FrameBackend = CrosstermBackend<Stdout>;
pub type FrameTerminal = Terminal<FrameBackend>;

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
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input,
    Transcript,
}

impl Tui {
    /// Enter the alternate screen and create the terminal.
    pub fn enter(app: App, theme: Theme) -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
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
        })
    }

    /// Restore the terminal. Must be called on exit (including panics).
    pub fn leave(&mut self) -> io::Result<()> {
        disable_raw_mode()?;
        execute!(self.terminal.backend_mut(), LeaveAlternateScreen)?;
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

    /// Render one frame.
    fn draw_impl(&mut self) -> io::Result<()> {
        let theme = self.theme;
        let terminal = &mut self.terminal;

        terminal.draw(|f| {
            let area = f.area();
            // 1. Background fill (deep void).
            f.render_widget(Block::default().style(Style::default().bg(theme.bg_void.to_color())), area);
            // 2. Particle backdrop.
            widgets::draw_particles(f, &self.field, theme, area);
            // 3. Layout: header / body / input / status.
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2), // header
                    Constraint::Min(8),   // body
                    Constraint::Length(7), // input (multi-line)
                    Constraint::Length(1), // status
                ])
                .split(area);

            widgets::draw_header(f, chunks[0], &self.app, theme, &self.orbit_spinner, &self.pulse);

            // Body: transcript (left, large) + sidebar (right).
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Min(40),
                    Constraint::Length(34),
                ])
                .split(chunks[1]);

            // When reasoning is live, split transcript vertically: conversation + reasoning.
            if self.app.reasoning_open {
                let convo_split = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(4), Constraint::Length(8)])
                    .split(body[0]);
                widgets::draw_transcript(f, convo_split[0], &self.app, theme);
                widgets::draw_reasoning(f, convo_split[1], &self.app, theme, &self.spinner);
            } else {
                widgets::draw_transcript(f, body[0], &self.app, theme);
            }

            widgets::draw_activity(
                f,
                body[1],
                &self.app,
                theme,
                &self.spinner,
                &self.equalizer,
            );

            widgets::draw_input(f, chunks[2], &self.input, &self.app, theme);

            let elapsed = self
                .app
                .turn_started
                .map(|t| t.elapsed())
                .unwrap_or_default();
            widgets::draw_status(f, chunks[3], &self.app, theme, elapsed);

            if self.show_help {
                widgets::draw_help_overlay(f, area, theme);
            }
        })?;
        Ok(())
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

    /// Render one frame to the terminal.
    pub fn draw(&mut self) -> io::Result<()> {
        self.draw_impl()
    }

    /// Handle a single crossterm key event. Returns an action for the host.
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<TuiAction> {
        self.handle_key_impl(key)
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

    /// Handle a single crossterm key event. Returns an action for the host.
    fn handle_key_impl(&mut self, key: KeyEvent) -> Option<TuiAction> {
        if key.kind != KeyEventKind::Press {
            return None;
        }
        // Global keys.
        match (key.modifiers, key.code) {
            (m, KeyCode::Char('c')) if m.contains(KeyModifiers::CONTROL) => {
                if self.app.is_busy() {
                    return Some(TuiAction::Interrupt);
                }
                self.app.mode = RunMode::Quitting;
                return Some(TuiAction::Quit);
            }
            (_, KeyCode::Char('?')) => {
                self.show_help = !self.show_help;
                return None;
            }
            (_, KeyCode::Char('r')) | (_, KeyCode::Char('R')) => {
                self.app.show_reasoning = !self.app.show_reasoning;
                return None;
            }
            (_, KeyCode::Tab) => {
                self.focus = if self.focus == Focus::Input {
                    Focus::Transcript
                } else {
                    Focus::Input
                };
                return None;
            }
            (_, KeyCode::Esc) => {
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
                    KeyCode::PageUp => self.app.scroll_up(10),
                    KeyCode::PageDown => self.app.scroll_down(10),
                    _ => {}
                }
                None
            }
            Focus::Input => self.handle_input_key(key),
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) -> Option<TuiAction> {
        // If busy, most input is locked except interrupts (handled above).
        if self.app.is_busy() {
            return None;
        }
        match (key.modifiers, key.code) {
            (m, KeyCode::Enter) if m.contains(KeyModifiers::ALT) => {
                self.input.insert_newline();
                None
            }
            (_, KeyCode::Enter) => {
                let text = self.input.text();
                if !text.trim().is_empty() {
                    self.input.clear();
                    self.app.record_user(&text);
                    return Some(TuiAction::Submit(text));
                }
                None
            }
            (m, KeyCode::Char('j')) if m.contains(KeyModifiers::CONTROL) => {
                self.input.move_down();
                None
            }
            (m, KeyCode::Char('k')) if m.contains(KeyModifiers::CONTROL) => {
                self.input.move_up();
                None
            }
            (m, KeyCode::Char('h')) if m.contains(KeyModifiers::CONTROL) => {
                self.input.backspace();
                None
            }
            (m, KeyCode::Char('d')) if m.contains(KeyModifiers::CONTROL) => {
                self.input.delete();
                None
            }
            (m, KeyCode::Char('a')) if m.contains(KeyModifiers::CONTROL) => {
                self.input.move_line_start();
                None
            }
            (m, KeyCode::Char('e')) if m.contains(KeyModifiers::CONTROL) => {
                self.input.move_line_end();
                None
            }
            (m, KeyCode::Char('b')) if m.contains(KeyModifiers::CONTROL) => {
                self.input.move_left();
                None
            }
            (m, KeyCode::Char('f')) if m.contains(KeyModifiers::CONTROL) => {
                self.input.move_right();
                None
            }
            (m, KeyCode::Left) if m.contains(KeyModifiers::CONTROL) => {
                self.input.move_word_left();
                None
            }
            (m, KeyCode::Right) if m.contains(KeyModifiers::CONTROL) => {
                self.input.move_word_right();
                None
            }
            (_, KeyCode::Left) => { self.input.move_left(); None }
            (_, KeyCode::Right) => { self.input.move_right(); None }
            (_, KeyCode::Up) => { self.input.move_up(); None }
            (_, KeyCode::Down) => { self.input.move_down(); None }
            (_, KeyCode::Home) => { self.input.move_line_start(); None }
            (_, KeyCode::End) => { self.input.move_line_end(); None }
            (_, KeyCode::Backspace) => { self.input.backspace(); None }
            (_, KeyCode::Delete) => { self.input.delete(); None }
            (_, KeyCode::Char(c)) => { self.input.insert_char(c); None }
            _ => None,
        }
    }

    /// Main loop. Drains agent events, pumps input, and redraws at the
    /// activity-scaled frame rate. Returns the final assistant text.
    pub async fn run_turn(
        &mut self,
        prompt: &str,
        mut rx: mpsc::UnboundedReceiver<AgentEvent>,
    ) -> String {
        self.app.record_user(prompt);
        // Pump the animation loop until the turn completes.
        let frame_budget = Duration::from_millis(1000 / self.activity.target_fps().max(20) as u64);
        loop {
            // 1. Drain agent events.
            while let Ok(ev) = rx.try_recv() {
                self.app.apply(ev);
            }
            // 2. Check terminal events (resize only during turn).
            while event::poll(Duration::from_millis(0)).unwrap_or(false) {
                if let Ok(Event::Resize(w, h)) = event::read() {
                    let _ = self.terminal.resize(ratatui::layout::Rect::new(0, 0, w, h));
                    self.field.resize(w as usize, h as usize);
                } else if let Ok(Event::Key(k)) = event::read() {
                    if let Some(action) = self.handle_key(k) {
                        match action {
                            TuiAction::Interrupt => { /* host owns interrupt */ }
                            TuiAction::Quit => {
                                self.app.mode = RunMode::Quitting;
                                return self.app.last_final_text.clone();
                            }
                            TuiAction::Submit(_) => {}
                        }
                    }
                }
            }
            // 3. Tick + render.
            self.tick_animations();
            let _ = self.draw();

            // Termination.
            if !self.app.is_busy() && self.app.active_agents.is_empty() && self.app.streaming_assistant.is_empty() {
                // Turn done — one final flush of events.
                while let Ok(ev) = rx.try_recv() {
                    self.app.apply(ev);
                }
                let _ = self.draw();
                break;
            }
            // Sleep the remainder of the frame budget.
            tokio::time::sleep(frame_budget.min(Duration::from_millis(50))).await;
        }
        self.app.last_final_text.clone()
    }

    /// Drive the interactive REPL loop: read user input via the TUI, emit
    /// Submit actions, and let the host run the turn with its own event
    /// channel.
    pub async fn run_interactive<F, Fut>(&mut self, mut on_submit: F)
    where
        F: FnMut(String) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let frame_budget = Duration::from_millis(16);
        loop {
            // Pump any leftover agent events (defensive).
            // Input loop: poll events, tick animations, render.
            self.tick_animations();
            let _ = self.draw();
            if event::poll(frame_budget).unwrap_or(false) {
                match event::read() {
                    Ok(Event::Key(k)) => {
                        if let Some(action) = self.handle_key(k) {
                            match action {
                                TuiAction::Quit => {
                                    self.app.mode = RunMode::Quitting;
                                    let _ = self.draw();
                                    return;
                                }
                                TuiAction::Submit(text) => {
                                    on_submit(text).await;
                                }
                                _ => {}
                            }
                        }
                    }
                    Ok(Event::Resize(w, h)) => {
                        let _ = self.terminal.resize(ratatui::layout::Rect::new(0, 0, w, h));
                        self.field.resize(w as usize, h as usize);
                    }
                    _ => {}
                }
            }
        }
    }
}

// The helper `Rect` import is used in resize calls above.

/// One-shot render of the current state to stdout (for testing / headless).
pub fn render_once(app: &App, theme: Theme) -> io::Result<()> {
    let mut stdout = io::stdout();
    enable_raw_mode()?;
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut spinner = Spinner::dots();
    let mut eq = Equalizer::new(28);
    let mut field = ParticleField::new(80, 24);
    let mut pulse = Pulse::new();
    let mut activity = Activity::idle();
    let mut clock = Clock::start();
    let size = terminal.size()?;
    field.resize(size.width as usize, size.height as usize);
    // a few ticks for animation richness
    for _ in 0..5 {
        let dt = clock.dt();
        activity.update(app.active_count(), dt);
        spinner.tick(dt, activity.speed());
        eq.tick(dt, activity);
        field.tick(dt, activity, theme);
        pulse.tick(dt, activity);
    }
    terminal.draw(|f| {
        let area = f.area();
        f.render_widget(Block::default().style(Style::default().bg(theme.bg_void.to_color())), area);
        widgets::draw_particles(f, &field, theme, area);
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Min(8),
                Constraint::Length(7),
                Constraint::Length(1),
            ])
            .split(area);
        widgets::draw_header(f, chunks[0], app, theme, &spinner, &pulse);
        let body = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(40), Constraint::Length(34)])
            .split(chunks[1]);
        widgets::draw_transcript(f, body[0], app, theme);
        widgets::draw_activity(f, body[1], app, theme, &spinner, &eq);
    })?;
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    Ok(())
}

#[allow(dead_code)]
fn _unused_style_mod() {
    let _ = Modifier::BOLD;
}
