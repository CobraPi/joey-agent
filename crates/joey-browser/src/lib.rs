//! joey-browser: CDP-driven browser automation for joey-agent (feature 016).
//!
//! Owns the full browser control plane: attach to the user's running
//! Chromium-family browser (preserving logins) or auto-launch a managed
//! instance (headless when no display); deep DOM perception piercing shadow
//! roots and frames; resilient actions with a cascading fallback resolver;
//! settle detection; overlay handling; Set-of-Mark visual fallback; bounded
//! feed deltas. See specs/016-please-modify-joey/ for the design.
//!
//! All CDP wire detail is encapsulated here — joey-tools consumes only the
//! narrow [`session::BrowserManager`] API (contracts/cdp-session.md).

pub mod actions;
pub mod cdp;
pub mod config;
pub mod extract;
pub mod launch;
pub mod refs;
pub mod session;
pub mod snapshot;
pub mod url_safety_bridge;
pub mod vision;

pub use cdp::BrowserError;
pub use config::BrowserConfig;
pub use refs::{ElementRef, ResolvedBy};
pub use session::{BrowserManager, BrowserStatus, Mode};
pub use snapshot::{Blocker, Delta, RegionSummary, Snapshot};
pub use snapshot::VisualObservation;
