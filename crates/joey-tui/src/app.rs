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
use crate::state::{App, RunMode, TranscriptItem, NoticeKind};
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
    /// Main-transcript-only: the index addresses `App::transcript`.
    CopyItem(usize),
    /// Copy the text of pane `pane`'s transcript item `idx` (pane-relative)
    /// to the clipboard. T017 (D4): the `y`/`Y` keys with a subagent pane
    /// focused must resolve against the PANE transcript, never the main
    /// one — a bare `CopyItem` would copy the wrong text. Host owns the
    /// clipboard access.
    CopyPaneItem { pane: usize, idx: usize },
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
    // Parallel-subagent feature: the subagent tab rail occupies the RIGHT
    // edge whenever panes exist (each spawned child stacks a vertical tab
    // there; the orchestrator is the implicit leftmost tab = focus None).
    let show_rail = !app.subagent_panes.is_empty() && area.width >= 96;
    let with_rail_area = if show_rail {
        // Collapsed (default): the fixed 19-col tab strip — byte-for-byte
        // parity with the original layout.
        //
        // Expanded (Ctrl+N / title-click): widen to a 48-col detail rail,
        // but only while the transcript keeps a sane minimum: the rail
        // yields (clamps back toward collapsed) whenever the remaining
        // main area would drop below 60 cols. 48 was chosen in the middle
        // of the 40-56 range: wide enough for model + phase + last-tool
        // lines, narrow enough that the rail never dominates the body.
        let rail_w = if app.subagent_rail_expanded {
            let expanded_w = 48u16.min(area.width);
            if area.width.saturating_sub(expanded_w) >= 60 {
                expanded_w
            } else {
                19u16.min(area.width) // clamp: keep the transcript >= 60 cols
            }
        } else {
            19u16.min(area.width)
        };
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1), Constraint::Length(rail_w)])
            .split(area);
        widgets::draw_subagent_rail(f, cols[1], app, theme);
        cols[0]
    } else {
        app.last_subagent_tab_rects.borrow_mut().clear();
        app.last_orchestrator_tab_rect.set((0, 0, 0, 0));
        app.last_subagent_rail_title_rect.set((0, 0, 0, 0));
        area
    };

    // Body: transcript (left, large) + sidebar (right). The sidebar
    // yields entirely on narrow terminals.
    // T020 (US4, FR-006): it also yields while the focused pane's output
    // viewer is maximized — the shared viewer chrome ("… Ctrl+O or Esc to
    // restore") needs the full main-column width; the takeover is
    // transient (Ctrl+O/Esc restores the full pane view with sidebar).
    let pane_viewer_takeover = app.focused_pane().is_some()
        && app.output_viewer_open
        && with_rail_area.height >= 12;
    let show_sidebar = with_rail_area.width >= 72 && !pane_viewer_takeover;
    let body = if show_sidebar {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(40), Constraint::Length(34)])
            .split(with_rail_area)
    } else {
        Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(1)])
            .split(with_rail_area)
    };

    // When reasoning is live (and shown), split the transcript
    // vertically: conversation + reasoning.
    let show_reasoning_panel =
        app.reasoning_open && app.show_reasoning && body[0].height >= 14;
    // Reset the reasoning panel's hit-test rect for this frame; draw_reasoning
    // re-records it when (and only when) it actually renders. Frames that
    // skip the panel (output viewer takeover, NeuroCode takeover, reasoning
    // hidden/closed, short terminals) must not leave stale geometry catching
    // clicks.
    app.last_reasoning_rect.set((0, 0, 0, 0));

    // ── Pane-focused mode (parallel-subagent feature) ─────────────────
    // A focused subagent takes over the main transcript area; the
    // maximized panels (stats/output viewer) retarget to the child too.
    if let Some(pane) = app.focused_pane() {
        // T021 (US4, FR-008, D6): the pane's live reasoning panel mirrors
        // the main screen's docked strip — same `draw_reasoning` widget,
        // retargeted to the pane's `streaming_reasoning` (a non-empty
        // accumulator IS the live condition; the pane has no
        // `reasoning_open` flag). Hidden while reasoning is toggled off or
        // the area is too short — same guards as `show_reasoning_panel`.
        let show_pane_reasoning_panel =
            app.show_reasoning && !pane.streaming_reasoning.is_empty() && body[0].height >= 14;
        if app.stats_open && body[0].height >= 12 {
            let (transcript_h, stats_h) = split_expanded_feed(body[0].height);
            let main = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(transcript_h),
                    Constraint::Min(stats_h),
                ])
                .split(body[0]);
            widgets::draw_pane_transcript(f, main[0], app, pane, theme, focused, glow);
            widgets::draw_pane_stats_page(f, main[1], app, pane, theme, spinner);
        } else if pane_viewer_takeover {
            // T020 (US4, FR-006/FR-008, D6): the maximized output viewer
            // retargets to the focused pane — the SAME `draw_output_viewer`
            // chrome over the pane's tool output, with a transcript strip
            // kept on top (main-view layout parity), spanning the FULL main
            // column (`with_rail_area` — the sidebar yielded above).
            // Precedence mirrors the main branch: stats > viewer >
            // reasoning > explorer (T022 added the mode-attributed
            // explorer arm below — reachable only when the spawning
            // mode matches, FR-008).
            let (transcript_h, viewer_h) = split_expanded_feed(with_rail_area.height);
            let main = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(transcript_h),
                    Constraint::Min(viewer_h),
                ])
                .split(with_rail_area);
            widgets::draw_pane_transcript(f, main[0], app, pane, theme, focused, glow);
            widgets::draw_output_viewer(f, main[1], app, theme, spinner);
        } else if pane.reasoning_expanded && show_pane_reasoning_panel && body[0].height >= 12 {
            // T034 (US4, FR-008, D6): the pane's expanded live reasoning —
            // clicking the pane's docked strip (or Esc while expanded)
            // takes the pane's live stream over the pane view below a
            // transcript strip, mirroring the main screen's expanded
            // reasoning arm: same split_expanded_feed math, same shared
            // draw_reasoning widget (the only difference is the panel's
            // target-aware field source resolving the pane's state).
            let (transcript_h, reasoning_h) = split_expanded_feed(body[0].height);
            let main = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(transcript_h),
                    Constraint::Min(reasoning_h),
                ])
                .split(body[0]);
            widgets::draw_pane_transcript(f, main[0], app, pane, theme, focused, glow);
            widgets::draw_reasoning(f, main[1], app, theme, spinner);
        } else if show_pane_reasoning_panel {
            let convo_split = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(4), Constraint::Length(8)])
                .split(body[0]);
            widgets::draw_pane_transcript(f, convo_split[0], app, pane, theme, focused, glow);
            widgets::draw_reasoning(f, convo_split[1], app, theme, spinner);
        } else if app.neurocode_expanded
            && app.neurocode_active
            && pane.spawned_by_neurocode
            && body[0].height >= 12
        {
            // T022 (US4, FR-008, D2): mode-attributed explorer arm. The
            // pane reuses the orchestrator's `draw_explorer` (same widget
            // function, no pane-local reimplementation) but is reachable
            // ONLY when the spawning mode matches — `spawned_by_neurocode`
            // is snapshotted at SubagentSpawn, so a plain delegation pane
            // never shows the mode explorer even with both App flags set
            // (FR-008). Precedence per the pane branch: stats > viewer >
            // reasoning > explorer.
            let (transcript_h, feed_h) = split_expanded_feed(body[0].height);
            let main = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(transcript_h),
                    Constraint::Min(feed_h),
                ])
                .split(body[0]);
            widgets::draw_pane_transcript(f, main[0], app, pane, theme, focused, glow);
            crate::neurocode_viz::draw_explorer(f, main[1], app, theme);
        } else {
            widgets::draw_pane_transcript(f, body[0], app, pane, theme, focused, glow);
        }
    } else
    // Maximized takeovers of the main screen area — the transcript (with
    // its live streaming tail) keeps a strip at the top so the conversation
    // stays visible. Precedence: stats page (most explicit, opened from the
    // header) > output viewer > NeuroCode explorer > reasoning panel.
    if app.stats_open && body[0].height >= 12 {
        let (transcript_h, stats_h) = split_expanded_feed(body[0].height);
        let main = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(transcript_h),
                Constraint::Min(stats_h),
            ])
            .split(body[0]);
        widgets::draw_transcript(f, main[0], app, theme, focused, glow);
        widgets::draw_stats_page(f, main[1], app, theme, spinner);
    } else if app.output_viewer_open && body[0].height >= 12 {
        let (transcript_h, viewer_h) = split_expanded_feed(body[0].height);
        let main = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(transcript_h),
                Constraint::Min(viewer_h),
            ])
            .split(body[0]);
        widgets::draw_transcript(f, main[0], app, theme, focused, glow);
        widgets::draw_output_viewer(f, main[1], app, theme, spinner);
    } else if app.neurocode_expanded && app.neurocode_active && body[0].height >= 12 {
        let (transcript_h, feed_h) = split_expanded_feed(body[0].height);
        let main = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(transcript_h),
                Constraint::Min(feed_h),
            ])
            .split(body[0]);
        widgets::draw_transcript(f, main[0], app, theme, focused, glow);
        crate::neurocode_viz::draw_explorer(f, main[1], app, theme);
    } else if app.reasoning_expanded && show_reasoning_panel && body[0].height >= 12 {
        // Expanded reasoning (click the docked strip to toggle): the live
        // reasoning stream takes over the main screen, with a live
        // transcript strip kept at the top so assistant streaming stays
        // visible while thinking.
        let (transcript_h, reasoning_h) = split_expanded_feed(body[0].height);
        let main = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(transcript_h),
                Constraint::Min(reasoning_h),
            ])
            .split(body[0]);
        widgets::draw_transcript(f, main[0], app, theme, focused, glow);
        widgets::draw_reasoning(f, main[1], app, theme, spinner);
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

/// Test-only entry into `render_body` with fresh animation widgets — lets
/// integration tests (crates/joey-tui/tests/) drive the REAL body layout
/// (rail + panes + sidebar) against a TestBackend without constructing the
/// full `Tui`. Hidden from docs; not part of the public API surface.
#[doc(hidden)]
pub fn render_body_for_test(
    f: &mut Frame,
    area: Rect,
    app: &App,
    theme: Theme,
    focused: bool,
    glow: f32,
) {
    let spinner = crate::anim::Spinner::dots();
    let equalizer = crate::anim::Equalizer::new(28);
    render_body(f, area, app, theme, focused, glow, &spinner, &equalizer);
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
    /// Header gradient bar animator — the "agent running" indicator. Owned
    /// here so the busy→flow state survives across frames; drawn via
    /// `draw_header(..., Some(&self.header_flow))`.
    pub(crate) header_flow: crate::anim::HeaderFlow,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus {
    Input,
    Transcript,
}

/// T003 (research.md D1/D3): the view a transcript-targeted key acts on.
///
/// Resolved from `App.focused_subagent` in exactly ONE place
/// (`Tui::resolve_transcript_target`) so every transcript-targeted key
/// handler routes through a single point — no scattered
/// `focused_subagent.is_some()` checks. Deliberately a lightweight enum +
/// match (D1: no `TranscriptView` trait / view-object refactor).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TranscriptTarget {
    /// The orchestrator's main transcript (`App::transcript` / `App::scroll`).
    Main,
    /// The focused subagent pane's transcript (`SubagentPane`, by index).
    Pane(usize),
}

/// T037 (Phase 10, FR-008): does the NeuroCode explorer own KEY routing
/// right now? This is the exact predicate of the explorer arm at the top
/// of [`Tui::handle_key`], extracted as a free function so the routing
/// decision is assertable without a TTY. It mirrors the draw-side gate
/// (the pane explorer arm renders only when `pane.spawned_by_neurocode`),
/// keeping keys and pixels consistent:
///
/// - no pane focused (orchestrator view) → the explorer owns the keys,
///   byte-identical to the pre-T037 two-flag gate;
/// - a pane the NeuroCode mode spawned is focused → the explorer owns
///   the keys (it is drawn in that pane's view);
/// - any other (plain delegation) pane focused → the explorer owns
///   NOTHING: it is neither drawn NOR fed — keys fall through to normal
///   pane routing (scroll/expand/etc. via the transcript resolver).
///
/// A stale `focused_subagent` index (pane evicted) resolves to no pane,
/// matching the draw branch's `if let Some(pane) = app.focused_pane()`.
pub fn neurocode_explorer_owns_keys(app: &App) -> bool {
    app.neurocode_expanded
        && app.neurocode_active
        && app
            .focused_pane()
            .map_or(true, |pane| pane.spawned_by_neurocode)
}

/// T011 (US2, FR-003): pane-side counterpart of `App::item_is_expandable`
/// (same kinds: tool calls, file diffs, reasoning blocks). The pane
/// transcript lives on `SubagentPane`, which has no such helper, so the
/// Space/x Pane arm in `handle_key` uses this — keeping the kind list in
/// app.rs next to the routing instead of reaching into state.rs.
fn pane_item_is_expandable(item: &TranscriptItem) -> bool {
    matches!(
        item,
        TranscriptItem::Tool { .. } | TranscriptItem::FileDiff { .. } | TranscriptItem::Reasoning { .. }
    )
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
            header_flow: crate::anim::HeaderFlow::new(),
            show_help: false,
            focus: Focus::Input,
            restored: false,
            completion_engine: joey_tools::completion::CompletionEngine::new(),
            completion_cwd: std::env::current_dir().unwrap_or_default(),
            completion_suppressed: false,
        })
    }

    /// Re-enter the terminal after a `leave()` (e.g. returning from
    /// `$EDITOR` via `/prompt`): re-enables raw mode + the alternate screen
    /// and clears the `restored` latch. Complements idempotent `leave()`.
    pub fn enter_from_leave(&mut self) -> io::Result<()> {
        if !self.restored {
            return Ok(()); // never left
        }
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            EnableBracketedPaste,
            EnableMouseCapture
        )?;
        // Force a full repaint on the next draw (the screen was destroyed).
        self.terminal.clear()?;
        self.restored = false;
        Ok(())
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
            header_flow: crate::anim::HeaderFlow::new(),
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
        self.tick_animations_with_dt(dt);
    }

    /// `tick_animations` with an explicit dt — fixed-tick hosts and tests
    /// (the real `Clock` yields ~0 for back-to-back calls, which is correct
    /// for the frame loop but useless for deterministic test stepping).
    pub fn tick_animations_with_dt(&mut self, dt: Duration) {
        let target = self.target_agents();
        self.activity.update(target, dt);
        let speed = self.activity.speed();
        self.spinner.tick(dt, speed);
        self.orbit_spinner.tick(dt, speed);
        self.field.tick(dt, self.activity, self.theme);
        self.equalizer.tick(dt, self.activity);
        self.pulse.tick(dt, self.activity);
        // Header flow: the busy flag drives the eased envelope; the wave
        // pace rides the shared activity speed (faster with more agents).
        self.header_flow.set_busy(self.app.is_busy());
        self.header_flow.tick(dt, speed);
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
            header_flow,
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

            widgets::draw_header(f, chunks[0], app, theme, orbit_spinner, pulse, Some(header_flow));

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
            if app.show_status_bar {
                widgets::draw_status(f, chunks[3], app, theme, elapsed);
            }

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

    /// T003 (D3): THE single routing point for transcript-targeted keys.
    /// `focused_subagent == None` → [`TranscriptTarget::Main`] (orchestrator
    /// view, byte-identical to the pre-indirection behavior); a focused pane
    /// → [`TranscriptTarget::Pane`] with its index. Key handlers match on
    /// this instead of reading `focused_subagent` themselves, so target
    /// resolution has exactly one definition.
    fn resolve_transcript_target(&self) -> TranscriptTarget {
        match self.app.focused_subagent {
            Some(idx) => TranscriptTarget::Pane(idx),
            None => TranscriptTarget::Main,
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

        // NeuroCode fullscreen explorer: while expanded, navigation keys
        // (arrows/hjkl/Tab/Enter/zoom) drive the explorer instead of the
        // input/transcript. Esc (global handler below) docks it. Any key
        // the explorer doesn't claim falls through to normal handling.
        // T037 (FR-008): the explorer only owns the keys when it owns the
        // SCREEN — no pane focused, or the focused pane was spawned by
        // the NeuroCode mode (the draw-side gate's own condition, via
        // `neurocode_explorer_owns_keys`). A plain pane focused while the
        // flags are set keeps normal pane routing: not drawn, not fed.
        if neurocode_explorer_owns_keys(&self.app) {
            if crate::neurocode_viz::explorer_key(&mut self.app, &key) {
                return None;
            }
        }

        // Agent-stats page: while open, navigation keys scroll its context
        // stream (arrows/PgUp/PgDn/Home/End + hjkl/g/G in transcript
        // focus). Esc (global handler above) restores. Printable keys fall
        // through to the input box. When a subagent pane is focused, the
        // same keys drive the PANE's stats stream (retargeted view).
        if self.app.stats_open {
            let vim = self.focus == Focus::Transcript;
            // T003 (D3): route through the single resolver instead of a raw
            // `focused_subagent` check.
            let pane_focused =
                matches!(self.resolve_transcript_target(), TranscriptTarget::Pane(_));
            match key.code {
                KeyCode::Up => {
                    if pane_focused {
                        self.app.pane_stats_scroll_up(1);
                    } else {
                        self.app.stats_scroll_up(1);
                    }
                    return None;
                }
                KeyCode::Down => {
                    if pane_focused {
                        self.app.pane_stats_scroll_down(1);
                    } else {
                        self.app.stats_scroll_down(1);
                    }
                    return None;
                }
                KeyCode::PageUp => {
                    if pane_focused {
                        self.app.pane_stats_scroll_up(20);
                    } else {
                        self.app.stats_scroll_up(20);
                    }
                    return None;
                }
                KeyCode::PageDown => {
                    if pane_focused {
                        self.app.pane_stats_scroll_down(20);
                    } else {
                        self.app.stats_scroll_down(20);
                    }
                    return None;
                }
                KeyCode::Home => {
                    if pane_focused {
                        self.app.set_focused_pane_stats_view(Some(0));
                    } else {
                        self.app.stats_view = Some(0);
                    }
                    return None;
                }
                KeyCode::End => {
                    if pane_focused {
                        self.app.set_focused_pane_stats_view(None);
                    } else {
                        self.app.stats_view = None;
                    }
                    return None;
                }
                KeyCode::Char('k') if vim => {
                    if pane_focused {
                        self.app.pane_stats_scroll_up(1);
                    } else {
                        self.app.stats_scroll_up(1);
                    }
                    return None;
                }
                KeyCode::Char('j') if vim => {
                    if pane_focused {
                        self.app.pane_stats_scroll_down(1);
                    } else {
                        self.app.stats_scroll_down(1);
                    }
                    return None;
                }
                KeyCode::Char('g') if vim => {
                    if pane_focused {
                        self.app.set_focused_pane_stats_view(Some(0));
                    } else {
                        self.app.stats_view = Some(0);
                    }
                    return None;
                }
                KeyCode::Char('G') if vim => {
                    if pane_focused {
                        self.app.set_focused_pane_stats_view(None);
                    } else {
                        self.app.stats_view = None;
                    }
                    return None;
                }
                // Expandable-stats: Space/x toggles the context entry at the
                // center of the visible window (keyboard parity with clicks;
                // same resolution strategy as the transcript's Space).
                KeyCode::Char(' ') | KeyCode::Char('x') => {
                    let (inner_y, _start) = if pane_focused {
                        self.app.last_pane_stats_window.get()
                    } else {
                        self.app.last_stats_window.get()
                    };
                    let (x, y, w, h) = if pane_focused {
                        self.app.last_pane_stats_rect.get()
                    } else {
                        self.app.last_stats_rect.get()
                    };
                    let center_row = if h > 0 { y + h / 2 } else { inner_y };
                    let inside = w > 0
                        && h > 0
                        && center_row >= y
                        && center_row < y + h
                        && x > 0;
                    if inside {
                        let hit = if pane_focused {
                            self.app.pane_stats_context_entry_hit(center_row)
                        } else {
                            self.app.stats_context_entry_hit(center_row)
                        };
                        if let Some(entry_idx) = hit {
                            if pane_focused {
                                self.app.toggle_pane_context_entry(entry_idx);
                            } else {
                                self.app.toggle_context_entry(entry_idx);
                            }
                        }
                    }
                    return None;
                }
                _ => {}
            }
        }

        // Maximized terminal output viewer: while open, navigation keys
        // (arrows/PgUp/PgDn/Home/End, plus hjkl/g/G in transcript focus)
        // scroll the viewer's window instead of the input/transcript. Esc
        // (global handler above) restores. Printable keys fall through so
        // the user can keep typing into the input box while watching.
        if self.app.output_viewer_open {
            let vim = self.focus == Focus::Transcript;
            match key.code {
                KeyCode::Up => {
                    self.app.output_viewer_scroll_up(1);
                    return None;
                }
                KeyCode::Down => {
                    self.app.output_viewer_scroll_down(1);
                    return None;
                }
                KeyCode::PageUp => {
                    self.app.output_viewer_scroll_up(20);
                    return None;
                }
                KeyCode::PageDown => {
                    self.app.output_viewer_scroll_down(20);
                    return None;
                }
                KeyCode::Home => {
                    self.app.output_viewer_view = Some(0);
                    return None;
                }
                KeyCode::End => {
                    self.app.output_viewer_view = None;
                    return None;
                }
                KeyCode::Char('k') if vim => {
                    self.app.output_viewer_scroll_up(1);
                    return None;
                }
                KeyCode::Char('j') if vim => {
                    self.app.output_viewer_scroll_down(1);
                    return None;
                }
                KeyCode::Char('g') if vim => {
                    self.app.output_viewer_view = Some(0);
                    return None;
                }
                KeyCode::Char('G') if vim => {
                    self.app.output_viewer_view = None;
                    return None;
                }
                _ => {}
            }
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
                    self.close_search();
                    return None;
                }
                if self.app.agent_picker_open {
                    self.app.agent_picker_open = false;
                    return None;
                }
                // T034 (US4, FR-008, D6): a focused pane's EXPANDED
                // reasoning docks back before the pane itself releases
                // focus — mirroring the main view's Esc precedence where
                // the expanded reasoning panel collapses before any
                // lower-priority Esc behavior (one Esc = one surface
                // closed). No pane focused → this arm is never taken.
                if self
                    .app
                    .focused_pane()
                    .is_some_and(|p| p.reasoning_expanded)
                {
                    self.app.toggle_focused_pane_reasoning_expanded();
                    return None;
                }
                // Parallel-subagent feature: a focused pane releases the main
                // view back to the orchestrator first.
                if self.app.focused_subagent.is_some() {
                    self.app.focus_subagent(None);
                    return None;
                }
                // Close the agent-stats page before any lower-priority Esc
                // behavior.
                if self.app.stats_open {
                    self.app.close_stats();
                    return None;
                }
                // Restore from the maximized terminal output viewer before
                // any lower-priority Esc behavior.
                if self.app.output_viewer_open {
                    self.app.close_output_viewer();
                    return None;
                }
                // Dock the expanded NeuroCode feed back to its bottom-right
                // panel before any lower-priority Esc behavior.
                if self.app.neurocode_expanded {
                    self.app.toggle_neurocode_expanded();
                    return None;
                }
                // Collapse the expanded live reasoning panel back to its
                // docked bottom strip.
                if self.app.reasoning_expanded {
                    self.app.toggle_reasoning_expanded();
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
            // T012 (US2, FR-004): retargets to the FOCUSED pane. Like Ctrl+A
            // (T004), the App mutator reads `focused_subagent` itself, so the
            // arm stays target-agnostic — no scattered is_some() checks (the
            // resolver defines target resolution; the mutator owns the walk).
            KeyCode::Char('e') if ctrl => {
                self.app.cycle_focused_reasoning_expand();
                return None;
            }
            // Feature 005 (T028): Ctrl+G toggles the most-recent tool call's
            // expanded state (full args/result view).
            // T012 (US2, FR-004): retargets to the FOCUSED pane (same
            // self-retargeting mutator pattern as Ctrl+E above).
            KeyCode::Char('g') if ctrl => {
                self.app.toggle_focused_tool_expand();
                return None;
            }
            KeyCode::Char('l') if ctrl => {
                self.app.transcript.clear();
                self.app.scroll = None;
                // Parallel-subagent feature: Ctrl+L also clears the panes +
                // rail (full reset back to the orchestrator view).
                self.app.clear_subagent_panes();
                return None;
            }
            // ── Subagent pane focus ────────────────────────────────────
            // Ctrl+P returns to the orchestrator tab (the pinned rail tab
            // at the bottom does the same by mouse). No-op when already on
            // the main view or when no panes exist. (Ctrl+W stays bound to
            // delete-word-back in the input editor.)
            KeyCode::Char('p') if ctrl => {
                if self.app.focused_subagent.is_some() {
                    self.app.focus_subagent(None);
                    // T004: stats anchors are per-pane — with no pane
                    // focused this is a no-op, so the departed pane KEEPS
                    // its own scroll across focus switches (FR-010); there
                    // is no shared anchor left to reset.
                    self.app.set_focused_pane_stats_view(None);
                }
                return None;
            }
            // ── Subagent rail expansion ───────────────────────────────
            // Ctrl+N toggles the right rail between the collapsed 19-col
            // tab strip and the expanded detail view (same as clicking the
            // rail's title row). Ctrl+B was the natural mnemonic but is
            // already bound (half-page scroll up + input cursor-left), so
            // Ctrl+N ("narrow/widen") is used instead.
            KeyCode::Char('n') if ctrl => {
                self.app.toggle_subagent_rail();
                return None;
            }
            // ── Maximized terminal output viewer ──────────────────────
            // Ctrl+O toggles it: targets the most recent terminal item
            // (running or finished — a finished one replays its full output).
            // T020 (US4, FR-006/FR-008, D6): target-agnostic — the shared
            // viewer widget resolves its SOURCE from the transcript target
            // (the focused pane's transcript when one is focused, the main
            // transcript otherwise), so the same key serves both views.
            // The App mutator resolves the OPEN against the main
            // transcript; when main holds no tool at all but the focused
            // pane does, open directly so Ctrl+O never silently no-ops on
            // a pane view.
            KeyCode::Char('o') if ctrl => {
                let was_open = self.app.output_viewer_open;
                self.app.toggle_output_viewer(None);
                if !was_open && !self.app.output_viewer_open {
                    // The mutator found no MAIN tool to open on. When a pane
                    // is focused and ITS transcript holds a tool, open the
                    // viewer anyway — the widget resolves the pane source at
                    // render time (`output_viewer_index` stays None; it is
                    // main-transcript-indexed and meaningless for a pane).
                    let pane_has_tool = self
                        .app
                        .focused_pane()
                        .map(|p| {
                            p.transcript.iter().any(|it| matches!(it, TranscriptItem::Tool { .. }))
                        })
                        .unwrap_or(false);
                    if pane_has_tool {
                        self.app.output_viewer_open = true;
                        self.app.output_viewer_index = None;
                        self.app.output_viewer_view = None;
                    }
                }
                return None;
            }
            // ── Agent stats page ───────────────────────────────────────
            // Ctrl+A toggles the maximized stats/context page (same view
            // as clicking the header's right section). NOT gated on the
            // T003 resolver: the stats page retargets ITSELF to the focused
            // pane (T004 per-pane state; the stats key-branch above routes
            // through the same resolver), so Ctrl+A stays target-agnostic —
            // gating it here would regress the existing pane-stats parity.
            KeyCode::Char('a') if ctrl => {
                self.app.toggle_stats();
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
            // Subagent rail scrolling (Alt+Up / Alt+Down) — scrolls the
            // right rail's tab window by one pane, making overflow tabs
            // reachable regardless of focus mode. Bound ABOVE the Up-key
            // history-recall arm (which would otherwise swallow Alt+Up on
            // a single-line input). When NeuroCode is also active, panes
            // take priority (the rail keeps its wheel route regardless).
            KeyCode::Up
                if key.modifiers.contains(KeyModifiers::ALT)
                    && !self.app.subagent_panes.is_empty() =>
            {
                self.app.subagent_rail_scroll_up(1);
                return None;
            }
            KeyCode::Down
                if key.modifiers.contains(KeyModifiers::ALT)
                    && !self.app.subagent_panes.is_empty() =>
            {
                self.app.subagent_rail_scroll_down(1);
                return None;
            }
            // When in Input focus, Shift+Up switches to Transcript scroll mode.
            // T007 (US1): the 1-line scroll routes through the single
            // resolver so a focused pane's transcript moves, not the main
            // one; the focus switch itself is target-agnostic.
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.focus = Focus::Transcript;
                match self.resolve_transcript_target() {
                    TranscriptTarget::Pane(_) => self.app.pane_scroll_up(1),
                    TranscriptTarget::Main => self.app.scroll_up(1),
                }
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
                // T003 (D3): route through the single resolver.
                match self.resolve_transcript_target() {
                    TranscriptTarget::Pane(_) => self.app.pane_scroll_up(10),
                    TranscriptTarget::Main => self.app.scroll_up(10),
                }
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
                // T003 (D3): route through the single resolver. T007 (US1):
                // the at-bottom focus return reads the TARGET's anchor —
                // the focused pane's scroll when a pane is targeted, the
                // main anchor otherwise (Main arm keeps the exact
                // pre-indirection behavior).
                let at_bottom = match self.resolve_transcript_target() {
                    TranscriptTarget::Pane(_) => {
                        self.app.pane_scroll_down(10);
                        self.app.focused_pane().map_or(true, |p| p.scroll.is_none())
                    }
                    TranscriptTarget::Main => {
                        self.app.scroll_down(10);
                        self.app.scroll.is_none()
                    }
                };
                // If we've reached the bottom, return focus to input.
                if at_bottom {
                    self.focus = Focus::Input;
                }
                return None;
            }
            // Half-page scrolling (Ctrl+u / Ctrl+d style, but using Ctrl+b/f
            // to avoid clobbering the input editor's kill commands).
            KeyCode::Char('b') if ctrl => {
                let half = 15usize;
                // T003 (D3): route through the single resolver.
                match self.resolve_transcript_target() {
                    TranscriptTarget::Pane(_) => self.app.pane_scroll_up(half),
                    TranscriptTarget::Main => self.app.scroll_up(half),
                }
                if self.focus == Focus::Input {
                    self.focus = Focus::Transcript;
                }
                return None;
            }
            KeyCode::Char('f') if ctrl => {
                let half = 15usize;
                // T003 (D3): route through the single resolver. T007 (US1):
                // the at-bottom focus return reads the TARGET's anchor (the
                // pane's scroll when one is focused), matching PageDown.
                let at_bottom = match self.resolve_transcript_target() {
                    TranscriptTarget::Pane(_) => {
                        self.app.pane_scroll_down(half);
                        self.app.focused_pane().map_or(true, |p| p.scroll.is_none())
                    }
                    TranscriptTarget::Main => {
                        self.app.scroll_down(half);
                        self.app.scroll.is_none()
                    }
                };
                if at_bottom {
                    self.focus = Focus::Input;
                }
                return None;
            }
            _ => {}
        }

        // Focus-dependent keys.
        match self.focus {
            Focus::Transcript => {
                // T003 (D1/D3): single routing point for transcript-targeted
                // keys. `Main` keeps the exact pre-indirection behavior
                // (byte-identical when no pane is focused); `Pane` arms route
                // to the focused pane's transcript. Search keys ('/'/n/N,
                // T016) and copy (y/Y, T017) are wired through the same
                // target resolution — search via the T015 focus-follow
                // mutators.
                let target = self.resolve_transcript_target();
                match key.code {
                    KeyCode::Up | KeyCode::Char('k') => match target {
                        TranscriptTarget::Pane(_) => self.app.pane_scroll_up(1),
                        TranscriptTarget::Main => self.app.scroll_up(1),
                    },
                    KeyCode::Down | KeyCode::Char('j') => match target {
                        TranscriptTarget::Pane(_) => self.app.pane_scroll_down(1),
                        TranscriptTarget::Main => self.app.scroll_down(1),
                    },
                    // T003 routes g/G/Home/End through the resolver too — this
                    // naturally fixes the FR-002 misroute where they hit the
                    // MAIN transcript even with a pane focused (full
                    // affordance parity is T007; pane scroll_to_top needs a
                    // max bound, so top pins to the render-time bound).
                    KeyCode::Char('g') | KeyCode::Home => match target {
                        TranscriptTarget::Pane(_) => {
                            // Top = max scroll offset (render-recorded bound).
                            let max = self.app.last_pane_max_scroll.get();
                            if let Some(pane) = self.app.focused_pane_mut() {
                                pane.scroll = Some(max);
                            }
                        }
                        TranscriptTarget::Main => self.app.scroll_to_top(),
                    },
                    KeyCode::Char('G') | KeyCode::End => match target {
                        TranscriptTarget::Pane(_) => {
                            // Bottom = auto-follow.
                            if let Some(pane) = self.app.focused_pane_mut() {
                                pane.scroll = None;
                            }
                        }
                        TranscriptTarget::Main => self.app.scroll_to_bottom(),
                    },
                    // Space / x: expand-toggle the item at the TOP of the
                    // viewport (mouse clicks also toggle, via hit-testing).
                    // Scroll so the tool/terminal block you want is the top
                    // visible item, then press Space/x.
                    KeyCode::Char(' ') | KeyCode::Char('x') => {
                        // T011 (US2, FR-003): routed via `target` — the Pane
                        // arm mirrors the Main arm's resolution strategy
                        // (viewport-center hit-test via the shared
                        // `transcript_hit_test_core`, then the first-expandable-
                        // at/below-top fallback), just reading the focused
                        // pane's transcript and geometry.
                        match target {
                            TranscriptTarget::Pane(_) => {
                                let (tx, ty, tw, th) = self.app.last_pane_text_area.get();
                                let center_row = ty + th / 2;
                                let max = self.app.last_pane_max_scroll.get();
                                let area = (tx, ty, tw, th);
                                let resolved = self
                                    .app
                                    .focused_pane()
                                    .and_then(|pane| {
                                        // Center row first (same machinery
                                        // pane clicks use, so keyboard and
                                        // mouse always agree).
                                        widgets::transcript_hit_test_core(
                                            &pane.transcript,
                                            &pane.streaming_assistant,
                                            pane.scroll,
                                            max,
                                            area,
                                            self.theme,
                                            center_row,
                                        )
                                        .filter(|&i| pane_item_is_expandable(&pane.transcript[i]))
                                    })
                                    .or_else(|| {
                                        // Fall back to the first expandable
                                        // item at or below the top of the
                                        // view (the "whole transcript fits"
                                        // case), mirroring the Main arm.
                                        self.app.focused_pane().and_then(|pane| {
                                            let top = widgets::transcript_hit_test_core(
                                                &pane.transcript,
                                                &pane.streaming_assistant,
                                                pane.scroll,
                                                max,
                                                area,
                                                self.theme,
                                                ty,
                                            );
                                            top.and_then(|t0| {
                                                (t0..pane.transcript.len()).find(|&i| {
                                                    pane_item_is_expandable(&pane.transcript[i])
                                                })
                                            })
                                        })
                                    });
                                if let Some(i) = resolved {
                                    if let Some(p) = self.app.focused_pane_mut() {
                                        p.toggle_item_expand(i);
                                    }
                                }
                            }
                            TranscriptTarget::Main => {
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
                        }
                    }
                    KeyCode::Enter => {
                        self.focus = Focus::Input;
                        return None;
                    }
                    KeyCode::Char('?') => self.show_help = true,
                    KeyCode::Char('r') => self.toggle_reasoning(),
                    KeyCode::Char('/') => {
                        // Enter search mode. T016 (US3, FR-007, D5):
                        // focus-follow — `open_search` routes the live bar
                        // to the FOCUSED pane's per-view SearchState when
                        // one is focused (fresh query, the orchestrator's
                        // exact open semantics); no pane focused → Main,
                        // byte-identical to before.
                        self.open_search();
                    }
                    KeyCode::Char('n') => {
                        // Find next match. T016: `search_next` focus-follows
                        // (T015) — with a pane focused it walks THAT pane's
                        // matches; the Main walk is byte-identical to before
                        // (self-retargeting mutator, the T012 Ctrl+E/Ctrl+G
                        // pattern — the arm stays target-agnostic).
                        self.app.search_next(true);
                    }
                    KeyCode::Char('N') => {
                        // Find previous match. T016: focus-follow
                        // `search_next` (see 'n' above).
                        self.app.search_next(false);
                    }
                    // `y` copies the last assistant message to the clipboard
                    // (host handles the clipboard); `Y` copies the last user
                    // message. Works regardless of scroll position.
                    // T003 routed these via `target`; T017 (D4) completes the
                    // Pane arms: the same last-assistant/user resolution the
                    // Main arm uses, but against the focused pane's
                    // transcript, emitting `CopyPaneItem` (pane id +
                    // pane-relative idx). The selection model is verbatim
                    // from the orchestrator (no persistent cursor).
                    KeyCode::Char('y') => match target {
                        TranscriptTarget::Main => {
                            let idx = self
                                .app
                                .transcript
                                .iter()
                                .rposition(|i| matches!(i, TranscriptItem::Assistant { .. }));
                            if let Some(idx) = idx {
                                return Some(TuiAction::CopyItem(idx));
                            }
                        }
                        TranscriptTarget::Pane(pane_idx) => {
                            if let Some(pane) = self.app.subagent_panes.get(pane_idx) {
                                let idx = pane
                                    .transcript
                                    .iter()
                                    .rposition(|i| matches!(i, TranscriptItem::Assistant { .. }));
                                if let Some(idx) = idx {
                                    return Some(TuiAction::CopyPaneItem {
                                        pane: pane_idx,
                                        idx,
                                    });
                                }
                            }
                        }
                    },
                    KeyCode::Char('Y') => match target {
                        TranscriptTarget::Main => {
                            let idx = self
                                .app
                                .transcript
                                .iter()
                                .rposition(|i| matches!(i, TranscriptItem::User { .. }));
                            if let Some(idx) = idx {
                                return Some(TuiAction::CopyItem(idx));
                            }
                        }
                        TranscriptTarget::Pane(pane_idx) => {
                            if let Some(pane) = self.app.subagent_panes.get(pane_idx) {
                                let idx = pane
                                    .transcript
                                    .iter()
                                    .rposition(|i| matches!(i, TranscriptItem::User { .. }));
                                if let Some(idx) = idx {
                                    return Some(TuiAction::CopyPaneItem {
                                        pane: pane_idx,
                                        idx,
                                    });
                                }
                            }
                        }
                    },
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
                // T016 (D3): routed via the single resolver — focus-follow
                // `open_search` targets the FOCUSED pane's SearchState when
                // one is focused; otherwise Main, byte-identical to before.
                self.open_search();
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
    ///
    /// T016 (US3, FR-007, design D5): the live bar edits
    /// `App::search_query`; the T015 focus-follow `run_search`/
    /// `search_next` carry it into the FOCUSED pane's per-view SearchState
    /// (query preserved, match indicator + pin set on the pane; the
    /// orchestrator's own search state untouched) or run the
    /// orchestrator's walk when no pane is focused (byte-identical to the
    /// pre-pane behavior, constitution VII). The rendered indicator
    /// mirrors the TARGET view (`draw_search_bar` routes it).
    fn handle_search_key(&mut self, key: KeyEvent) -> Option<TuiAction> {
        match key.code {
            KeyCode::Esc => {
                self.close_search();
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

    /// T016 (D5): open the search bar. The live latch + query live at App
    /// level (the orchestrator's exact open semantics — fresh query); when
    /// a pane is focused, the latch mirrors onto that pane's per-view
    /// SearchState (FR-010: the pane remembers its bar across focus
    /// switches) and every subsequent `run_search`/n/N targets the pane's
    /// transcript via the T015 focus-follow mutators.
    fn open_search(&mut self) {
        self.app.search_open = true;
        self.app.search_query.clear();
        if let Some(pane) = self.app.focused_pane_mut() {
            pane.search_open = true;
            pane.search_query.clear();
        }
    }

    /// T016 (D5): close the search bar — the orchestrator's close
    /// semantics (latch off, live query cleared) applied to the App latch
    /// AND every pane's mirror (the live bar is a singleton overlay, so at
    /// most the focused pane's mirror is on). The panes' preserved
    /// query/indicator/pin from the last run stay where the user navigated
    /// (same as the orchestrator keeping its scroll after close).
    fn close_search(&mut self) {
        self.app.search_open = false;
        self.app.search_query.clear();
        for pane in &mut self.app.subagent_panes {
            pane.search_open = false;
            pane.search_query.clear();
        }
    }

    /// Handle a mouse event for scroll wheel support.
    ///
    /// Call this from the host when a MouseEvent is received. Enables mouse
    /// wheel scrolling in the transcript area. When the pointer is over the
    /// NeuroCode context panel or the live reasoning panel (docked or
    /// expanded), the wheel scrolls that panel instead of the transcript.
    pub fn handle_mouse_scroll(&mut self, row: u16, col: u16, delta_up: bool) {
        // Subagent rail window scrolling: the wheel over the rail strip
        // (right edge) scrolls its tab list — even while a pane is focused
        // (the rail has priority over the pane transcript for pointer
        // events within its own columns). Checked first so it never leaks
        // into the pane/main scroll paths below.
        {
            let (x, y, w, h) = self.app.last_subagent_rail_rect.get();
            if w > 0
                && h > 0
                && row >= y
                && row < y + h
                && col >= x
                && col < x + w
                && !self.app.subagent_panes.is_empty()
            {
                if delta_up {
                    self.app.subagent_rail_scroll_up(3);
                } else {
                    self.app.subagent_rail_scroll_down(3);
                }
                return;
            }
        }
        // Parallel-subagent feature: the pane stats page + pane transcript
        // are the retargeted views when a pane is focused — check their
        // rects before the main-screen equivalents. T007 (US1): gated on
        // the single routing point (D3 — no scattered focused_subagent
        // checks), and the pane-transcript arm mirrors the orchestrator's
        // wheel semantics: up from Input switches to Transcript scroll
        // focus, reaching the bottom (follow-tail) returns focus to Input.
        if matches!(self.resolve_transcript_target(), TranscriptTarget::Pane(_)) {
            {
                let (x, y, w, h) = self.app.last_pane_stats_rect.get();
                if w > 0 && h > 0 && row >= y && row < y + h && col >= x && col < x + w {
                    if delta_up {
                        self.app.pane_stats_scroll_up(3);
                    } else {
                        self.app.pane_stats_scroll_down(3);
                    }
                    return;
                }
            }
            {
                let (x, y, w, h) = self.app.last_pane_text_area.get();
                if w > 0 && h > 0 && row >= y && row < y + h && col >= x && col < x + w {
                    if delta_up {
                        self.app.pane_scroll_up(3);
                        if self.focus == Focus::Input {
                            self.focus = Focus::Transcript;
                        }
                    } else {
                        self.app.pane_scroll_down(3);
                        if self.app.focused_pane().map_or(true, |p| p.scroll.is_none()) {
                            self.focus = Focus::Input;
                        }
                    }
                    return;
                }
            }
        }
        // Agent-stats page first: the wheel scrolls its context stream and
        // never leaks to the transcript while open.
        {
            let (x, y, w, h) = self.app.last_stats_rect.get();
            if w > 0 && h > 0 && row >= y && row < y + h && col >= x && col < x + w {
                if delta_up {
                    self.app.stats_scroll_up(3);
                } else {
                    self.app.stats_scroll_down(3);
                }
                return;
            }
        }
        // Maximized output viewer first: the wheel scrolls the viewer's
        // window and never leaks to the transcript.
        {
            let (x, y, w, h) = self.app.last_output_viewer_rect.get();
            if w > 0 && h > 0 && row >= y && row < y + h && col >= x && col < x + w {
                if delta_up {
                    self.app.output_viewer_scroll_up(3);
                } else {
                    self.app.output_viewer_scroll_down(3);
                }
                return;
            }
        }
        // Expanded explorer first: the wheel drives its panes (canvas zoom,
        // node list, feed) and never leaks to the transcript.
        if let Some(area) = self.expanded_neurocode_area() {
            if row >= area.y && row < area.y + area.height && col >= area.x && col < area.x + area.width {
                crate::neurocode_viz::explorer_scroll(&mut self.app, row, col, delta_up);
                return;
            }
        }
        if self.neurocode_panel_hit(row, col) {
            // Wheel over the context feed: scroll the feed itself.
            if delta_up {
                self.app.neurocode_scroll = self.app.neurocode_scroll.saturating_add(3);
            } else {
                self.app.neurocode_scroll = self.app.neurocode_scroll.saturating_sub(3);
            }
            return;
        }
        if self.reasoning_panel_hit(row, col) {
            // Wheel over the live reasoning panel: up freezes the view at
            // an absolute anchor; down moves toward the tail and re-pins
            // (auto-follow) only when the bottom is reached.
            // T034: the TARGET follows focus — the FOCUSED pane's view
            // state moves while a pane is focused; main's otherwise
            // (byte-identical to before).
            if self.app.focused_subagent.is_some() {
                if delta_up {
                    self.app.pane_reasoning_scroll_up(3);
                } else {
                    self.app.pane_reasoning_scroll_down(3);
                }
            } else if delta_up {
                self.app.reasoning_scroll_up(3);
            } else {
                self.app.reasoning_scroll_down(3);
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

    /// True when `(row, col)` falls inside the live reasoning panel as
    /// drawn by the last frame (docked strip or expanded main view).
    fn reasoning_panel_hit(&self, row: u16, col: u16) -> bool {
        let (x, y, w, h) = self.app.last_reasoning_rect.get();
        w > 0 && h > 0 && row >= y && row < y + h && col >= x && col < x + w
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

    /// The rect of the expanded NeuroCode explorer as drawn by the last
    /// frame (the lower pane of the main split), when active + expanded.
    fn expanded_neurocode_area(&self) -> Option<Rect> {
        if !self.app.neurocode_active || !self.app.neurocode_expanded {
            return None;
        }
        let (x, y, w, h) = self.app.last_neurocode_rect.get();
        if w == 0 || h == 0 {
            return None;
        }
        Some(Rect::new(x, y, w, h))
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
    /// Clicking the live reasoning panel (docked bottom strip or expanded
    /// main view) toggles it the same way.
    pub fn handle_mouse_click(&mut self, row: u16, col: u16) {
        // Expandable-rail feature: clicking the rail's TITLE row toggles
        // expansion (checked with the other rail targets; the title sits
        // above every tab so rects never overlap).
        if self.app.subagent_rail_title_hit(row, col) {
            self.app.toggle_subagent_rail();
            return;
        }
        // Parallel-subagent feature: the right rail's tabs are the top
        // click target. The pinned ORCHESTRATOR tab (rail bottom) returns
        // to the main view; clicking a pane's tab focuses it (retargeting
        // the main transcript + stats window); clicking the focused tab
        // again returns to the orchestrator view.
        if self.app.orchestrator_tab_hit(row, col) {
            self.app.focus_subagent(None);
            // T004: per-pane stats anchors — no shared anchor to reset; the
            // departed pane keeps its scroll (FR-010).
            self.app.set_focused_pane_stats_view(None);
            return;
        }
        if let Some(idx) = self.app.subagent_tab_hit(row, col) {
            let target = if self.app.focused_subagent == Some(idx) {
                None
            } else {
                Some(idx)
            };
            self.app.focus_subagent(target);
            // T004: per-pane stats anchors — the newly focused pane keeps its
            // own scroll; no shared reset (FR-010).
            self.app.set_focused_pane_stats_view(None);
            return;
        }
        // Header right section (model/session/status): opens the maximized
        // agent-stats page. Checked first — it's at the very top of the
        // screen, above every other hit target.
        {
            let (x, y, w, h) = self.app.last_header_right_rect.get();
            if w > 0 && h > 0 && row >= y && row < y + h && col >= x && col < x + w {
                self.app.toggle_stats();
                return;
            }
        }
        // HyperCode badge in header: toggles the feature.
        {
            let (x, y, w, h) = self.app.last_hypercode_rect.get();
            if w > 0 && h > 0 && row >= y && row < y + h && col >= x && col < x + w {
                self.app.hypercode_enabled = !self.app.hypercode_enabled;
                self.app.push_item(crate::TranscriptItem::Notice {
                    text: format!(
                        "⚡ HyperCode mode: {}",
                        if self.app.hypercode_enabled { "ON" } else { "OFF" }
                    ),
                    kind: NoticeKind::Success,
                });
                return;
            }
        }
        // Stats page open: clicks inside it never leak to the transcript.
        // Expandable-stats: a click that resolves to a context-stream entry
        // toggles that entry's expansion (main window parity — every row is
        // expandable). Clicks on the dashboard header/footer are no-ops.
        {
            let (x, y, w, h) = self.app.last_stats_rect.get();
            if w > 0 && h > 0 && row >= y && row < y + h && col >= x && col < x + w {
                let pane_focused = self.app.focused_subagent.is_some();
                let hit = if pane_focused {
                    self.app.pane_stats_context_entry_hit(row)
                } else {
                    self.app.stats_context_entry_hit(row)
                };
                if let Some(entry_idx) = hit {
                    if pane_focused {
                        self.app.toggle_pane_context_entry(entry_idx);
                    } else {
                        self.app.toggle_context_entry(entry_idx);
                    }
                }
                return;
            }
        }
        // Expanded explorer first: clicks select nodes / dock via its own
        // hit-testing (title bar docks; canvas selects nearest node; node
        // list selects rows). Only fully-consumed clicks return early.
        if let Some(area) = self.expanded_neurocode_area() {
            if row >= area.y && row < area.y + area.height && col >= area.x && col < area.x + area.width {
                if crate::neurocode_viz::explorer_click(&mut self.app, row, col, area) {
                    return;
                }
                return; // clicks inside the explorer never leak out
            }
        }
        // NeuroCode feed next: a click inside the docked panel (any part of
        // it, including borders) toggles docked ↔ expanded and is consumed.
        if self.neurocode_panel_hit(row, col) {
            self.app.toggle_neurocode_expanded();
            return;
        }
        // Live reasoning panel next (same docked ↔ expanded toggle).
        // T034 (US4, FR-008, D6): the TARGET follows focus — with a pane
        // focused the click toggles THAT pane's expansion (main state
        // untouched, focused-view isolation); unfocused it toggles the
        // main panel exactly as before (byte-identical).
        if self.reasoning_panel_hit(row, col) {
            if self.app.focused_subagent.is_some() {
                self.app.toggle_focused_pane_reasoning_expanded();
            } else {
                self.app.toggle_reasoning_expanded();
            }
            return;
        }
        // Focus the transcript on any click within it.
        if self.focus == Focus::Input {
            self.focus = Focus::Transcript;
        }
        // Parallel-subagent feature: when a pane is focused, clicks in the
        // main area hit the PANE's transcript — toggle its items' expansion
        // (main window parity for the pane view). The hit-test shares the
        // main transcript's line accounting (items + streaming tail,
        // bottom-anchored), just reading from the pane.
        if self.app.focused_subagent.is_some() {
            let (tx, ty, tw, th) = self.app.last_pane_text_area.get();
            if tw > 0 && th > 0 && row >= ty && row < ty + th && col >= tx && col < tx + tw {
                if let Some(pane) = self.app.focused_pane() {
                    if let Some(item_idx) = widgets::transcript_hit_test_core(
                        &pane.transcript,
                        &pane.streaming_assistant,
                        pane.scroll,
                        self.app.last_pane_max_scroll.get(),
                        (tx, ty, tw, th),
                        self.theme,
                        row,
                    ) {
                        if let Some(p) = self.app.focused_pane_mut() {
                            p.toggle_item_expand(item_idx);
                        }
                    }
                }
                return;
            }
        }
        // Resolve the clicked item via per-item hit-testing. ALL expandable
        // kinds (tool/terminal blocks, diffs, reasoning) toggle INLINE —
        // the reasoning-history expand format unified across the transcript.
        // The maximized output viewer remains available via Ctrl+O for
        // live-following long-running output.
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
    use crate::state::{ReasoningExpandState, ToolStatus, TranscriptItem};
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: Some(full_result.to_string()),
            is_terminal,
            exit_code: if is_terminal { Some(0) } else { None },
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        });
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        // Simulate the first frame recording the text-area geometry.
        let theme = Theme::aurora();
        let t = Tui::new_for_test(app, theme, terminal);
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
            Some(TranscriptItem::Tool { expand_state: ReasoningExpandState::TailWindow | ReasoningExpandState::Full, .. })
        );
        assert!(!expanded_before);
        t.handle_key(space());
        let expanded_after = matches!(
            t.app.transcript.back(),
            Some(TranscriptItem::Tool { expand_state: ReasoningExpandState::TailWindow | ReasoningExpandState::Full, .. })
        );
        assert!(expanded_after, "Space toggled the top tool item");
        // Toggle back (short result: Full → Collapsed directly).
        t.handle_key(space());
        let collapsed = matches!(
            t.app.transcript.back(),
            Some(TranscriptItem::Tool { expand_state: ReasoningExpandState::Collapsed, .. })
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
            expand_state: ReasoningExpandState::TailWindow,
            full_args: None,
            full_result: Some(full),
            is_terminal: true,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: Some(full),
            is_terminal: true,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: Some(full),
            is_terminal: true,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
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
            expand_state: ReasoningExpandState::Collapsed,
        });
        // Collapsed hides early lines; expanded shows them.
        app.toggle_item_expand_by_index(0);
        if let Some(TranscriptItem::FileDiff { expand_state, .. }) = app.transcript.back() {
            assert!(matches!(expand_state, ReasoningExpandState::TailWindow | ReasoningExpandState::Full));
        } else {
            panic!("no diff item");
        }
    }
}

#[cfg(test)]
mod header_flow_integration_tests {
    //! Tui-level integration: the busy flag must flow into the header
    //! animator through tick_animations (the same path the host loop uses).
    use super::*;
    use ratatui::backend::TestBackend;

    fn tui() -> Tui<TestBackend> {
        let app = App::new("s", "m");
        let terminal = ratatui::Terminal::new(TestBackend::new(80, 24)).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    fn tick_n(t: &mut Tui<TestBackend>, n: usize) {
        for _ in 0..n {
            t.tick_animations_with_dt(Duration::from_millis(33));
        }
    }

    #[test]
    fn busy_app_engages_header_flow_via_tick_animations() {
        let mut t = tui();
        assert_eq!(t.header_flow.amount(), 0.0, "starts static");
        t.app_mut().mode = crate::state::RunMode::Busy;
        tick_n(&mut t, 60); // ~seconds of frames at test speed
        assert!(
            t.header_flow.amount() > 0.5,
            "busy turn engages the flow, got {}",
            t.header_flow.amount()
        );
        assert!(t.header_flow.brightness(0.5) > 0.0, "wave is visible");
    }

    #[test]
    fn idle_app_keeps_header_flow_static() {
        let mut t = tui();
        tick_n(&mut t, 60);
        assert_eq!(t.header_flow.amount(), 0.0);
        assert_eq!(t.header_flow.brightness(0.42), 0.0);
    }

    #[test]
    fn turn_end_settles_flow_back_to_static() {
        let mut t = tui();
        t.app_mut().mode = crate::state::RunMode::Busy;
        tick_n(&mut t, 90);
        assert!(t.header_flow.amount() > 0.5);
        t.app_mut().mode = crate::state::RunMode::Input;
        tick_n(&mut t, 90);
        assert_eq!(
            t.header_flow.amount(),
            0.0,
            "flow eases out after the turn ends"
        );
    }

    #[test]
    fn draw_renders_without_panic_in_both_modes() {
        // Full-frame smoke: the new parameter must not break any draw path.
        // Uses the terminal directly (TestBackend's error type is Infallible,
        // so Tui::draw's io::Error bound doesn't apply in tests).
        let mut t = tui();
        t.terminal.draw(|_f| {}).unwrap();
        t.app_mut().mode = crate::state::RunMode::Busy;
        tick_n(&mut t, 30);
        t.terminal.draw(|_f| {}).unwrap();
    }
}

#[cfg(test)]
mod stats_page_key_tests {
    //! Ctrl+A / Esc / navigation / header-click for the agent-stats page.
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::backend::TestBackend;

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn tui() -> Tui<TestBackend> {
        let app = App::new("sess", "model");
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    /// Expandable-rail feature: Ctrl+N toggles the rail expansion flag.
    #[test]
    fn ctrl_n_toggles_subagent_rail_expansion() {
        let mut t = tui();
        t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
            id: 1,
            goal: "child work".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
        assert!(!t.app.subagent_rail_expanded, "collapsed by default");
        t.handle_key(ctrl_key('n'));
        assert!(t.app.subagent_rail_expanded, "Ctrl+N expanded the rail");
        t.handle_key(ctrl_key('n'));
        assert!(!t.app.subagent_rail_expanded, "Ctrl+N collapsed it again");
        // Works without panes too (the rail is hidden but the flag still
        // toggles — it only affects rendering when panes exist).
        t.app.clear_subagent_panes();
        t.handle_key(ctrl_key('n'));
        assert!(t.app.subagent_rail_expanded);
    }

    /// Ctrl+N must not leak into the input editor (plain 'n' still types).
    #[test]
    fn plain_n_still_types_into_input() {
        let mut t = tui();
        t.handle_key(plain(KeyCode::Char('n')));
        assert_eq!(t.input.text(), "n", "plain n reaches the input box");
    }

    /// Ctrl+P behavior is unchanged by the new binding: returns to the
    /// orchestrator from a focused pane, no-op otherwise.
    #[test]
    fn ctrl_p_behavior_unchanged() {
        let mut t = tui();
        t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
            id: 1,
            goal: "child work".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
        t.app.focus_subagent(Some(0));
        t.app.toggle_subagent_rail();
        t.handle_key(ctrl_key('p'));
        assert!(t.app.focused_subagent.is_none(), "Ctrl+P returns to orchestrator even while expanded");
        // Ctrl+P doesn't touch the expansion flag.
        assert!(t.app.subagent_rail_expanded, "Ctrl+P leaves expansion alone");
    }

    /// Clicking the rail's title row toggles expansion through the mouse
    /// handler (mouse parity with Ctrl+N).
    #[test]
    fn clicking_rail_title_toggles_expansion() {
        let mut t = tui();
        t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
            id: 1,
            goal: "child work".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
        // Simulate the rect the widget recorded on the last frame.
        t.app.last_subagent_rail_title_rect.set((80, 1, 18, 1));
        t.handle_mouse_click(1, 85);
        assert!(t.app.subagent_rail_expanded, "title click expanded the rail");
        t.handle_mouse_click(1, 85);
        assert!(!t.app.subagent_rail_expanded, "second title click collapsed it");
    }

    /// Ctrl+P returns to the orchestrator view from a focused subagent
    /// pane (mouse parity with the pinned rail tab). No-op when already
    /// on the main view.
    #[test]
    fn ctrl_p_returns_to_orchestrator_from_pane() {
        let mut t = tui();
        // Spawn a pane and focus it.
        t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
            id: 1,
            goal: "child work".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
        t.app.focus_subagent(Some(0));
        assert!(t.app.focused_subagent.is_some());
        t.handle_key(ctrl_key('p'));
        assert!(t.app.focused_subagent.is_none(), "Ctrl+P returned to the orchestrator");
        // No panes at all: still a safe no-op.
        t.handle_key(ctrl_key('p'));
        assert!(t.app.focused_subagent.is_none());
    }

    #[test]
    fn ctrl_a_toggles_and_esc_closes() {
        let mut t = tui();
        assert!(!t.app.stats_open);
        t.handle_key(ctrl_key('a'));
        assert!(t.app.stats_open, "Ctrl+A opened the stats page");
        t.app.last_stats_max_anchor.set(10);
        t.handle_key(plain(KeyCode::Esc));
        assert!(!t.app.stats_open, "Esc closed it");
        assert_eq!(t.app.last_stats_rect.get(), (0, 0, 0, 0), "rect zeroed");
    }

    #[test]
    fn arrows_scroll_and_typing_falls_through() {
        let mut t = tui();
        t.handle_key(ctrl_key('a'));
        t.app.last_stats_max_anchor.set(30);
        t.handle_key(plain(KeyCode::Up));
        assert_eq!(t.app.stats_view, Some(29), "Up scrolled the stream");
        t.handle_key(plain(KeyCode::Down));
        assert!(t.app.stats_view.is_none(), "Down at the tail resumed follow");
        // Printable char still types into the input box.
        t.handle_key(plain(KeyCode::Char('h')));
        assert_eq!(t.input.text(), "h", "printable keys keep typing");
        // End/Home work as tail/top.
        t.app.last_stats_max_anchor.set(30);
        t.handle_key(plain(KeyCode::Home));
        assert_eq!(t.app.stats_view, Some(0));
        t.handle_key(plain(KeyCode::End));
        assert!(t.app.stats_view.is_none());
    }

    #[test]
    fn header_right_click_toggles_stats() {
        let mut t = tui();
        // Simulate the header's right section drawn at the top-right.
        t.app.last_header_right_rect.set((70, 0, 28, 1));
        t.handle_mouse_click(0, 85);
        assert!(t.app.stats_open, "header right click opened the stats page");
        // Click again closes.
        t.handle_mouse_click(0, 85);
        assert!(!t.app.stats_open);
        // A click elsewhere in the header does NOT toggle.
        t.app.last_header_right_rect.set((70, 0, 28, 1));
        t.handle_mouse_click(0, 10);
        assert!(!t.app.stats_open, "left header click is not a stats toggle");
    }

    #[test]
    fn wheel_inside_stats_rect_scrolls_stream() {
        let mut t = tui();
        t.handle_key(ctrl_key('a'));
        // Simulate the stats page drawn at rows 8..30.
        t.app.last_stats_rect.set((0, 8, 100, 22));
        t.app.last_stats_max_anchor.set(50);
        t.handle_mouse_scroll(15, 40, true);
        assert_eq!(t.app.stats_view, Some(47), "wheel-up scrolled the stream");
        t.handle_mouse_scroll(15, 40, false);
        // 47 + 3 = 50 >= max_anchor 50 → back to the tail, follow resumes.
        assert!(t.app.stats_view.is_none(), "wheel-down to the tail resumed follow");
        // Outside the rect falls through to the transcript.
        t.app.stats_view = Some(47);
        t.handle_mouse_scroll(2, 40, true);
        assert_eq!(t.app.stats_view, Some(47), "outside wheel didn't touch the stream");
    }

    #[test]
    fn stats_open_survives_and_shows_live_updates() {
        // While open, a new snapshot replaces state and the page stays open.
        let mut t = tui();
        t.handle_key(ctrl_key('a'));
        use joey_agent_core::events::ContextEntry;
        use joey_agent_core::AgentEvent;
        t.app.apply(AgentEvent::ContextSnapshot {
            entries: vec![ContextEntry {
                role: "user".into(),
                tokens: 42,
                preview: "hello".into(),
                has_tool_calls: false,
                is_compressed_summary: false,
                full_content: String::new(),
            }],
            system_tokens: 100,
            history_tokens: 42,
            context_window: 1_000,
            compression_threshold: 800,
            compactions: 0,
            model: "m".into(),
        });
        assert!(t.app.stats_open, "still open after a snapshot");
        assert_eq!(t.app.context_entries.len(), 1);
    }

    /// Alt+Down / Alt+Up scroll the subagent rail window (scrollable-rail
    /// feature); plain j/k pane-scroll behavior is unchanged.
    #[test]
    fn alt_arrows_scroll_subagent_rail() {
        let mut t = tui();
        for i in 0..12 {
            t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
                id: 1 + i,
                goal: format!("goal-{i}"),
                model: "m".into(),
                toolset_summary: "file".into(),
                depth: 0,
            });
        }
        // Simulate the geometry a rendered frame records (clamped max).
        t.app.last_subagent_rail_max_scroll.set(7);
        let alt_down = KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        let alt_up = KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        assert_eq!(t.app.subagent_rail_scroll, 0);
        t.handle_key(alt_down);
        assert_eq!(t.app.subagent_rail_scroll, 1, "Alt+Down scrolled down 1 pane");
        t.handle_key(alt_down);
        assert_eq!(t.app.subagent_rail_scroll, 2);
        t.handle_key(alt_up);
        assert_eq!(t.app.subagent_rail_scroll, 1, "Alt+Up scrolled back up");
        // Clamps at 0 and max.
        t.handle_key(alt_up);
        t.handle_key(alt_up);
        t.handle_key(alt_up);
        assert_eq!(t.app.subagent_rail_scroll, 0, "clamped at 0");
        for _ in 0..20 {
            t.handle_key(alt_down);
        }
        assert_eq!(t.app.subagent_rail_scroll, 7, "clamped at recorded max");
        // Existing j/k pane-scroll behavior unchanged: with no pane focused
        // (input focus), Alt+arrows never leaked 'j'/'k' into the input.
        assert_eq!(t.input.text(), "", "no chars leaked into the input");
    }

    /// Alt+arrows must NOT claim the keys when no subagent panes exist —
    /// NeuroCode's Alt-scroll (and plain typing) keeps working.
    #[test]
    fn alt_arrows_idle_without_panes() {
        let mut t = tui();
        t.app.last_subagent_rail_max_scroll.set(0);
        t.handle_key(KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::ALT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert_eq!(t.app.subagent_rail_scroll, 0, "no panes → no rail scroll");
    }

    /// Mouse wheel over the rail rect scrolls the rail — both with and
    /// without a focused pane; elsewhere the transcript keeps the wheel.
    #[test]
    fn wheel_over_rail_rect_scrolls_rail() {
        let mut t = tui();
        for i in 0..12 {
            t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
                id: 1 + i,
                goal: format!("goal-{i}"),
                model: "m".into(),
                toolset_summary: "file".into(),
                depth: 0,
            });
        }
        t.app.last_subagent_rail_max_scroll.set(7);
        // Simulate the rail drawn on the right edge (101..120 cols).
        t.app.last_subagent_rail_rect.set((101, 1, 19, 28));
        t.handle_mouse_scroll(10, 110, false);
        assert_eq!(t.app.subagent_rail_scroll, 3, "wheel down over rail +3 panes");
        t.handle_mouse_scroll(10, 110, true);
        assert_eq!(t.app.subagent_rail_scroll, 0, "wheel up clamped back to 0");
        // Rail priority holds even while a pane is focused.
        t.app.focus_subagent(Some(0));
        t.app.last_pane_text_area.set((0, 1, 100, 28));
        t.handle_mouse_scroll(10, 110, false);
        assert_eq!(
            t.app.subagent_rail_scroll, 3,
            "wheel over rail wins over the pane transcript"
        );
        // Wheel outside the rail still drives the pane transcript.
        t.app.subagent_rail_scroll = 0;
        t.handle_mouse_scroll(10, 50, false);
        assert_eq!(t.app.subagent_rail_scroll, 0, "outside wheel untouched the rail");
    }
}

#[cfg(test)]
mod output_viewer_key_tests {
    //! Ctrl+O / Esc / navigation for the maximized terminal-output viewer.
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use joey_agent_core::events::AgentEvent;
    use ratatui::backend::TestBackend;

    fn tui_with_running_terminal() -> Tui<TestBackend> {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::ToolStart {
            name: "terminal".into(),
            emoji: "💻".into(),
            summary: "long job".into(),
        });
        app.apply(AgentEvent::ToolOutput { name: "terminal".into(), chunk: "partial\n".into() });
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    fn key(code: KeyCode, c: char) -> KeyEvent {
        let _ = c;
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    #[test]
    fn ctrl_o_opens_viewer_and_esc_closes() {
        let mut t = tui_with_running_terminal();
        assert!(!t.app.output_viewer_open);
        t.handle_key(key(KeyCode::Char('o'), 'o'));
        assert!(t.app.output_viewer_open, "Ctrl+O opened the viewer");
        // Simulate the widget's render-time anchor bound, then Esc closes.
        t.app.last_output_viewer_max_anchor.set(10);
        t.handle_key(KeyEvent {
            code: KeyCode::Esc,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert!(!t.app.output_viewer_open, "Esc restored the normal view");
        assert_eq!(t.app.last_output_viewer_rect.get(), (0, 0, 0, 0), "rect zeroed on close");
    }

    #[test]
    fn ctrl_o_is_noop_without_terminal_items() {
        let mut app = App::new("s", "m");
        app.push_item(TranscriptItem::User { text: "hi".into() });
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut t = Tui::new_for_test(app, Theme::aurora(), terminal);
        t.handle_key(key(KeyCode::Char('o'), 'o'));
        assert!(!t.app.output_viewer_open, "nothing to maximize");
    }

    #[test]
    fn viewer_arrow_keys_scroll_not_input() {
        let mut t = tui_with_running_terminal();
        t.handle_key(key(KeyCode::Char('o'), 'o'));
        t.app.last_output_viewer_max_anchor.set(20);
        let up = KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        t.handle_key(up);
        assert_eq!(t.app.output_viewer_view, Some(19), "Up scrolled the viewer");
        // Typing a printable char still reaches the input box (fall-through).
        t.handle_key(KeyEvent {
            code: KeyCode::Char('h'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert_eq!(t.input.text(), "h", "printable keys keep typing");
    }

    #[test]
    fn wheel_inside_viewer_rect_scrolls_viewer() {
        let mut t = tui_with_running_terminal();
        t.handle_key(key(KeyCode::Char('o'), 'o'));
        // Simulate the viewer being drawn at rows 8..30 of a 100x30 screen.
        t.app.last_output_viewer_rect.set((0, 8, 100, 22));
        t.app.last_output_viewer_max_anchor.set(50);
        t.handle_mouse_scroll(15, 40, true);
        assert_eq!(t.app.output_viewer_view, Some(47), "wheel-up scrolled the viewer");
        // A wheel event outside the viewer falls through to the transcript.
        t.handle_mouse_scroll(2, 40, true);
        assert_eq!(t.app.output_viewer_view, Some(47), "outside wheel didn't touch the viewer");
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

    // ── Interactive explorer (feature 015 follow-up) ─────────────────

    mod explorer_helpers {
        use super::*;
        use joey_neurocode::context::snapshot::{
            ContextGraphSnapshot, EdgeSnapshot, NodeSnapshot,
        };

        /// A small realistic snapshot: 1 primary + 3 expanded nodes on
        /// depth rings, two typed edges.
        pub fn sample_snapshot() -> ContextGraphSnapshot {
            let mut s = ContextGraphSnapshot::default();
            s.tier = "Frontier".into();
            s.token_estimate = 4321;
            s.nodes.push(NodeSnapshot {
                id: 1,
                fqcn: "com.x.UserServiceImpl".into(),
                name: "UserServiceImpl".into(),
                kind: "Class".into(),
                package: "com.x".into(),
                source_path: "src/UserServiceImpl.java".into(),
                primary: true,
                fan_in: 2,
                ..Default::default()
            });
            for (i, (name, kind, reason, depth)) in [
                ("UserService", "Interface", "implements", 1),
                ("UserRepository", "Class", "injects", 1),
                ("User", "Enum", "exchanges type", 2),
            ]
            .iter()
            .enumerate()
            {
                s.nodes.push(NodeSnapshot {
                    id: 2 + i as u64,
                    fqcn: format!("com.x.{}", name),
                    name: name.to_string(),
                    kind: kind.to_string(),
                    reason: Some(reason.to_string()),
                    via: Some("UserServiceImpl".into()),
                    depth: *depth,
                    ..Default::default()
                });
            }
            s.edges.push(EdgeSnapshot { from: 0, to: 1, kind: "Implements".into() });
            s.edges.push(EdgeSnapshot { from: 0, to: 2, kind: "Injects".into() });
            s.budget.max_expanded_nodes = 24;
            s.budget.max_expansion_depth = 3;
            s
        }

        /// Apply the full NeuroCode event sequence (active, context,
        /// graph) like the engine would.
        pub fn apply_neurocode(app: &mut App, snapshot: ContextGraphSnapshot) {
            app.apply(AgentEvent::NeuroCodeActive { active: true });
            app.apply(AgentEvent::NeuroCodeContext {
                tier: "Frontier".into(),
                token_estimate: 4321,
                expanded_nodes: snapshot.nodes.len() - 1,
                cold_mode: false,
                formatted_context: "## NeuroCode Context\nTarget: UserServiceImpl".into(),
            });
            app.apply(AgentEvent::NeuroCodeGraph { snapshot });
        }
    }

    /// The explorer renders the graph view with stats + node names from
    /// the snapshot (not the raw feed text).
    #[test]
    fn expanded_explorer_shows_graph_view() {
        use explorer_helpers::*;
        let mut app = App::new("s", "m");
        apply_neurocode(&mut app, sample_snapshot());
        app.toggle_neurocode_expanded();
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 36)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);
        draw_body(&mut tui); // 80x24 default area in draw_body… use big term

        let text = buffer_text(&tui);
        assert!(text.contains("neurocode explorer"), "explorer chrome");
        assert!(text.contains("UserServiceImpl"), "primary node labeled");
        assert!(
            text.contains("UserRepository") || text.contains("UserService"),
            "expanded nodes labeled"
        );
    }

    /// Keyboard: directional nav moves the selection, Tab cycles panes,
    /// Enter jumps to the node list, Esc docks. Shift+arrows pan the canvas.
    #[test]
    fn explorer_keyboard_drives_selection_and_tabs() {
        use crate::neurocode_viz::VizTab;
        use explorer_helpers::*;

        let mut app = App::new("s", "m");
        apply_neurocode(&mut app, sample_snapshot());
        app.toggle_neurocode_expanded();
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 36)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);
        draw_body(&mut tui);

        // Canvas cells were recorded by the draw.
        let cells = tui.app.neurocode_viz.node_cells.borrow().clone();
        assert_eq!(cells.len(), 4, "all nodes placed on the canvas");

        // Right-arrow: selection moves to a node strictly right of center.
        let sel_before = tui.app.neurocode_viz.selected;
        let _ = tui.handle_key(KeyEvent {
            code: KeyCode::Right,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        let sel_after = tui.app.neurocode_viz.selected;
        assert_ne!(sel_before, sel_after, "→ moved the selection");
        assert_eq!(tui.app.neurocode_viz.list_cursor, sel_after, "list synced");

        // Tab: graph → nodes.
        let _ = tui.handle_key(KeyEvent {
            code: KeyCode::Tab,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert_eq!(tui.app.neurocode_viz.tab, VizTab::Nodes);
        // Down in the list moves the cursor (and selection).
        let cursor_before = tui.app.neurocode_viz.list_cursor;
        let _ = tui.handle_key(KeyEvent {
            code: KeyCode::Down,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert!(tui.app.neurocode_viz.list_cursor > cursor_before || cursor_before == 3);

        // Esc docks the explorer.
        let _ = tui.handle_key(esc());
        assert!(!tui.app.neurocode_expanded, "Esc docks the explorer");
    }

    /// Shift+arrows pan the canvas without moving the selection.
    #[test]
    fn shift_arrows_pan_the_canvas() {
        use explorer_helpers::*;
        let mut app = App::new("s", "m");
        apply_neurocode(&mut app, sample_snapshot());
        app.toggle_neurocode_expanded();
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 36)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);
        draw_body(&mut tui);

        let pan_before = tui.app.neurocode_viz.pan;
        let sel_before = tui.app.neurocode_viz.selected;
        let _ = tui.handle_key(KeyEvent {
            code: KeyCode::Right,
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert!(tui.app.neurocode_viz.pan.0 > pan_before.0, "pan moved right");
        assert_eq!(tui.app.neurocode_viz.selected, sel_before, "selection unchanged");
        // '0' resets the view.
        let _ = tui.handle_key(KeyEvent {
            code: KeyCode::Char('0'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        assert_eq!(tui.app.neurocode_viz.pan, (0, 0), "0 resets the camera");
    }

    /// Wheel over the expanded explorer zooms the graph (not transcript).
    #[test]
    fn wheel_over_explorer_zooms_canvas() {
        use explorer_helpers::*;
        let mut app = App::new("s", "m");
        apply_neurocode(&mut app, sample_snapshot());
        app.toggle_neurocode_expanded();
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 36)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);
        draw_body(&mut tui);

        let zoom_before = tui.app.neurocode_viz.zoom;
        let (x, y, _w, h) = tui.app.last_neurocode_rect.get();
        // Wheel in the canvas area (left side of the explorer).
        tui.handle_mouse_scroll(y + h / 2, x + 2, true);
        assert!(
            tui.app.neurocode_viz.zoom > zoom_before,
            "wheel-up over canvas zooms in"
        );
        // Wheel-down zooms back out.
        tui.handle_mouse_scroll(y + h / 2, x + 2, false);
        assert_eq!(tui.app.neurocode_viz.zoom, zoom_before);
    }

    /// Clicking a node cell on the canvas selects it; clicking the title
    /// bar docks the explorer.
    #[test]
    fn canvas_click_selects_and_title_click_docks() {
        use explorer_helpers::*;
        let mut app = App::new("s", "m");
        apply_neurocode(&mut app, sample_snapshot());
        app.toggle_neurocode_expanded();
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 36)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);
        draw_body(&mut tui);

        // Click the second node's cell.
        let cells = tui.app.neurocode_viz.node_cells.borrow().clone();
        let (cx, cy) = cells[2];
        tui.handle_mouse_click(cy, cx);
        assert_eq!(tui.app.neurocode_viz.selected, 2, "canvas click selected node 2");
        assert!(tui.app.neurocode_expanded, "still expanded");

        // Title-bar click docks.
        let (x, y, _w, _h) = tui.app.last_neurocode_rect.get();
        tui.handle_mouse_click(y, x + 5);
        assert!(!tui.app.neurocode_expanded, "title click docked the explorer");
    }

    /// Node-list row clicks select the corresponding node.
    #[test]
    fn node_list_click_selects_row() {
        use explorer_helpers::*;
        let mut app = App::new("s", "m");
        apply_neurocode(&mut app, sample_snapshot());
        app.toggle_neurocode_expanded();
        // Switch to the nodes tab so the list is wide-screen.
        app.neurocode_viz.tab = crate::neurocode_viz::VizTab::Nodes;
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 36)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);
        draw_body(&mut tui);

        let (lx, ly, lw, lh) = tui.app.last_viz_nodes_rect.get();
        assert!(lw > 0 && lh > 0, "node-list rect recorded");
        // Click the 4th visible row: border(1) + header(1) + node 2.
        let row = ly + 4.min(lh.saturating_sub(1));
        tui.handle_mouse_click(row, lx + lw / 2);
        assert_eq!(tui.app.neurocode_viz.selected, 2, "row click selected node");
    }

    /// Deactivate resets explorer state alongside the feed.
    #[test]
    fn deactivate_resets_explorer_state() {
        use explorer_helpers::*;
        let mut app = App::new("s", "m");
        apply_neurocode(&mut app, sample_snapshot());
        app.neurocode_viz.selected = 3;
        app.neurocode_viz.zoom = 2.5;
        app.neurocode_viz.tab = crate::neurocode_viz::VizTab::Feed;
        app.toggle_neurocode_expanded();
        app.apply(AgentEvent::NeuroCodeActive { active: false });
        assert!(!app.neurocode_expanded);
        assert!(app.neurocode_snapshot.is_none(), "snapshot dropped");
        assert_eq!(app.neurocode_viz.selected, 0, "viz state reset");
        assert_eq!(app.neurocode_viz.zoom, 1.0);
        assert_eq!(app.neurocode_viz.tab, crate::neurocode_viz::VizTab::Graph);
        assert_eq!(app.last_viz_nodes_rect.get(), (0, 0, 0, 0));
    }

    /// A new snapshot arriving resets selection so the explorer doesn't
    /// point at a stale index.
    #[test]
    fn new_snapshot_resets_selection() {
        use explorer_helpers::*;
        let mut app = App::new("s", "m");
        apply_neurocode(&mut app, sample_snapshot());
        app.neurocode_viz.selected = 3;
        app.apply(AgentEvent::NeuroCodeGraph { snapshot: sample_snapshot() });
        assert_eq!(app.neurocode_viz.selected, 0, "selection reset on new graph");
    }

    /// No snapshot (cold mode / old events): the explorer falls back to
    /// the raw feed text and Esc still docks.
    #[test]
    fn explorer_falls_back_to_raw_feed_without_snapshot() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::NeuroCodeActive { active: true });
        app.apply(AgentEvent::NeuroCodeContext {
            tier: "Frontier".into(),
            token_estimate: 10,
            expanded_nodes: 0,
            cold_mode: true,
            formatted_context: "RAW_FEED_MARKER_ cold mode".into(),
        });
        app.toggle_neurocode_expanded();
        assert!(app.neurocode_snapshot.is_none());
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 36)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);
        draw_body(&mut tui);
        assert!(buffer_text(&tui).contains("RAW_FEED_MARKER_"), "raw feed shown");
        // Any click docks in fallback mode.
        let (x, y, w, h) = tui.app.last_neurocode_rect.get();
        tui.handle_mouse_click(y + h / 2, x + w / 2);
        assert!(!tui.app.neurocode_expanded, "fallback click docks");
    }
}

#[cfg(test)]
mod reasoning_expand_tests {
    //! Click-to-expand behavior for the LIVE reasoning panel (docked bottom
    //! strip ↔ main-screen takeover). Drives the real draw layout through a
    //! TestBackend so hit-testing rects are exactly what the user would see.
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

    /// Build a Tui (80x30 — wide enough for the sidebar) with a LIVE
    /// reasoning block streaming recognizable text.
    fn tui_with_reasoning() -> Tui<TestBackend> {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::TurnStart { max_iterations: 10 });
        app.apply(AgentEvent::ReasoningDelta(
            "LIVE_REASONING_MARKER_ the model is thinking about the problem \
             step by step and streaming its thoughts here, far enough to wrap \
             across several lines in the docked strip."
                .into(),
        ));
        let terminal = ratatui::Terminal::new(TestBackend::new(80, 30)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);
        draw_body(&mut tui); // records last_reasoning_rect (docked)
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
    fn click_on_docked_strip_expands_to_main_screen() {
        let mut t = tui_with_reasoning();
        assert!(!t.app.reasoning_expanded, "starts docked");

        // Hit-test must resolve inside the docked strip first.
        let (x, y, w, h) = t.app.last_reasoning_rect.get();
        assert!(w > 0 && h > 0, "docked reasoning rect recorded by draw");
        let docked_h = h;
        t.handle_mouse_click(y + h / 2, x + w / 2);
        assert!(t.app.reasoning_expanded, "click expanded the panel");

        // Re-draw: the reasoning now renders in the MAIN area, taller than
        // the 8-row docked strip.
        draw_body(&mut t);
        let text = buffer_text(&t);
        assert!(text.contains("LIVE_REASONING_MARKER_"), "reasoning visible");
        let (_ex, _ey, _ew, eh) = t.app.last_reasoning_rect.get();
        assert!(
            eh > docked_h,
            "expanded reasoning is taller than the docked strip ({} > {})",
            eh,
            docked_h
        );
    }

    #[test]
    fn second_click_docks_back() {
        let mut t = tui_with_reasoning();
        let (x, y, w, h) = t.app.last_reasoning_rect.get();
        // Expand…
        t.handle_mouse_click(y + h / 2, x + w / 2);
        assert!(t.app.reasoning_expanded);
        draw_body(&mut t);
        // …then click the EXPANDED panel anywhere to dock back.
        let (ex, ey, ew, eh) = t.app.last_reasoning_rect.get();
        t.handle_mouse_click(ey + eh / 2, ex + ew / 2);
        assert!(!t.app.reasoning_expanded, "second click docked the panel");
        draw_body(&mut t);
        let (nx, ny, nw, nh) = t.app.last_reasoning_rect.get();
        assert_eq!((nx, nw), (x, w), "docked rect restored");
        assert_eq!(nh, h);
        let _ = ny;
    }

    #[test]
    fn esc_collapses_expanded_panel() {
        let mut t = tui_with_reasoning();
        t.app.toggle_reasoning_expanded();
        assert!(t.app.reasoning_expanded);
        let _ = t.handle_key(esc());
        assert!(!t.app.reasoning_expanded, "Esc collapsed the panel");
    }

    #[test]
    fn click_outside_panel_is_untouched() {
        let mut t = tui_with_reasoning();
        // Click the far top-left of the transcript area (never the panel).
        t.handle_mouse_click(1, 1);
        assert!(!t.app.reasoning_expanded, "transcript click does not expand");
    }

    #[test]
    fn wheel_over_panel_scrolls_reasoning_not_transcript() {
        let mut t = tui_with_reasoning();
        let (x, y, w, h) = t.app.last_reasoning_rect.get();
        t.handle_mouse_scroll(y + h / 2, x + w / 2, true);
        assert!(
            t.app.reasoning_view.is_some(),
            "wheel-up over panel freezes the reasoning view"
        );
        // Wheel elsewhere still scrolls the transcript (focus moves off
        // input — the established signal).
        t.focus = Focus::Input;
        t.handle_mouse_scroll(1, 1, true);
        assert!(
            !matches!(t.focus, Focus::Input),
            "wheel elsewhere hits transcript"
        );
    }

    #[test]
    fn reasoning_close_auto_docks_expanded_panel() {
        let mut t = tui_with_reasoning();
        t.app.toggle_reasoning_expanded();
        assert!(t.app.reasoning_expanded);
        // The reasoning block ends: assistant content starts streaming.
        t.app.apply(AgentEvent::ContentDelta("answer".into()));
        assert!(!t.app.reasoning_open, "reasoning block closed");
        assert!(
            !t.app.reasoning_expanded,
            "expanded panel auto-docked on close"
        );
        assert!(t.app.reasoning_view.is_none(), "view reset on close");
    }

    #[test]
    fn toggle_is_noop_without_live_reasoning() {
        let mut app = App::new("s", "m");
        // No ReasoningDelta ever applied — nothing live.
        app.toggle_reasoning_expanded();
        assert!(!app.reasoning_expanded, "no live block → no-op");
        // With a live block it works.
        app.apply(AgentEvent::ReasoningDelta("thinking".into()));
        app.toggle_reasoning_expanded();
        assert!(app.reasoning_expanded);
    }

    // ── Freeze-on-scroll-up / follow-resume-at-bottom semantics ──────

    /// Fixture: expanded reasoning with a stream long enough to overflow
    /// even the tall expanded window (many wrapped lines).
    fn tui_with_long_reasoning() -> Tui<TestBackend> {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::TurnStart { max_iterations: 10 });
        let long = (0..40)
            .map(|i| format!("reasoning line {} — considering step {} carefully", i, i))
            .collect::<Vec<_>>()
            .join("\n");
        app.apply(AgentEvent::ReasoningDelta(long));
        let terminal = ratatui::Terminal::new(TestBackend::new(80, 30)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);
        tui.app.toggle_reasoning_expanded();
        draw_body(&mut tui); // expanded frame measures the stream
        tui
    }

    /// Scrolling up freezes the view: the anchor is absolute, so further
    /// ReasoningDeltas must NOT move the window.
    #[test]
    fn scroll_up_freezes_view_against_streaming() {
        let mut t = tui_with_long_reasoning();
        let max0 = t.app.last_reasoning_max_anchor.get();
        assert!(max0 > 0, "stream overflows the expanded window");

        // Wheel up: view freezes at an absolute anchor above the tail.
        t.app.reasoning_scroll_up(3);
        let anchor = t.app.reasoning_view.expect("frozen anchor set");
        assert_eq!(anchor, max0 - 3, "wheel-up anchors above the tail");

        // More reasoning streams in — the anchor must NOT move.
        t.app.apply(AgentEvent::ReasoningDelta(
            " and more thinking that would push the tail further down".into(),
        ));
        assert_eq!(
            t.app.reasoning_view,
            Some(anchor),
            "frozen anchor is immune to streaming"
        );

        // The rendered window still shows the frozen region: re-draw and
        // verify the measured max grew while the anchor stayed.
        draw_body(&mut t);
        assert!(
            t.app.last_reasoning_max_anchor.get() > max0,
            "stream grew underneath the frozen window"
        );
        assert_eq!(t.app.reasoning_view, Some(anchor));
    }

    /// Scrolling down while frozen moves toward the tail but stays frozen
    /// until the bottom is actually reached; at the bottom, auto-follow
    /// resumes.
    #[test]
    fn follow_resumes_only_at_bottom() {
        let mut t = tui_with_long_reasoning();
        let max = t.app.last_reasoning_max_anchor.get();
        assert!(max >= 10, "fixture stream long enough to walk down");

        // Freeze up by 10, then walk down 3 at a time.
        t.app.reasoning_scroll_up(10);
        assert_eq!(t.app.reasoning_view, Some(max - 10));
        t.app.reasoning_scroll_down(3);
        assert_eq!(
            t.app.reasoning_view,
            Some(max - 7),
            "partial down stays frozen"
        );
        t.app.reasoning_scroll_down(3);
        assert_eq!(t.app.reasoning_view, Some(max - 4));
        // Overshooting the tail resumes follow.
        t.app.reasoning_scroll_down(100);
        assert!(
            t.app.reasoning_view.is_none(),
            "reaching the bottom resumes auto-follow"
        );
        // Wheel-down while following is a no-op (already pinned).
        t.app.reasoning_scroll_down(3);
        assert!(t.app.reasoning_view.is_none());
    }

    /// A frozen view survives geometry changes: when the measured maximum
    /// grows the anchor stays put; when it shrinks past the anchor (resize
    /// up / window grew), the bottom has come to meet the view — follow
    /// resumes without user action.
    #[test]
    fn frozen_anchor_clamps_but_stays_frozen() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::TurnStart { max_iterations: 10 });
        app.apply(AgentEvent::ReasoningDelta("thinking hard".into()));
        // Simulate a render-measured max of 40, freeze 15 above the tail.
        app.last_reasoning_max_anchor.set(40);
        app.reasoning_scroll_up(15);
        assert_eq!(app.reasoning_view, Some(25));
        // Stream grows: anchor 25 is still a valid window — stays frozen.
        app.last_reasoning_max_anchor.set(60);
        app.reasoning_scroll_down(0); // no-op probe; still Some(25)
        assert_eq!(app.reasoning_view, Some(25));
        // Now walk down to the (new) bottom and confirm resume works.
        app.reasoning_scroll_down(35); // 25 + 35 = 60 >= 60
        assert!(app.reasoning_view.is_none());
        // Shrink case: freeze near the tail, then max drops below the
        // anchor — the view is effectively at the bottom → follow resumes
        // on the next down-scroll.
        app.last_reasoning_max_anchor.set(50);
        app.reasoning_scroll_up(5); // anchor 45
        assert_eq!(app.reasoning_view, Some(45));
        app.last_reasoning_max_anchor.set(30); // shrink (resize)
        app.reasoning_scroll_down(1); // 45+1 >= 30 → bottom reached
        assert!(app.reasoning_view.is_none());
    }

    /// The docked strip obeys the same freeze semantics as the expanded
    /// view (shared state, different geometry).
    #[test]
    fn docked_strip_shares_the_freeze_semantics() {
        let mut t = tui_with_reasoning();
        // (docked) Freeze via wheel over the docked strip.
        let (x, y, w, h) = t.app.last_reasoning_rect.get();
        t.handle_mouse_scroll(y + h / 2, x + w / 2, true);
        assert!(t.app.reasoning_view.is_some(), "docked strip freezes too");
        // Expand: the toggle re-pins (documented behavior on mode change),
        // then wheel-up freezes again and survives streaming.
        t.app.toggle_reasoning_expanded();
        assert!(t.app.reasoning_view.is_none(), "expand re-pins to tail");
        draw_body(&mut t);
        t.app.reasoning_scroll_up(2);
        let anchor = t.app.reasoning_view.unwrap();
        t.app.apply(AgentEvent::ReasoningDelta("more".into()));
        assert_eq!(t.app.reasoning_view, Some(anchor));
    }

    /// A frame that skips the reasoning panel (NeuroCode takeover of the
    /// main area) must zero its hit-test rect — a stale rect would catch
    /// clicks meant for the transcript and toggle an invisible panel.
    #[test]
    fn neurocode_takeover_zeroes_reasoning_rect() {
        let mut t = tui_with_reasoning();
        let (x, y, w, h) = t.app.last_reasoning_rect.get();
        assert!(w > 0 && h > 0, "docked strip recorded");
        // NeuroCode active + expanded takes over the main area.
        t.app.apply(AgentEvent::NeuroCodeActive { active: true });
        t.app.apply(AgentEvent::NeuroCodeContext {
            tier: "Frontier".into(),
            token_estimate: 10,
            expanded_nodes: 1,
            cold_mode: false,
            formatted_context: "ctx".into(),
        });
        t.app.toggle_neurocode_expanded();
        draw_body(&mut t);
        assert_eq!(
            t.app.last_reasoning_rect.get(),
            (0, 0, 0, 0),
            "reasoning rect zeroed while NeuroCode owns the main area"
        );
        // A click where the reasoning strip used to be is a plain
        // transcript click, not a phantom toggle.
        t.handle_mouse_click(y + h / 2, x + w / 2);
        assert!(!t.app.reasoning_expanded, "no phantom toggle on stale rect");
        // And the expanded reasoning flag itself was reset when the frame
        // re-evaluated (still expanded from the user's perspective is fine
        // only if drawn; here the takeover wins, so docking is correct).
    }

    /// Visual smoke (run with --nocapture to eyeball): renders docked,
    /// expanded, and scrolled frames through a TestBackend.
    #[test]
    fn visual_frames_printable() {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::TurnStart { max_iterations: 10 });
        let long = (0..40)
            .map(|i| format!("reasoning line {} — considering step {} carefully", i, i))
            .collect::<Vec<_>>()
            .join("\n");
        app.apply(AgentEvent::ReasoningDelta(long));
        let terminal = ratatui::Terminal::new(TestBackend::new(80, 30)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);

        for label in ["DOCKED", "EXPANDED", "EXPANDED+SCROLLED"] {
            if label == "EXPANDED" {
                tui.app.toggle_reasoning_expanded();
            }
            if label == "EXPANDED+SCROLLED" {
                let (x, y, w, h) = tui.app.last_reasoning_rect.get();
                tui.handle_mouse_scroll(y + h / 2, x + w / 2, true);
            }
            draw_body(&mut tui);
            if std::env::var("JOEY_TUI_VISUAL").is_ok() {
                let buf = &tui.terminal.backend().buffer().content;
                println!("════ {} ════", label);
                for row in 0..30usize {
                    let line: String = buf[row * 80..row * 80 + 80]
                        .iter()
                        .map(|c| c.symbol().to_string())
                        .collect();
                    let trimmed = line.trim_end();
                    if !trimmed.is_empty() {
                        println!("{:2}|{}", row, trimmed);
                    }
                }
                println!();
            }
            // Always assert the invariant: reasoning text present.
            assert!(buffer_text(&tui).contains("reasoning line"));
        }
    }

    // ── T034: pane-aware click/Esc retargeting ────────────────────────

    /// Fixture: a focused pane with a live reasoning block streaming
    /// recognizable text (80x30 — the rail is hidden below width 96, so
    /// the pane view owns the main column).
    fn tui_with_focused_pane_reasoning() -> Tui<TestBackend> {
        let mut app = App::new("s", "m");
        app.apply(AgentEvent::SubagentSpawn {
            id: 1,
            goal: "pane child".into(),
            model: "m".into(),
            toolset_summary: "all".into(),
            depth: 0,
        });
        app.apply(AgentEvent::SubagentEvent {
            id: 1,
            event: Box::new(AgentEvent::ReasoningDelta(
                "PANE_REASONING_MARKER_ the child is thinking about its task, \
                 streaming thoughts here, long enough to wrap across several \
                 lines in the docked strip and even in the expanded view."
                    .into(),
            )),
        });
        app.focus_subagent(Some(0));
        let terminal = ratatui::Terminal::new(TestBackend::new(80, 30)).unwrap();
        let mut tui = Tui::new_for_test(app, Theme::aurora(), terminal);
        draw_body(&mut tui); // records the pane's docked reasoning rect
        tui
    }

    /// T034: clicking the pane reasoning panel toggles the PANE's
    /// expansion — App's main reasoning_expanded stays untouched.
    #[test]
    fn click_on_pane_reasoning_panel_toggles_pane_expansion() {
        let mut t = tui_with_focused_pane_reasoning();
        assert!(!t.app.focused_pane().unwrap().reasoning_expanded);
        assert!(!t.app.reasoning_expanded);

        let (x, y, w, h) = t.app.last_reasoning_rect.get();
        assert!(w > 0 && h > 0, "pane docked reasoning rect recorded");
        let docked_h = h;
        t.handle_mouse_click(y + h / 2, x + w / 2);
        assert!(
            t.app.focused_pane().unwrap().reasoning_expanded,
            "PARITY (T034): click toggles the PANE's expansion"
        );
        assert!(
            !t.app.reasoning_expanded,
            "isolation: main reasoning_expanded untouched by the pane click"
        );

        // Re-draw: the pane's reasoning renders in the pane view's main
        // area, taller than the 8-row docked strip.
        draw_body(&mut t);
        let text = buffer_text(&t);
        assert!(text.contains("PANE_REASONING_MARKER_"), "reasoning visible");
        let (_ex, _ey, _ew, eh) = t.app.last_reasoning_rect.get();
        assert!(
            eh > docked_h,
            "expanded pane reasoning is taller than the docked strip ({} > {})",
            eh,
            docked_h
        );
        // The expanded title carries the collapse affordance.
        assert!(
            text.contains("click or Esc to collapse"),
            "expanded pane panel title carries the collapse affordance"
        );

        // Second click docks back (same mutator).
        let (ex, ey, ew, eh) = t.app.last_reasoning_rect.get();
        t.handle_mouse_click(ey + eh / 2, ex + ew / 2);
        assert!(
            !t.app.focused_pane().unwrap().reasoning_expanded,
            "second click docked the pane panel"
        );
    }

    /// T034: Esc collapses the pane's expanded reasoning — the pane KEEPS
    /// focus (one Esc = one surface closed, main-view precedence order).
    #[test]
    fn esc_collapses_pane_expanded_reasoning() {
        let mut t = tui_with_focused_pane_reasoning();
        t.app.toggle_focused_pane_reasoning_expanded();
        assert!(t.app.focused_pane().unwrap().reasoning_expanded);
        let _ = t.handle_key(esc());
        assert!(
            !t.app.focused_pane().unwrap().reasoning_expanded,
            "Esc collapsed the pane's expanded reasoning"
        );
        assert!(
            t.app.focused_subagent.is_some(),
            "the pane itself keeps focus (one surface per Esc)"
        );
    }

    /// T034: the pane panel title carries the SAME affordance segments as
    /// main when live (docked: "click to expand"; expanded: the collapse
    /// variant is pinned by the click test above).
    #[test]
    fn pane_panel_title_carries_main_affordance_when_live() {
        let t = tui_with_focused_pane_reasoning();
        let text = buffer_text(&t);
        assert!(
            text.contains("reasoning · live · click to expand"),
            "PARITY (T034): docked pane panel title carries the expand affordance"
        );
        // The honest-chrome caveat is gone: no bare " reasoning · live "
        // title (without the affordance) anywhere in the frame.
        assert!(
            !text.contains("reasoning · live ") || text.contains("reasoning · live ·"),
            "no affordance-less pane reasoning title remains"
        );
    }

    /// T034: expanded pane reasoning supports the frozen-anchor/scroll
    /// semantics — wheel-up over the pane panel freezes the PANE's view
    /// (main's reasoning_view untouched), the anchor is immune to further
    /// streaming, and scrolling back to the bottom re-pins.
    #[test]
    fn pane_expanded_reasoning_freeze_and_repin_semantics() {
        let mut t = tui_with_focused_pane_reasoning();
        t.app.toggle_focused_pane_reasoning_expanded();
        assert!(t.app.focused_pane().unwrap().reasoning_view.is_none());
        draw_body(&mut t); // expanded pane frame measures the stream

        // Wheel over the expanded pane panel: freezes the pane's view.
        let (x, y, w, h) = t.app.last_reasoning_rect.get();
        t.handle_mouse_scroll(y + h / 2, x + w / 2, true);
        let anchor = t
            .app
            .focused_pane()
            .unwrap()
            .reasoning_view
            .expect("pane view frozen by wheel-up");
        assert!(
            t.app.reasoning_view.is_none(),
            "isolation: main reasoning_view untouched by the pane wheel"
        );

        // Further streaming must not move the frozen pane anchor.
        t.app.apply(AgentEvent::SubagentEvent {
            id: 1,
            event: Box::new(AgentEvent::ReasoningDelta(
                " and more thinking that would push the tail further down".into(),
            )),
        });
        assert_eq!(
            t.app.focused_pane().unwrap().reasoning_view,
            Some(anchor),
            "frozen pane anchor is immune to streaming"
        );

        // Scrolling back to the bottom re-pins (auto-follow resumes).
        t.app.pane_reasoning_scroll_down(100);
        assert!(
            t.app.focused_pane().unwrap().reasoning_view.is_none(),
            "reaching the bottom re-pins the pane view"
        );
    }

    /// T034 (constitution VII non-regression): with NO pane focused, the
    /// click still toggles the MAIN expansion exactly as before — the
    /// click_on_docked_strip_expands_to_main_screen test above pins the
    /// same path; this adds the explicit pane-present-but-unfocused pin:
    /// panes exist, focus is on the orchestrator, the click must hit the
    /// MAIN panel (not any pane's).
    #[test]
    fn unfocused_click_still_toggles_main_with_panes_present() {
        let mut t = tui_with_focused_pane_reasoning();
        t.app.focus_subagent(None); // orchestrator view, pane still exists
        // Main has no live reasoning of its own → its strip doesn't
        // render, so the pane's (now unfocused-invisible) rect is zeroed;
        // give MAIN a live block to make its strip clickable.
        t.app.apply(AgentEvent::ReasoningDelta("MAIN_MARKER thinking".into()));
        draw_body(&mut t);
        assert!(t.app.focused_subagent.is_none());
        let (x, y, w, h) = t.app.last_reasoning_rect.get();
        assert!(w > 0 && h > 0, "main docked reasoning rect recorded");
        t.handle_mouse_click(y + h / 2, x + w / 2);
        assert!(
            t.app.reasoning_expanded,
            "unfocused: click toggles MAIN expansion (byte-identical)"
        );
        assert!(
            !t.app.subagent_panes[0].reasoning_expanded,
            "unfocused: the pane's expansion is untouched"
        );
    }
}

#[cfg(test)]
mod transcript_routing_tests {
    //! T003 (D1/D3, constitution VII): the focused-pane action-routing
    //! indirection. One resolver (`Tui::resolve_transcript_target` reading
    //! `App.focused_subagent`) feeds every transcript-targeted key handler.
    //! These pin the MECHANISM only: Main resolution when unfocused (even
    //! with panes present), Pane resolution when focused, and one
    //! representative key per family dispatching through it. Full per-story
    //! behavior belongs to the T006+ suites.
    use super::*;
    use crate::state::{ReasoningExpandState, ToolStatus};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::backend::TestBackend;

    fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn tui() -> Tui<TestBackend> {
        let app = App::new("sess", "model");
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    fn spawn_pane(t: &mut Tui<TestBackend>, id: u64, goal: &str) {
        t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
            id,
            goal: goal.into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
    }

    /// (a) The single routing point resolves Main whenever
    /// `focused_subagent == None` — panes existing is irrelevant — and the
    /// focused pane's index otherwise.
    #[test]
    fn resolver_main_when_unfocused_even_with_panes() {
        let mut t = tui();
        spawn_pane(&mut t, 1, "child one");
        spawn_pane(&mut t, 2, "child two");
        assert_eq!(
            t.resolve_transcript_target(),
            TranscriptTarget::Main,
            "panes exist but none focused → Main"
        );
        t.app.focus_subagent(Some(1));
        assert_eq!(t.resolve_transcript_target(), TranscriptTarget::Pane(1));
        t.app.focus_subagent(Some(0));
        assert_eq!(t.resolve_transcript_target(), TranscriptTarget::Pane(0));
        t.app.focus_subagent(None);
        assert_eq!(
            t.resolve_transcript_target(),
            TranscriptTarget::Main,
            "back to orchestrator → Main"
        );
    }

    /// (c-scroll) `g`/`G` dispatch through the indirection: Main arm is
    /// byte-identical (scroll_to_top/scroll_to_bottom on the main anchor);
    /// Pane arm retargets (the FR-002 misroute fix falls out of routing
    /// through the resolver).
    #[test]
    fn scroll_g_dispatches_through_indirection() {
        let mut t = tui();
        spawn_pane(&mut t, 1, "child");
        t.app.last_max_scroll.set(40);
        t.focus = Focus::Transcript;
        // None case: main transcript to top, pane untouched.
        t.handle_key(plain(KeyCode::Char('g')));
        assert_eq!(t.app.scroll, Some(40), "g scrolled MAIN to top (Main arm)");
        assert_eq!(t.app.subagent_panes[0].scroll, None, "pane scroll untouched");
        // Pane case: g/G route to the focused pane's transcript.
        t.app.last_pane_max_scroll.set(12);
        t.app.focus_subagent(Some(0));
        t.handle_key(plain(KeyCode::Char('g')));
        assert_eq!(t.app.subagent_panes[0].scroll, Some(12), "g scrolled the PANE to top");
        assert_eq!(t.app.scroll, Some(40), "main anchor untouched by pane-targeted g");
        t.handle_key(plain(KeyCode::Char('G')));
        assert_eq!(t.app.subagent_panes[0].scroll, None, "G re-pinned the PANE to auto-follow");
    }

    /// (c-expand) Space dispatches through the indirection: Main arm keeps
    /// the exact pre-indirection expand behavior; the Pane arm (T011) now
    /// retargets to the focused pane — with an empty pane/geometry here it
    /// naturally no-ops and leaves main state untouched (the pane cycle
    /// itself is pinned in `pane_expand_key_routing_tests`).
    #[test]
    fn expand_space_dispatches_through_indirection() {
        let mut t = tui();
        // Spawn the pane FIRST: apply(SubagentSpawn) appends a Notice item
        // to the main transcript, which would otherwise sit at back().
        spawn_pane(&mut t, 1, "child");
        t.app.push_item(TranscriptItem::Tool {
            name: "read_file".into(),
            emoji: "📄".into(),
            summary: "path=/tmp/x".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.5),
            result_preview: "1\n2\n3".into(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: Some("full output".into()),
            is_terminal: false,
            exit_code: None,
            live_output: String::new(),
            live_output_capacity: crate::state::LIVE_OUTPUT_CAPACITY,
        });
        t.app.last_text_area.set((0, 0, 98, 28));
        t.focus = Focus::Transcript;
        // None case (pane present, unfocused): MAIN item expands as before.
        t.handle_key(plain(KeyCode::Char(' ')));
        let expanded = matches!(
            t.app.transcript.back(),
            Some(TranscriptItem::Tool { expand_state: ReasoningExpandState::TailWindow | ReasoningExpandState::Full, .. })
        );
        assert!(expanded, "Space expanded the MAIN item (Main arm)");
        // Pane case: routed to the Pane arm → no-op, main state untouched.
        t.app.focus_subagent(Some(0));
        t.handle_key(plain(KeyCode::Char(' ')));
        let still_expanded = matches!(
            t.app.transcript.back(),
            Some(TranscriptItem::Tool { expand_state: ReasoningExpandState::TailWindow | ReasoningExpandState::Full, .. })
        );
        assert!(still_expanded, "pane-focused Space no-ops (expand story pending)");
    }

    /// (c-copy) `y` dispatches through the indirection: Main arm still
    /// emits `CopyItem(last assistant)`; the Pane arm resolves against
    /// the pane transcript (T017) — this pane's transcript is EMPTY, so
    /// there is no assistant item and no action fires.
    #[test]
    fn copy_y_dispatches_through_indirection() {
        let mut t = tui();
        t.app.push_item(TranscriptItem::User { text: "u".into() });
        t.app.push_item(TranscriptItem::Assistant { text: "a".into() });
        spawn_pane(&mut t, 1, "child");
        t.focus = Focus::Transcript;
        let idx = t
            .app
            .transcript
            .iter()
            .rposition(|i| matches!(i, TranscriptItem::Assistant { .. }))
            .expect("assistant item present");
        // None case: y emits CopyItem(last assistant) exactly as before.
        assert!(matches!(t.handle_key(plain(KeyCode::Char('y'))), Some(TuiAction::CopyItem(i)) if i == idx));
        // Pane case with an EMPTY pane transcript: no pane assistant item
        // → no clipboard action (CopyPaneItem needs something to resolve).
        t.app.focus_subagent(Some(0));
        assert!(
            t.handle_key(plain(KeyCode::Char('y'))).is_none(),
            "pane-focused y with empty pane transcript emits nothing"
        );
    }

    /// T017 (D4): with a pane focused, `y`/`Y` resolve against the PANE
    /// transcript and emit `CopyPaneItem { pane, idx }` with a
    /// pane-relative idx pointing at the pane's last assistant/user item —
    /// never a main-transcript `CopyItem` (which would copy the wrong
    /// text). With `focused_subagent == None` the main `CopyItem` path is
    /// unchanged (non-regression pin, constitution VII).
    #[test]
    fn copy_y_pane_emits_copy_pane_item_and_main_path_unchanged() {
        let mut t = tui();
        // Main: last assistant is "main secret" at idx 1.
        t.app.push_item(TranscriptItem::User { text: "main q".into() });
        t.app.push_item(TranscriptItem::Assistant { text: "main secret".into() });
        // Pane 0: distinct texts; last assistant is idx 1, last user idx 2.
        spawn_pane(&mut t, 1, "child");
        let pane = &mut t.app.subagent_panes[0];
        pane.push_item(TranscriptItem::User { text: "pane q".into() });
        pane.push_item(TranscriptItem::Assistant { text: "pane secret".into() });
        pane.push_item(TranscriptItem::User { text: "pane tail".into() });
        t.focus = Focus::Transcript;

        // Focused pane: y → CopyPaneItem { pane: 0, idx: 1 } (pane-relative).
        t.app.focus_subagent(Some(0));
        assert!(
            matches!(
                t.handle_key(plain(KeyCode::Char('y'))),
                Some(TuiAction::CopyPaneItem { pane: 0, idx: 1 })
            ),
            "pane-focused y resolves the PANE's last assistant (idx 1), \
             not the main one"
        );
        // Focused pane: Y → CopyPaneItem { pane: 0, idx: 2 } (last pane user).
        assert!(
            matches!(
                t.handle_key(plain(KeyCode::Char('Y'))),
                Some(TuiAction::CopyPaneItem { pane: 0, idx: 2 })
            ),
            "pane-focused Y resolves the PANE's last user (idx 2)"
        );

        // Non-regression: unfocused → CopyItem(main last assistant), the
        // pre-T017 behavior byte-for-byte.
        t.app.focus_subagent(None);
        let main_idx = t
            .app
            .transcript
            .iter()
            .rposition(|i| matches!(i, TranscriptItem::Assistant { .. }))
            .expect("main assistant present");
        assert_eq!(main_idx, 1);
        assert!(
            matches!(
                t.handle_key(plain(KeyCode::Char('y'))),
                Some(TuiAction::CopyItem(i)) if i == main_idx
            ),
            "unfocused y still emits CopyItem(main last assistant)"
        );
    }
}

#[cfg(test)]
mod pane_scroll_key_routing_tests {
    //! T007 (US1, FR-002): every scroll-key family routes to the FOCUSED
    //! pane through the single resolver — half/page scrolls with the
    //! at-bottom focus return reading the TARGET's anchor, and the mouse
    //! wheel over the pane transcript area with the orchestrator's wheel
    //! focus semantics. Negative pins: unfocused (focused_subagent == None)
    //! leaves pane scroll untouched (the T005 subagent_panes suite pins the
    //! main-arm direction; these pin the pane side).
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::backend::TestBackend;

    fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn shift_up() -> KeyEvent {
        KeyEvent {
            code: KeyCode::Up,
            modifiers: KeyModifiers::SHIFT,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn tui() -> Tui<TestBackend> {
        let app = App::new("sess", "model");
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    fn spawn_and_focus(t: &mut Tui<TestBackend>) {
        t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
            id: 1,
            goal: "child".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
        t.app.focus_subagent(Some(0));
        // Render-time pane bound, as the pane widget would record.
        t.app.last_pane_max_scroll.set(40);
    }

    /// Ctrl+B / Ctrl+F move the focused pane 15 lines (clamped at the
    /// render-time bound / follow-tail at the bottom), and Ctrl+F's
    /// at-bottom focus return fires on the PANE anchor, not the main one.
    #[test]
    fn ctrl_b_ctrl_f_scroll_focused_pane_and_clamp() {
        let mut t = tui();
        spawn_and_focus(&mut t);
        t.handle_key(ctrl_key('b'));
        assert_eq!(
            t.app.subagent_panes[0].scroll,
            Some(15),
            "Ctrl+B moved the pane up a half page"
        );
        assert_eq!(t.app.scroll, None, "main anchor untouched");
        // Clamped at the render-time bound.
        t.app.last_pane_max_scroll.set(20);
        t.handle_key(ctrl_key('b'));
        assert_eq!(t.app.subagent_panes[0].scroll, Some(20), "Ctrl+B clamps at max");
        // Ctrl+F walks back down; reaching follow-tail returns focus to Input.
        t.handle_key(ctrl_key('f'));
        assert_eq!(t.app.subagent_panes[0].scroll, Some(5));
        t.focus = Focus::Transcript;
        t.handle_key(ctrl_key('f'));
        assert_eq!(t.app.subagent_panes[0].scroll, None, "Ctrl+F clamps at follow-tail");
        assert_eq!(t.focus, Focus::Input, "pane reached bottom → focus returns to Input");
    }

    /// PgUp / PgDn move the focused pane 10 lines; PgDn's at-bottom focus
    /// return reads the PANE anchor (T007 fix — previously the main one).
    #[test]
    fn page_keys_scroll_focused_pane_with_target_anchored_focus_return() {
        let mut t = tui();
        spawn_and_focus(&mut t);
        t.handle_key(plain(KeyCode::PageUp));
        assert_eq!(t.app.subagent_panes[0].scroll, Some(10), "PgUp moved the pane 10 lines");
        assert_eq!(t.app.scroll, None, "main anchor untouched");
        t.handle_key(plain(KeyCode::PageDown));
        assert_eq!(
            t.app.subagent_panes[0].scroll,
            None,
            "PgDn by exactly the offset lands at follow-tail (scroll_down parity)"
        );
        // Main anchor is None the whole time, so the pre-T007 code would
        // ALSO have flipped focus here — pin the pane side anyway: a
        // second PgDn from scroll 0 clamps to follow-tail.
        t.focus = Focus::Transcript;
        t.handle_key(plain(KeyCode::PageDown));
        assert_eq!(t.app.subagent_panes[0].scroll, None, "PgDn clamps at follow-tail");
        assert_eq!(t.focus, Focus::Input, "pane at bottom → focus returns to Input");
        // Contrast: pane still scrolled (Some) → focus must NOT return even
        // though the MAIN anchor is None (the pre-T007 code read the main
        // anchor here and would have wrongly flipped focus to Input).
        t.app.last_pane_max_scroll.set(40);
        t.handle_key(plain(KeyCode::PageUp));
        t.handle_key(plain(KeyCode::PageUp));
        assert_eq!(t.app.subagent_panes[0].scroll, Some(20));
        t.handle_key(plain(KeyCode::PageDown)); // 20 → 10, still pinned
        assert_eq!(t.app.subagent_panes[0].scroll, Some(10));
        assert_eq!(
            t.focus,
            Focus::Transcript,
            "pane still scrolled → focus stays (main anchor is None but irrelevant)"
        );
    }

    /// Shift+Up scrolls the FOCUSED PANE 1 line (T007 fix — previously
    /// always the main transcript) while switching to Transcript focus.
    #[test]
    fn shift_up_scrolls_focused_pane_one_line() {
        let mut t = tui();
        spawn_and_focus(&mut t);
        t.handle_key(shift_up());
        assert_eq!(t.focus, Focus::Transcript, "Shift+Up switches to transcript focus");
        assert_eq!(t.app.subagent_panes[0].scroll, Some(1), "the PANE scrolled 1 line");
        assert_eq!(t.app.scroll, None, "main anchor untouched");
    }

    /// Mouse wheel over the pane transcript area (focused pane) scrolls it
    /// 3 lines per notch; up switches Input→Transcript focus, reaching the
    /// bottom (follow-tail) returns focus to Input — the orchestrator's
    /// wheel semantics, on the pane.
    #[test]
    fn wheel_over_pane_area_scrolls_focused_pane() {
        let mut t = tui();
        spawn_and_focus(&mut t);
        t.app.last_pane_text_area.set((0, 2, 60, 20));
        let (row, col) = (10u16, 30u16);
        t.handle_mouse_scroll(row, col, true);
        assert_eq!(t.app.subagent_panes[0].scroll, Some(3), "wheel-up scrolled the pane 3 lines");
        assert_eq!(t.focus, Focus::Transcript, "wheel-up from Input switches to Transcript");
        assert_eq!(t.app.scroll, None, "main anchor untouched");
        t.handle_mouse_scroll(row, col, true);
        assert_eq!(t.app.subagent_panes[0].scroll, Some(6));
        // Wheel-down toward the bottom: 6-3=3, still pinned → focus kept.
        t.handle_mouse_scroll(row, col, false);
        assert_eq!(t.app.subagent_panes[0].scroll, Some(3));
        assert_eq!(t.focus, Focus::Transcript, "still scrolled → focus stays on Transcript");
        // Down past the bottom clamps to follow-tail and returns focus.
        t.handle_mouse_scroll(row, col, false);
        t.handle_mouse_scroll(row, col, false);
        assert_eq!(t.app.subagent_panes[0].scroll, None, "wheel-down clamps at follow-tail");
        assert_eq!(t.focus, Focus::Input, "pane at bottom → focus returns to Input");
        // Clamped up: a huge bound keeps clamping to last_pane_max_scroll.
        t.app.last_pane_max_scroll.set(4);
        t.handle_mouse_scroll(row, col, true);
        t.handle_mouse_scroll(row, col, true);
        assert_eq!(
            t.app.subagent_panes[0].scroll,
            Some(4),
            "wheel-up clamps at the render-time bound"
        );
    }

    /// Negative: with focused_subagent == None the same keys leave the pane
    /// scroll untouched — panes may exist, but the orchestrator view is
    /// targeted (T005's subagent_panes suite pins the main-arm direction).
    #[test]
    fn unfocused_keys_leave_pane_scroll_untouched() {
        let mut t = tui();
        // Pane exists but is NOT focused.
        t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
            id: 1,
            goal: "child".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
        t.app.last_pane_max_scroll.set(40);
        t.app.last_max_scroll.set(40);
        t.handle_key(ctrl_key('b'));
        t.handle_key(plain(KeyCode::PageUp));
        t.handle_key(shift_up());
        assert_eq!(
            t.app.subagent_panes[0].scroll,
            None,
            "pane scroll untouched while unfocused"
        );
        assert_eq!(t.app.scroll, Some(1 + 15 + 10), "main transcript scrolled (Main arms)");
    }
}

#[cfg(test)]
mod pane_expand_key_routing_tests {
    //! T011 (US2, FR-003): the Space/x expand arm in `Tui::handle_key`
    //! retargets to the FOCUSED pane. The Pane arm mirrors the Main arm's
    //! resolution strategy — viewport-CENTER row hit-tested through the
    //! shared `widgets::transcript_hit_test_core` (the same machinery pane
    //! clicks use), falling back to the first expandable item at/below the
    //! top-visible row — then toggles via `SubagentPane::toggle_item_expand`.
    //! Integration tests can't construct `Tui` (`new_for_test` is
    //! `#[cfg(test)]`-gated), so the ROUTING is pinned here; the underlying
    //! three-state cycle + click hit-test parity is pinned by T010's
    //! tests/pane_expand_parity.rs, and the unfocused Main-arm direction by
    //! transcript_routing_tests::expand_space_dispatches_through_indirection.
    use super::*;
    use crate::state::{ReasoningExpandState, ToolStatus, LIVE_OUTPUT_CAPACITY};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::backend::TestBackend;

    fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn tui() -> Tui<TestBackend> {
        let app = App::new("sess", "model");
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    fn spawn_pane(t: &mut Tui<TestBackend>) {
        t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
            id: 1,
            goal: "child".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
    }

    /// A completed Tool item with an `n`-line result (>200 exercises the
    /// full Collapsed → TailWindow → Full → Collapsed cycle).
    fn long_tool(n: usize) -> TranscriptItem {
        let result =
            (0..n).map(|j| format!("tool out line {j:03}")).collect::<Vec<_>>().join("\n");
        TranscriptItem::Tool {
            name: "longtool".into(),
            emoji: "🔧".into(),
            summary: "long tool summary".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.5),
            result_preview: result.clone(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: Some("{}".into()),
            full_result: Some(result),
            is_terminal: false,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: LIVE_OUTPUT_CAPACITY,
        }
    }

    /// A NON-expandable User item `n` lines tall (center-row decoy).
    fn tall_user(n: usize) -> TranscriptItem {
        TranscriptItem::User {
            text: (0..n).map(|j| format!("user line {j:03}")).collect::<Vec<_>>().join("\n"),
        }
    }

    fn expand_state_of(item: &TranscriptItem) -> ReasoningExpandState {
        match item {
            TranscriptItem::Tool { expand_state, .. }
            | TranscriptItem::Reasoning { expand_state, .. }
            | TranscriptItem::FileDiff { expand_state, .. } => *expand_state,
            _ => ReasoningExpandState::Collapsed,
        }
    }

    fn main_all_collapsed(t: &Tui<TestBackend>, ctx: &str) {
        for (i, it) in t.app.transcript.iter().enumerate() {
            assert_eq!(
                expand_state_of(it),
                ReasoningExpandState::Collapsed,
                "{ctx}: main item {i} untouched while a pane is focused"
            );
        }
    }

    /// Focused pane + Space/x: the viewport-center resolution lands on the
    /// pane's tall Tool item and cycles it Collapsed → TailWindow → Full →
    /// Collapsed (third press via `x` — same arm); the MAIN transcript's
    /// expandables never move (focused-view isolation).
    #[test]
    fn space_x_routes_to_focused_pane_tool_cycle() {
        let mut t = tui();
        spawn_pane(&mut t);
        t.app.focus_subagent(Some(0));
        t.app.push_item(long_tool(6)); // MAIN marker — must stay Collapsed
        t.app.subagent_panes[0].push_item(long_tool(220));
        t.app.last_pane_text_area.set((0, 0, 100, 28));
        t.app.last_pane_max_scroll.set(0);
        t.focus = Focus::Transcript;
        assert_eq!(
            expand_state_of(&t.app.subagent_panes[0].transcript[0]),
            ReasoningExpandState::Collapsed,
            "starts Collapsed"
        );

        t.handle_key(plain(KeyCode::Char(' ')));
        assert_eq!(
            expand_state_of(&t.app.subagent_panes[0].transcript[0]),
            ReasoningExpandState::TailWindow,
            "press 1 (Space): Collapsed → TailWindow"
        );
        t.handle_key(plain(KeyCode::Char(' ')));
        assert_eq!(
            expand_state_of(&t.app.subagent_panes[0].transcript[0]),
            ReasoningExpandState::Full,
            "press 2 (Space): TailWindow → Full (220 > 200)"
        );
        t.handle_key(plain(KeyCode::Char('x')));
        assert_eq!(
            expand_state_of(&t.app.subagent_panes[0].transcript[0]),
            ReasoningExpandState::Collapsed,
            "press 3 (x, same arm): Full → Collapsed"
        );

        main_all_collapsed(&t, "space/x on focused pane tool");
    }

    /// Focused pane whose CENTER row lands on a non-expandable User block:
    /// the arm falls back (mirroring the Main arm) to the first expandable
    /// item at/below the top-visible row — the Tool at the bottom — and
    /// toggles it. Keyboard Space and pane clicks therefore agree.
    #[test]
    fn space_x_pane_fallback_picks_first_expandable_below_top() {
        let mut t = tui();
        spawn_pane(&mut t);
        t.app.focus_subagent(Some(0));
        // Tall non-expandable decoy on top, expandable tool at the bottom:
        // bottom-anchored viewport keeps the center row inside the decoy.
        t.app.subagent_panes[0].push_item(tall_user(60));
        t.app.subagent_panes[0].push_item(long_tool(4));
        t.app.last_pane_text_area.set((0, 0, 100, 28));
        t.app.last_pane_max_scroll.set(0);
        t.focus = Focus::Transcript;

        t.handle_key(plain(KeyCode::Char(' ')));
        assert_ne!(
            expand_state_of(&t.app.subagent_panes[0].transcript[1]),
            ReasoningExpandState::Collapsed,
            "fallback resolved and toggled the pane's Tool item"
        );
        assert_eq!(
            expand_state_of(&t.app.subagent_panes[0].transcript[0]),
            ReasoningExpandState::Collapsed,
            "non-expandable decoy is not a toggle target (no panic, no change)"
        );
        main_all_collapsed(&t, "space/x fallback on focused pane");
    }

    /// Negative pin: panes exist and carry expandables, but with
    /// `focused_subagent == None` Space targets the MAIN transcript only —
    /// the pane's items stay Collapsed (constitution VII: unfocused
    /// behavior byte-identical to the pre-pane routing).
    #[test]
    fn unfocused_space_targets_main_only() {
        let mut t = tui();
        spawn_pane(&mut t); // pane exists, NOT focused
        t.app.subagent_panes[0].push_item(long_tool(220));
        t.app.push_item(long_tool(6)); // MAIN target
        t.app.last_text_area.set((0, 0, 98, 28));
        // Pane geometry would resolve if misrouted — pin that it doesn't.
        t.app.last_pane_text_area.set((0, 0, 100, 28));
        t.app.last_pane_max_scroll.set(0);
        t.focus = Focus::Transcript;

        t.handle_key(plain(KeyCode::Char(' ')));
        let main_expanded = matches!(
            t.app.transcript.back(),
            Some(TranscriptItem::Tool {
                expand_state: ReasoningExpandState::TailWindow
                    | ReasoningExpandState::Full,
                ..
            })
        );
        assert!(main_expanded, "Space expanded the MAIN item (Main arm)");
        assert_eq!(
            expand_state_of(&t.app.subagent_panes[0].transcript[0]),
            ReasoningExpandState::Collapsed,
            "pane item untouched while unfocused"
        );
    }
}

#[cfg(test)]
mod pane_ctrl_expand_key_routing_tests {
    //! T012 (US2, FR-004): the dedicated expand keys retarget to the FOCUSED
    //! pane. Ctrl+E (`cycle_focused_reasoning_expand`) and Ctrl+G
    //! (`toggle_focused_tool_expand`) are App mutators that read
    //! `focused_subagent` themselves (the Ctrl+A/T004 self-retargeting
    //! pattern — the arm stays target-agnostic, no scattered is_some()
    //! checks), so these key-level tests press the real keys through
    //! `Tui::handle_key` and pin: focused → the PANE's most-recent entry
    //! cycles through ALL THREE states while the main transcript stays
    //! Collapsed; unfocused → the main transcript's entry moves and the
    //! pane's stays Collapsed (byte-identical Main behavior, constitution
    //! VII). The App-method retarget itself is also pinned in state.rs's
    //! `pane_ctrl_expand_mutator_tests`.
    use super::*;
    use crate::state::{ReasoningExpandState, ToolStatus, LIVE_OUTPUT_CAPACITY};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::backend::TestBackend;
    use std::time::Duration;

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn tui() -> Tui<TestBackend> {
        let app = App::new("sess", "model");
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    fn spawn_pane(t: &mut Tui<TestBackend>) {
        t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
            id: 1,
            goal: "child".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
    }

    /// A `Reasoning` item with `n` lines (>200 exercises all three cycle
    /// states distinctly: Collapsed → TailWindow → Full → Collapsed).
    fn long_reasoning(n: usize) -> TranscriptItem {
        TranscriptItem::Reasoning {
            text: (0..n).map(|j| format!("think line {j:03}")).collect::<Vec<_>>().join("\n"),
            expand_state: ReasoningExpandState::Collapsed,
            thought_duration: Some(Duration::from_secs(2)),
        }
    }

    /// A completed Tool item with an `n`-line result (>200 for the full
    /// three-state cycle).
    fn long_tool(n: usize) -> TranscriptItem {
        let result =
            (0..n).map(|j| format!("tool out line {j:03}")).collect::<Vec<_>>().join("\n");
        TranscriptItem::Tool {
            name: "longtool".into(),
            emoji: "🔧".into(),
            summary: "long tool summary".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.5),
            result_preview: result.clone(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: Some("{}".into()),
            full_result: Some(result),
            is_terminal: false,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: LIVE_OUTPUT_CAPACITY,
        }
    }

    fn expand_state_of(item: &TranscriptItem) -> ReasoningExpandState {
        match item {
            TranscriptItem::Tool { expand_state, .. }
            | TranscriptItem::Reasoning { expand_state, .. }
            | TranscriptItem::FileDiff { expand_state, .. } => *expand_state,
            _ => ReasoningExpandState::Collapsed,
        }
    }

    fn main_all_collapsed(t: &Tui<TestBackend>, ctx: &str) {
        for (i, it) in t.app.transcript.iter().enumerate() {
            assert_eq!(
                expand_state_of(it),
                ReasoningExpandState::Collapsed,
                "{ctx}: main item {i} untouched while a pane is focused"
            );
        }
    }

    /// Focused pane + Ctrl+E: the PANE's most-recent reasoning entry cycles
    /// Collapsed → TailWindow → Full → Collapsed (three real presses); the
    /// pane's tool and every MAIN expandable stay Collapsed.
    #[test]
    fn ctrl_e_cycles_focused_pane_reasoning_three_states() {
        let mut t = tui();
        spawn_pane(&mut t);
        t.app.focus_subagent(Some(0));
        // Pane: tool first, reasoning LAST (the most-recent target).
        t.app.subagent_panes[0].push_item(long_tool(6));
        t.app.subagent_panes[0].push_item(long_reasoning(220));
        // MAIN markers — must stay Collapsed.
        t.app.push_item(long_reasoning(90));
        t.app.push_item(long_tool(91));
        t.focus = Focus::Transcript;
        let pane_reasoning = |t: &Tui<TestBackend>| expand_state_of(&t.app.subagent_panes[0].transcript[1]);

        assert_eq!(pane_reasoning(&t), ReasoningExpandState::Collapsed, "starts Collapsed");
        t.handle_key(ctrl_key('e'));
        assert_eq!(
            pane_reasoning(&t),
            ReasoningExpandState::TailWindow,
            "press 1: Collapsed → TailWindow"
        );
        t.handle_key(ctrl_key('e'));
        assert_eq!(
            pane_reasoning(&t),
            ReasoningExpandState::Full,
            "press 2: TailWindow → Full (220 > 200)"
        );
        t.handle_key(ctrl_key('e'));
        assert_eq!(
            pane_reasoning(&t),
            ReasoningExpandState::Collapsed,
            "press 3: Full → Collapsed"
        );

        assert_eq!(
            expand_state_of(&t.app.subagent_panes[0].transcript[0]),
            ReasoningExpandState::Collapsed,
            "pane tool untouched by Ctrl+E"
        );
        main_all_collapsed(&t, "ctrl+e while pane focused");
    }

    /// Focused pane + Ctrl+G: the PANE's most-recent tool entry cycles
    /// Collapsed → TailWindow → Full → Collapsed; the pane's reasoning and
    /// every MAIN expandable stay Collapsed.
    #[test]
    fn ctrl_g_toggles_focused_pane_tool_three_states() {
        let mut t = tui();
        spawn_pane(&mut t);
        t.app.focus_subagent(Some(0));
        // Pane: reasoning first, tool LAST (the most-recent target).
        t.app.subagent_panes[0].push_item(long_reasoning(90));
        t.app.subagent_panes[0].push_item(long_tool(220));
        t.app.push_item(long_reasoning(90)); // MAIN markers
        t.app.push_item(long_tool(91));
        t.focus = Focus::Transcript;
        let pane_tool = |t: &Tui<TestBackend>| expand_state_of(&t.app.subagent_panes[0].transcript[1]);

        assert_eq!(pane_tool(&t), ReasoningExpandState::Collapsed, "starts Collapsed");
        t.handle_key(ctrl_key('g'));
        assert_eq!(pane_tool(&t), ReasoningExpandState::TailWindow, "press 1: → TailWindow");
        t.handle_key(ctrl_key('g'));
        assert_eq!(pane_tool(&t), ReasoningExpandState::Full, "press 2: → Full (220 > 200)");
        t.handle_key(ctrl_key('g'));
        assert_eq!(pane_tool(&t), ReasoningExpandState::Collapsed, "press 3: → Collapsed");

        assert_eq!(
            expand_state_of(&t.app.subagent_panes[0].transcript[0]),
            ReasoningExpandState::Collapsed,
            "pane reasoning untouched by Ctrl+G"
        );
        main_all_collapsed(&t, "ctrl+g while pane focused");
    }

    /// Negative pin (constitution VII): panes exist and carry long
    /// expandables, but with `focused_subagent == None` the same keys act on
    /// the MAIN transcript — the pane's entries stay Collapsed.
    #[test]
    fn unfocused_ctrl_e_ctrl_g_act_on_main_only() {
        let mut t = tui();
        spawn_pane(&mut t); // pane exists, NOT focused
        t.app.subagent_panes[0].push_item(long_reasoning(220));
        t.app.subagent_panes[0].push_item(long_tool(220));
        t.app.push_item(long_reasoning(220)); // MAIN targets
        t.app.push_item(long_tool(220));
        t.focus = Focus::Transcript;
        assert!(t.app.focused_subagent.is_none());

        t.handle_key(ctrl_key('e'));
        let main_reasoning = t
            .app
            .transcript
            .iter()
            .rposition(|i| matches!(i, TranscriptItem::Reasoning { .. }))
            .expect("main reasoning present");
        assert_eq!(
            expand_state_of(&t.app.transcript[main_reasoning]),
            ReasoningExpandState::TailWindow,
            "Ctrl+E cycled the MAIN reasoning (Main behavior)"
        );
        t.handle_key(ctrl_key('g'));
        let main_tool = t
            .app
            .transcript
            .iter()
            .rposition(|i| matches!(i, TranscriptItem::Tool { .. }))
            .expect("main tool present");
        assert_eq!(
            expand_state_of(&t.app.transcript[main_tool]),
            ReasoningExpandState::TailWindow,
            "Ctrl+G toggled the MAIN tool (Main behavior)"
        );
        for (i, it) in t.app.subagent_panes[0].transcript.iter().enumerate() {
            assert_eq!(
                expand_state_of(it),
                ReasoningExpandState::Collapsed,
                "pane item {i} untouched while unfocused"
            );
        }
    }
}

#[cfg(test)]
mod pane_search_key_routing_tests {
    //! T016 (US3, FR-007, research.md D5): search keys route to the FOCUSED
    //! pane. Real `Tui::handle_key` presses pin the routing contract:
    //!   - '/' (Transcript focus) and Ctrl+S (Input focus) open the live bar
    //!     AND mirror it onto the focused pane's per-view SearchState; no
    //!     pane focused → App-level only (byte-identical Main behavior,
    //!     constitution VII).
    //!   - Typing drives the T015 focus-follow `run_search`: a pane-focused
    //!     search pins the PANE, never the main view; a main-only needle is
    //!     a no-match.
    //!   - n/N walk the TARGET view's matches only.
    //!   - Esc closes the bar and clears every mirror.
    //! The rendered indicator routing (pane mirror vs App) is pinned in
    //! tests/pane_search_copy.rs (buffer assertions over draw_search_bar).
    use super::*;
    use crate::state::TranscriptItem;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use ratatui::backend::TestBackend;

    fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn ctrl_key(c: char) -> KeyEvent {
        KeyEvent {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn char_key(c: char) -> KeyEvent {
        plain(KeyCode::Char(c))
    }

    fn tui() -> Tui<TestBackend> {
        let app = App::new("sess", "model");
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    fn spawn_pane(t: &mut Tui<TestBackend>) {
        t.app.apply(joey_agent_core::AgentEvent::SubagentSpawn {
            id: 1,
            goal: "child".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
    }

    fn user(s: &str) -> TranscriptItem {
        TranscriptItem::User { text: s.to_string() }
    }

    /// NOTE: in the OPEN bar, n/N are next/prev keys, not typed characters
    /// (pre-existing orchestrator semantics), so test queries avoid those
    /// letters.
    fn type_in_bar(t: &mut Tui<TestBackend>, query: &str) {
        assert!(
            !query.contains('n') && !query.contains('N'),
            "test queries must avoid the n/N navigation keys"
        );
        for c in query.chars() {
            t.handle_key(char_key(c));
        }
    }

    /// '/' in Transcript focus with a pane focused: the live bar opens AND
    /// the pane's mirror opens (fresh query); typing runs the T015
    /// focus-follow search — the PANE pins to its own match, the main view
    /// and the App indicator never move.
    #[test]
    fn slash_opens_pane_scoped_search_and_typing_pins_pane_only() {
        let mut t = tui();
        spawn_pane(&mut t);
        t.app.focus_subagent(Some(0));
        // Pane needle (newest), filler, main-only needle.
        t.app.subagent_panes[0].push_item(user("filler 0"));
        t.app.subagent_panes[0].push_item(user("pane gold here"));
        t.app.push_item(user("main gold beta"));
        t.app.last_pane_max_scroll.set(100);
        t.focus = Focus::Transcript;

        assert!(!t.app.subagent_panes[0].search_open, "bar closed initially");
        t.handle_key(char_key('/'));
        assert!(t.app.search_open, "live bar opened");
        assert!(t.app.subagent_panes[0].search_open, "pane mirror opened");

        type_in_bar(&mut t, "gold");
        let pane = &t.app.subagent_panes[0];
        assert!(pane.search_has_match, "pane occurrence found");
        assert_eq!(pane.search_query, "gold", "query preserved on the pane");
        assert!(pane.scroll.is_some(), "pane pinned to its match");
        assert_eq!(t.app.scroll, None, "main view untouched");
        assert!(!t.app.search_has_match, "App indicator mirrors MAIN only");
    }

    /// A needle that exists ONLY on main is a no-match from the focused
    /// pane: nothing pins, the indicator stays false.
    #[test]
    fn pane_search_main_only_needle_is_no_match_via_keys() {
        let mut t = tui();
        spawn_pane(&mut t);
        t.app.focus_subagent(Some(0));
        t.app.subagent_panes[0].push_item(user("no such marker"));
        t.app.push_item(user("main gold beta"));
        t.app.last_pane_max_scroll.set(100);
        t.focus = Focus::Transcript;

        t.handle_key(char_key('/'));
        type_in_bar(&mut t, "gold");
        let pane = &t.app.subagent_panes[0];
        assert!(!pane.search_has_match, "main-only needle → no pane match");
        assert_eq!(pane.scroll, None, "pane view never yanked");
        assert_eq!(t.app.scroll, None, "main view untouched");
    }

    /// n/N in the open bar walk the TARGET view only: with two pane
    /// occurrences, N steps to the older one and n returns, while the main
    /// scroll never leaves None.
    #[test]
    fn n_n_in_bar_walk_pane_matches_only() {
        let mut t = tui();
        spawn_pane(&mut t);
        t.app.focus_subagent(Some(0));
        let mut items: Vec<_> = (0..30).map(|i| user(&format!("filler {i}"))).collect();
        items[3] = user("pane gold old");
        items[27] = user("pane gold fresh");
        for it in items {
            t.app.subagent_panes[0].push_item(it);
        }
        t.app.push_item(user("main gold beta"));
        t.app.last_pane_max_scroll.set(100);
        t.focus = Focus::Transcript;

        t.handle_key(char_key('/'));
        type_in_bar(&mut t, "gold");
        let first = t.app.subagent_panes[0].scroll.expect("run pinned the pane");

        t.handle_key(char_key('N')); // toward older
        let second = t.app.subagent_panes[0].scroll.expect("N moved the pane");
        assert_ne!(first, second, "N advances between pane matches");
        assert_eq!(t.app.scroll, None, "main untouched by N");

        t.handle_key(char_key('n')); // back toward newer
        let third = t.app.subagent_panes[0].scroll.expect("n moved the pane");
        assert_ne!(second, third, "n advances back");
        assert_eq!(t.app.scroll, None, "main untouched by n");
    }

    /// n/N with the bar CLOSED (Transcript focus): same target routing via
    /// the self-retargeting mutator — pane matches move, main stays put.
    #[test]
    fn n_n_closed_bar_still_routes_to_focused_pane() {
        let mut t = tui();
        spawn_pane(&mut t);
        t.app.focus_subagent(Some(0));
        let mut items: Vec<_> = (0..30).map(|i| user(&format!("filler {i}"))).collect();
        items[3] = user("pane gold old");
        items[27] = user("pane gold fresh");
        for it in items {
            t.app.subagent_panes[0].push_item(it);
        }
        t.app.last_pane_max_scroll.set(100);
        t.focus = Focus::Transcript;

        // Seed exactly as the bar path would (query typed + run).
        t.app.search_query = "gold".into();
        t.app.run_search();
        assert!(t.app.subagent_panes[0].scroll.is_some(), "pane pinned");

        t.handle_key(char_key('N'));
        assert_eq!(
            t.app.subagent_panes[0].scroll,
            Some(24),
            "N (closed bar) walks the pane's older match (rev-idx 26 → 24)"
        );
        assert_eq!(t.app.scroll, None, "main untouched");
    }

    /// Ctrl+S from Input focus with a pane focused: same open semantics as
    /// '/' (live bar + pane mirror, fresh query).
    #[test]
    fn ctrl_s_opens_search_routed_to_focused_pane() {
        let mut t = tui();
        spawn_pane(&mut t);
        t.app.focus_subagent(Some(0));
        t.focus = Focus::Input;

        t.handle_key(ctrl_key('s'));
        assert!(t.app.search_open, "live bar opened via Ctrl+S");
        assert!(t.app.subagent_panes[0].search_open, "pane mirror opened");
        type_in_bar(&mut t, "zz");
        assert_eq!(t.app.search_query, "zz");
        t.handle_key(plain(KeyCode::Esc));
        t.handle_key(ctrl_key('s'));
        assert_eq!(t.app.search_query, "", "reopen starts a fresh query");
        assert_eq!(t.app.subagent_panes[0].search_query, "", "pane mirror fresh too");
    }

    /// Esc closes the bar and clears the App latch + every pane mirror
    /// (singleton overlay).
    #[test]
    fn esc_closes_bar_and_clears_pane_mirrors() {
        let mut t = tui();
        spawn_pane(&mut t);
        t.app.focus_subagent(Some(0));
        t.app.subagent_panes[0].push_item(user("pane gold here"));
        t.app.last_pane_max_scroll.set(100);
        t.focus = Focus::Transcript;
        t.handle_key(char_key('/'));
        type_in_bar(&mut t, "gold");
        assert!(t.app.subagent_panes[0].search_open);

        t.handle_key(plain(KeyCode::Esc));
        assert!(!t.app.search_open, "App latch off");
        assert!(!t.app.subagent_panes[0].search_open, "pane mirror off");
        assert_eq!(t.app.search_query, "", "live query cleared");
        assert_eq!(t.app.subagent_panes[0].search_query, "", "pane query cleared");
        // The pin from the last run survives close (orchestrator parity).
        assert!(t.app.subagent_panes[0].scroll.is_some(), "pin preserved");
        assert!(t.app.focused_subagent.is_some(), "Esc closed the bar, not the pane");
    }

    /// Unfocused regression pin (constitution VII): with panes present but
    /// NONE focused, '/' opens MAIN-only search — no pane mirror flips on.
    #[test]
    fn unfocused_slash_opens_main_search_only() {
        let mut t = tui();
        spawn_pane(&mut t); // pane exists, NOT focused
        t.app.push_item(user("main gold beta"));
        t.app.last_max_scroll.set(100);
        t.focus = Focus::Transcript;

        t.handle_key(char_key('/'));
        assert!(t.app.search_open, "main bar opened");
        assert!(!t.app.subagent_panes[0].search_open, "no pane mirror");
        type_in_bar(&mut t, "gold");
        assert!(t.app.search_has_match, "main matched");
        assert_eq!(t.app.scroll, Some(0), "main view pinned");
        assert_eq!(
            t.app.subagent_panes[0].scroll, None,
            "pane untouched by main search"
        );
        assert_eq!(
            t.app.subagent_panes[0].search_query, "",
            "pane SearchState untouched"
        );
    }
}

#[cfg(test)]
mod pane_viewer_key_tests {
    //! T020/T021/T025 (US4/US5, spec 017): Ctrl+O retargets to the focused
    //! pane (shared `draw_output_viewer`), the pane's live reasoning renders
    //! through the shared `draw_reasoning` panel, and the help overlay stays
    //! reachable from a focused pane (global handler). These pins drive the
    //! REAL key arms + the REAL render path (`render_body` via TestBackend),
    //! complementing the integration suites' mutator-level assertions.
    use super::*;
    use crate::state::{ReasoningExpandState, ToolStatus, LIVE_OUTPUT_CAPACITY};
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
    use joey_agent_core::events::AgentEvent;
    use ratatui::backend::TestBackend;

    /// Test-only adapter so the REAL `Tui::draw` path (whose bound is
    /// `io::Error: From<B::Error>`) can run against `TestBackend`, whose
    /// error type is `Infallible`. Every call forwards unchanged; the
    /// impossible error is simply eliminated.
    struct IoTestBackend(TestBackend);

    impl ratatui::backend::Backend for IoTestBackend {
        type Error = std::io::Error;

        fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
        where
            I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
        {
            self.0.draw(content).map_err(|e| match e {})
        }

        fn hide_cursor(&mut self) -> Result<(), Self::Error> {
            self.0.hide_cursor().map_err(|e| match e {})
        }

        fn show_cursor(&mut self) -> Result<(), Self::Error> {
            self.0.show_cursor().map_err(|e| match e {})
        }

        fn get_cursor_position(&mut self) -> Result<ratatui::layout::Position, Self::Error> {
            self.0.get_cursor_position().map_err(|e| match e {})
        }

        fn set_cursor_position<P: Into<ratatui::layout::Position>>(
            &mut self,
            position: P,
        ) -> Result<(), Self::Error> {
            self.0.set_cursor_position(position).map_err(|e| match e {})
        }

        fn clear(&mut self) -> Result<(), Self::Error> {
            self.0.clear().map_err(|e| match e {})
        }

        fn clear_region(
            &mut self,
            clear_type: ratatui::backend::ClearType,
        ) -> Result<(), Self::Error> {
            self.0.clear_region(clear_type).map_err(|e| match e {})
        }

        fn size(&self) -> Result<ratatui::layout::Size, Self::Error> {
            self.0.size().map_err(|e| match e {})
        }

        fn window_size(&mut self) -> Result<ratatui::backend::WindowSize, Self::Error> {
            self.0.window_size().map_err(|e| match e {})
        }

        fn flush(&mut self) -> Result<(), Self::Error> {
            self.0.flush().map_err(|e| match e {})
        }
    }

    /// Same as `tui()` but over [`IoTestBackend`], so `Tui::draw` (the real
    /// full-frame render incl. the global help-overlay call site) compiles.
    fn tui_real_draw() -> Tui<IoTestBackend> {
        let app = App::new("sess", "model");
        let terminal = ratatui::Terminal::new(IoTestBackend(TestBackend::new(100, 30))).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    fn key(code: KeyCode, c: char) -> KeyEvent {
        let _ = c;
        KeyEvent {
            code,
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn plain(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn tui() -> Tui<TestBackend> {
        let app = App::new("sess", "model");
        let terminal = ratatui::Terminal::new(TestBackend::new(100, 30)).unwrap();
        Tui::new_for_test(app, Theme::aurora(), terminal)
    }

    fn spawn_pane<B: ratatui::backend::Backend>(t: &mut Tui<B>) {
        t.app.apply(AgentEvent::SubagentSpawn {
            id: 1,
            goal: "child".into(),
            model: "m".into(),
            toolset_summary: "file".into(),
            depth: 0,
        });
        t.app.focus_subagent(Some(0));
    }

    fn pane_tool(t: &mut Tui<TestBackend>, lines: usize, marker: &str) {
        let result = (0..lines)
            .map(|j| format!("{marker} line {j}"))
            .collect::<Vec<_>>()
            .join("\n");
        t.app.subagent_panes[0].push_item(TranscriptItem::Tool {
            name: "toolp".into(),
            emoji: "🔧".into(),
            summary: format!("{marker} summary").into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.5),
            result_preview: result.clone(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: Some(result),
            is_terminal: false,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: LIVE_OUTPUT_CAPACITY,
        });
    }

    /// Render the real body layout and return the whole buffer as text.
    fn buffer_text(t: &mut Tui<TestBackend>) -> String {
        let area = Rect::new(0, 2, 100, 24);
        let spinner = crate::anim::Spinner::dots();
        let equalizer = crate::anim::Equalizer::new(28);
        t.terminal
            .draw(|f| {
                render_body(f, area, &t.app, t.theme, false, 0.5, &spinner, &equalizer);
            })
            .unwrap();
        t.terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol().to_string())
            .collect()
    }

    /// T020: Ctrl+O with a pane focused opens the viewer on the PANE's most
    /// recent tool — shared chrome + pane content, main tool absent — and
    /// toggles closed. Works even when the MAIN transcript has no tool at
    /// all (the pane-fallback open path).
    #[test]
    fn ctrl_o_opens_viewer_on_focused_pane_tool() {
        let mut t = tui();
        spawn_pane(&mut t);
        pane_tool(&mut t, 6, "PANE_TOOL");
        assert!(t.app.transcript.iter().all(|it| !matches!(it, TranscriptItem::Tool { .. })));

        t.handle_key(key(KeyCode::Char('o'), 'o'));
        assert!(t.app.output_viewer_open, "Ctrl+O opened the viewer (pane fallback)");

        let text = buffer_text(&mut t);
        assert!(text.contains("output · finished"), "shared viewer chrome rendered");
        assert!(text.contains("PANE_TOOL line 5"), "pane tool tail content in the viewer");
        assert!(text.contains("Ctrl+O or Esc to restore"), "restore affordance present");

        t.handle_key(key(KeyCode::Char('o'), 'o'));
        assert!(!t.app.output_viewer_open, "second Ctrl+O closed the viewer");
    }

    /// T020: with BOTH transcripts holding tools, the pane-focused viewer
    /// shows the PANE's tool, never the main one (focused-view isolation).
    #[test]
    fn pane_viewer_shows_pane_tool_not_main() {
        let mut t = tui();
        spawn_pane(&mut t);
        pane_tool(&mut t, 6, "PANE_TOOL");
        // Main marker tool (the pre-T020 code would have shown this).
        let main_result = (0..6).map(|j| format!("MAIN_TOOL line {j}")).collect::<Vec<_>>().join("\n");
        t.app.push_item(TranscriptItem::Tool {
            name: "toolm".into(),
            emoji: "🔧".into(),
            summary: "main summary".into(),
            status: ToolStatus::Done,
            duration_secs: Some(0.5),
            result_preview: main_result.clone(),
            expand_state: ReasoningExpandState::Collapsed,
            full_args: None,
            full_result: Some(main_result),
            is_terminal: false,
            exit_code: Some(0),
            live_output: String::new(),
            live_output_capacity: LIVE_OUTPUT_CAPACITY,
        });

        t.handle_key(key(KeyCode::Char('o'), 'o'));
        assert!(t.app.output_viewer_open);
        let text = buffer_text(&mut t);
        assert!(text.contains("PANE_TOOL line 5"), "pane tool content in the viewer");
        assert!(
            !text.contains("MAIN_TOOL"),
            "main tool never renders in the pane-focused viewer"
        );
    }

    /// T021 (render half): the focused pane's live reasoning renders through
    /// the shared `draw_reasoning` panel; the MAIN accumulator never leaks
    /// into the pane view. Unfocused, the pane's stream is not rendered.
    #[test]
    fn pane_reasoning_panel_renders_pane_stream() {
        let mut t = tui();
        spawn_pane(&mut t);
        t.app.subagent_panes[0].streaming_reasoning = "pane think line 39".into();
        // Main live reasoning — must stay invisible while the pane is focused.
        t.app.streaming_reasoning = "MAIN think marker".into();
        t.app.reasoning_open = true;

        let text = buffer_text(&mut t);
        assert!(text.contains("pane think line 39"), "pane stream rendered live");
        assert!(text.contains("thinking"), "shared draw_reasoning header present");
        assert!(!text.contains("MAIN think marker"), "main accumulator isolated");

        // Unfocused: the pane's reasoning stays in its pane.
        t.app.focus_subagent(None);
        let text = buffer_text(&mut t);
        assert!(!text.contains("pane think line 39"), "unfocused pane stream not rendered");
    }

    /// T025 (US5, FR-013): F1 and '?' open the help overlay while a pane is
    /// focused (global handler — identical content, one overlay for both
    /// views), and any dismissal key closes it. Verified-no-change pin.
    #[test]
    fn help_overlay_reachable_from_focused_pane() {
        let mut t = tui();
        spawn_pane(&mut t);
        assert!(!t.show_help);
        // F1 arm.
        t.handle_key(plain(KeyCode::F(1)));
        assert!(t.show_help, "F1 opened the help overlay from a focused pane");
        // Dismiss via '?' (same arm family as Esc/F1/q/Enter).
        t.handle_key(plain(KeyCode::Char('?')));
        assert!(!t.show_help, "'?' dismissed it");
        // '?' arm (transcript-focus path — the other global opening arm).
        t.focus = Focus::Transcript;
        t.handle_key(plain(KeyCode::Char('?')));
        assert!(t.show_help, "'?' opened the help overlay from a focused pane");
        t.handle_key(plain(KeyCode::Esc));
        assert!(!t.show_help, "Esc dismissed it");
    }

    /// T025 (US5, FR-013) content half: the overlay drawn while a pane is
    /// focused is IDENTICAL to the main-view overlay — `Tui::draw` has ONE
    /// global `draw_help_overlay` call site outside any pane/main body
    /// branch, so there is no pane-specific fork and no content drift.
    /// Pins the modal region cell-for-cell (symbols AND styling) across the
    /// two views via the REAL full-frame render path (`Tui::draw`).
    #[test]
    fn help_overlay_content_identical_from_focused_pane() {
        // Pane-focused render with the overlay armed via the global F1 arm.
        let mut pane = tui_real_draw();
        spawn_pane(&mut pane);
        pane.handle_key(plain(KeyCode::F(1)));
        assert!(pane.show_help);
        pane.draw().unwrap();
        let pane_buf = pane.terminal.backend().0.buffer().clone();

        // Main-view render: same terminal geometry + theme, no pane.
        let mut main = tui_real_draw();
        main.handle_key(plain(KeyCode::F(1)));
        assert!(main.show_help);
        main.draw().unwrap();
        let main_buf = main.terminal.backend().0.buffer().clone();

        assert_eq!(pane_buf.area(), main_buf.area(), "same TestBackend geometry");

        // The overlay modal rect, mirroring `draw_help_overlay`'s geometry:
        // w = 56.min(width), h = 26.min(height), centered — (22, 2, 56, 26)
        // on the 100×30 test terminal. Under it the two views legitimately
        // differ (pane vs main transcript); INSIDE it they must not.
        let rect = Rect::new(22, 2, 56, 26);
        let pane_text: String = (rect.y..rect.bottom())
            .flat_map(|y| (rect.x..rect.right()).map(move |x| (x, y)))
            .map(|(x, y)| pane_buf[(x, y)].symbol().to_string())
            .collect();
        assert!(pane_text.contains("closes"), "overlay rendered: {pane_text:?}");
        for y in rect.y..rect.bottom() {
            for x in rect.x..rect.right() {
                assert_eq!(
                    pane_buf[(x, y)],
                    main_buf[(x, y)],
                    "help overlay cell ({x},{y}) differs between pane and main views"
                );
            }
        }
    }
}
