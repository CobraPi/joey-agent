//! joey-tui — the animated ratatui frontend for joey-agent.
//!
//! A "busy yet elegant" synthwave-aurora terminal UI inspired by crush's
//! structured design vocabulary but with a unique vibrant identity. Animation
//! intensity scales live with the number of active agents, giving the user
//! something beautiful to watch while turns run.
//!
//! Crate layout:
//!   - [`theme`]   — palette, semantic tokens, gradient helpers.
//!   - [`anim`]    — particle field, spinners, equalizer, pulse, activity signal.
//!   - [`state`]   — the application model (consumes [`state::App::apply`] events).
//!   - [`input`]   — a lightweight multi-line text editor.
//!   - [`widgets`] — the rendered panels.
//!   - [`app`]     — terminal lifecycle, frame composition, key → action mapping.
//!
//! The event/render loop itself lives in the host (joey-cli's `tui` module):
//! it owns the agent, pumps crossterm events into [`Tui::handle_key`], applies
//! agent events to the model, and draws at [`Tui::frame_budget`] cadence.

pub mod anim;
pub mod app;
pub mod input;
pub mod state;
pub mod theme;
pub mod widgets;

pub use app::{Tui, TuiAction};
pub use state::{App as AppState, RunMode, TranscriptItem};
pub use theme::{gradient_spans, Theme};
