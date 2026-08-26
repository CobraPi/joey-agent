# Contract: joey-browser Public API (internal crate contract)

Feature: specs/016-please-modify-joey | `joey-browser` crate public surface consumed by joey-tools. Constitution VI: narrow, explicit interface; all CDP detail hides behind it. This is an internal-workspace contract but versioned like a public API (Principle VII) since joey-tools depends on it.

## Module layout (public re-exports via lib.rs)

```rust
pub mod cdp;        // transport — public only for tests + browser_cdp passthrough
pub mod launch;     // discovery + managed launch
pub mod session;    // BrowserManager, PageSession
pub mod snapshot;   // Snapshot, ElementRef, RegionSummary, Blocker, Delta, budgets
pub mod refs;       // FallbackResolver, ResolvedBy
pub mod actions;    // verb execution
pub mod vision;     // VisualObservation, SoM
pub mod config;     // BrowserConfig (resolved from joey-core dotted keys)
```

## Primary API (what joey-tools calls)

```rust
pub struct BrowserManager { /* … */ }

impl BrowserManager {
    /// Attach to a running browser at `cdp_url` (default http://127.0.0.1:9222),
    /// or launch managed when absent. Headless when no display and cfg says auto.
    pub async fn connect(cfg: BrowserConfig) -> Result<Self, BrowserError>;

    /// Ensure the agent's dedicated tab exists (creates via Target.createTarget
    /// on first use). Never touches user tabs.
    pub async fn ensure_page(&self) -> Result<PageRef, BrowserError>;

    pub async fn disconnect(self) -> Result<(), BrowserError>;   // kills child iff Managed
    pub fn status(&self) -> BrowserStatus;                       // mode, endpoint, page url/title

    pub async fn navigate(&self, url: &str) -> Result<NavResult, BrowserError>;
    pub async fn back(&self) -> Result<NavResult, BrowserError>;

    pub async fn snapshot(&self, opts: SnapshotOpts) -> Result<Snapshot, BrowserError>;
    pub async fn act(&self, action: AgentAction) -> Result<ActionResult, BrowserError>;
    pub async fn vision(&self, prompt: Option<&str>) -> Result<VisualObservation, BrowserError>;
    pub async fn console(&self) -> Result<ConsoleBuffer, BrowserError>;
    pub async fn handle_dialog(&self, action: DialogAction) -> Result<DialogResult, BrowserError>;
    pub async fn raw_cdp(&self, method: &str, params: Value) -> Result<Value, BrowserError>;
    pub async fn images(&self) -> Result<Vec<ImageInfo>, BrowserError>;
}
```

## Error type (normative variants)

```rust
pub enum BrowserError {
    NotConnected, AttachFailed(String), LaunchFailed(String), NoBrowserFound,
    PageGone,        // dedicated tab closed/crashed — recoverable via ensure_page
    TargetNotFound { candidates: Vec<ElementRef> },  // FR-007 refusal with candidates
    Ambiguous { candidates: Vec<ElementRef> },
    SettleTimeout { waited_ms: u64 },                // still returns partial state
    UrlBlocked(String),                              // from url_safety
    RawCdpDisabled,
    Protocol(String), Timeout(String),
}
```

## Guarantees (testable)

1. `connect` never requires a display when `headless=auto` and none exists.
2. `ensure_page` creates at most one agent tab per manager; idempotent.
3. All `act` verbs re-scan before executing; result reports `ResolvedBy`.
4. Navigations blocked by URL-safety return `UrlBlocked` before any CDP call.
5. `disconnect` on Managed kills the child process (no orphans); on Attached leaves the browser running.
6. All serialized output is char-boundary-safe truncated (repo audit lesson).
7. No DOM mutation for element identification (D2) — verifiable: scanner runs with MutationObserver disabled and DOM remains attribute-identical.
7b. Settle observer and marker overlay inject scripts whose own mutations are excluded from settle detection (marked with a sentinel).
8. Refids are unique within a snapshot and never reused across scans for different elements.

## Transport notes (cdp module, for tests/passthrough)

- One WebSocket to the browser endpoint; flat session mux keyed by `sessionId`.
- `Target.setAutoAttach { flatten: true }` drives per-frame sessions (D3).
- Canned-JSON unit tests for frame/finder/protocol errors; no live browser needed (live tests gated).
