//! Browser discovery + managed launch (research.md D8, T011).

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::process::{Child, Command};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::cdp::BrowserError;
use crate::config::HeadlessPolicy;

/// Discovered or explicitly configured browser executable.
#[derive(Debug, Clone)]
pub struct Discovered {
    pub path: PathBuf,
    /// True when found via explicit config (skip discovery).
    pub explicit: bool,
}

/// macOS application-bundle executable candidates.
#[cfg(target_os = "macos")]
const MACOS_CANDIDATES: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
];

/// Linux/Windows PATH + known-location candidate names.
const UNIX_NAMES: &[&str] = &[
    "google-chrome",
    "google-chrome-stable",
    "chromium",
    "chromium-browser",
    "microsoft-edge",
    "brave-browser",
];

const WINDOWS_CANDIDATES: &[&str] = &[
    r"C:\Program Files\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Google\Chrome\Application\chrome.exe",
    r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe",
    r"C:\Program Files\Microsoft\Edge\Application\msedge.exe",
];

/// Is a display available for headed mode? (Principle 0: unix checks
/// DISPLAY/WAYLAND_DISPLAY; Windows always has a session display.)
pub fn display_available() -> bool {
    #[cfg(target_os = "windows")]
    {
        true
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
    }
}

/// Should the managed launch be headless under the given policy?
pub fn resolve_headless(policy: HeadlessPolicy) -> bool {
    match policy {
        HeadlessPolicy::Always => true,
        HeadlessPolicy::Never => false,
        HeadlessPolicy::Auto => !display_available(),
    }
}

/// Discover a Chromium-family browser (explicit override first).
pub fn discover(explicit: Option<&str>) -> Option<Discovered> {
    if let Some(p) = explicit {
        let path = PathBuf::from(p);
        if path.is_file() {
            return Some(Discovered { path, explicit: true });
        }
        tracing::warn!(path = %p, "browser.executable_path set but not a file; falling back to discovery");
    }
    #[cfg(target_os = "macos")]
    {
        for c in MACOS_CANDIDATES {
            if Path::new(c).is_file() {
                return Some(Discovered { path: PathBuf::from(c), explicit: false });
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        for c in WINDOWS_CANDIDATES {
            if Path::new(c).is_file() {
                return Some(Discovered { path: PathBuf::from(c), explicit: false });
            }
        }
    }
    #[cfg(unix)]
    {
        for name in UNIX_NAMES {
            if let Ok(found) = which::which(name) {
                return Some(Discovered { path: found, explicit: false });
    }
        }
    }
    None
}

/// A launched managed browser.
pub struct ManagedBrowser {
    pub child: Child,
    /// The ws debugger URL parsed from stderr.
    pub ws_url: String,
}

/// Launch a managed browser with an ephemeral debugging port.
///
/// * `--remote-debugging-port=0` → ephemeral port; the browser prints
///   `DevTools listening on ws://…` on stderr, which we parse.
/// * `--headless=new` when the resolved policy says headless.
/// * `--user-data-dir` to a unique temp dir keeps the managed profile
///   isolated from the user's own Chrome profile.
pub async fn launch_managed(
    exe: &Path,
    headless: bool,
) -> Result<ManagedBrowser, BrowserError> {
    let user_data = std::env::temp_dir().join(format!("joey-browser-profile-{}", uuid::Uuid::new_v4()));
    let mut cmd = Command::new(exe);
    cmd.args([
        "--remote-debugging-port=0",
        format!("--user-data-dir={}", user_data.display()).as_str(),
        "--no-first-run",
        "--no-default-browser-check",
        // Deterministic viewport: the coarse-grid marker strategy and fixture
        // geometry assume 1280x800 (vision.rs GRID_*).
        "--window-size=1280,800",
    ]);
    if headless {
        cmd.arg("--headless=new");
    }
    cmd.stderr(Stdio::piped()).stdout(Stdio::null()).stdin(Stdio::null());
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| BrowserError::LaunchFailed(format!("spawn {}: {e}", exe.display())))?;

    // Parse "DevTools listening on ws://HOST:PORT/devtools/browser/UUID" from stderr.
    let stderr = child.stderr.take().expect("stderr piped");
    let mut reader = BufReader::new(stderr).lines();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let line = match tokio::time::timeout_at(deadline, reader.next_line()).await {
            Ok(Ok(Some(l))) => l,
            _ => break,
        };
        if let Some(ws) = parse_devtools_ws_url(&line) {
            return Ok(ManagedBrowser { child, ws_url: ws });
        }
    }
    let _ = child.kill().await;
    Err(BrowserError::LaunchFailed(
        "browser started but never printed a DevTools listening line".into(),
    ))
}

/// Extract the ws URL from a `DevTools listening on …` stderr line.
pub fn parse_devtools_ws_url(line: &str) -> Option<String> {
    let idx = line.find("DevTools listening on ")?;
    let rest = &line[idx + "DevTools listening on ".len()..];
    let ws = rest.trim();
    ws.starts_with("ws://").then(|| ws.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stderr_parsing() {
        let line = "[12345:123:0101/000000.000000:INFO:CONSOLE(0)] DevTools listening on ws://127.0.0.1:52341/devtools/browser/a1b2c3";
        assert_eq!(
            parse_devtools_ws_url(line).as_deref(),
            Some("ws://127.0.0.1:52341/devtools/browser/a1b2c3")
        );
        assert_eq!(parse_devtools_ws_url("no url here"), None);
        assert_eq!(parse_devtools_ws_url("DevTools listening on http://nope"), None);
    }

    #[test]
    fn headless_policy_resolution() {
        assert!(resolve_headless(HeadlessPolicy::Always));
        assert!(!resolve_headless(HeadlessPolicy::Never));
        // Auto follows display availability — assert consistency, not a value.
        assert_eq!(resolve_headless(HeadlessPolicy::Auto), !display_available());
    }

    #[test]
    fn explicit_path_wins_and_missing_falls_through() {
        // Missing explicit path falls through to discovery (may or may not
        // find something on this machine) — must not error, must not panic.
        let d = discover(Some("/nonexistent/browser"));
        // On a machine with no browser, d is None; both outcomes valid.
        let _ = d;
        // A real file that is not a browser still counts as explicit if it exists.
        let me = std::env::current_exe().ok();
        if let Some(exe) = me {
            let d = discover(exe.to_str());
            assert!(d.is_some());
            assert!(d.unwrap().explicit);
        }
    }
}
