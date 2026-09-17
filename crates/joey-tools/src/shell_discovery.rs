//! Cross-platform discovery of a POSIX-capable shell for spawning
//! `bash -c <script>` style commands from native code.
//!
//! ## Why not `Command::new("bash")`?
//!
//! On Windows, `CreateProcessW` resolves a bare program name by searching
//! the application directory, the current directory, **`System32`**, the
//! Windows directory, and only then `PATH`. On a machine with WSL
//! installed, `C:\Windows\System32\bash.exe` exists — so a native spawn
//! of `"bash"` silently gets the **WSL** launcher, not Git Bash. Inside
//! WSL bash, Windows paths (`C:\...`) do not exist and Windows-side
//! binaries (`rg.exe`) are not found by name, so every spawned shell
//! snippet (search pipelines, speckit scripts) breaks with
//! `command not found` / exit 127 while working perfectly when run from
//! an interactive Git Bash session (where MSYS bash wins PATH lookup).
//!
//! The fix: resolve Git Bash explicitly — known install locations first,
//! then a `PATH` scan that rejects the WSL/System32 launcher — and use
//! the resolved absolute path for every spawn.

use std::path::PathBuf;
use once_cell::sync::Lazy;

/// Absolute path to the POSIX shell to spawn, resolved once per process
/// (mirrors `terminal_tool`'s per-process shell cache: a session must not
/// flip between shells mid-run).
static POSIX_SHELL: Lazy<String> = Lazy::new(resolve_posix_shell);

/// Resolve the POSIX shell executable for `bash -c` spawns.
///
/// Unix: `bash` from `PATH`, else `/usr/bin/bash`, `/bin/bash`, `$SHELL`,
/// `/bin/sh` (always succeeds).
///
/// Windows: the `JOEY_GIT_BASH` env override, else known Git Bash install
/// locations, else the first `bash` on `PATH` that is NOT the WSL
/// `System32` launcher, else the literal `"bash"` (best effort — the
/// spawn will fail with a clear io error if truly absent).
fn resolve_posix_shell() -> String {
    #[cfg(unix)]
    {
        if let Ok(p) = which::which("bash") {
            return p.to_string_lossy().into_owned();
        }
        for candidate in ["/usr/bin/bash", "/bin/bash"] {
            if std::path::Path::new(candidate).is_file() {
                return candidate.to_string();
            }
        }
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }

    #[cfg(windows)]
    {
        if let Some(p) = git_bash() {
            return p;
        }
        "bash".to_string()
    }
}

/// Known Git Bash install locations (MSYS2 bash), in preference order.
#[cfg(windows)]
const GIT_BASH_LOCATIONS: &[&str] = &[
    r"C:\Program Files\Git\bin\bash.exe",
    r"C:\Program Files\Git\usr\bin\bash.exe",
    r"C:\Program Files (x86)\Git\bin\bash.exe",
    r"C:\Program Files (x86)\Git\usr\bin\bash.exe",
];

/// True if the given bash path is the WSL launcher (`System32\bash.exe`
/// or a `WindowsApps` alias), which must never be used for native shell
/// spawns (see module docs).
#[cfg(windows)]
fn is_wsl_bash(path: &std::path::Path) -> bool {
    let lower = path.to_string_lossy().to_lowercase();
    lower.contains(r"\windows\system32")
        || lower.contains(r"\windowsapps\")
        || lower.contains(r"\wsl\")
}

/// Resolve Git Bash on Windows: env override, known locations, then a
/// guarded PATH scan. `None` when no non-WSL bash can be found.
///
/// Public so other subsystems (terminal tool shell resolution) can prefer
/// Git Bash over the WSL launcher with the same rules.
#[cfg(windows)]
pub fn git_bash() -> Option<String> {
    // Explicit override wins (power users / unusual install roots).
    if let Ok(p) = std::env::var("JOEY_GIT_BASH") {
        if !p.is_empty() {
            return Some(p);
        }
    }
    for candidate in GIT_BASH_LOCATIONS {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return Some(candidate.to_string());
        }
    }
    // PATH scan — `which` honors the native `;`-separated PATH, but the
    // WSL launcher in System32 is also on PATH, so filter it out.
    if let Ok(p) = which::which("bash") {
        if !is_wsl_bash(&p) {
            return Some(p.to_string_lossy().into_owned());
        }
    }
    None
}

/// The cached POSIX shell path for `bash -c` spawns (see [`POSIX_SHELL`]).
pub fn posix_shell() -> &'static str {
    &POSIX_SHELL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_shell_resolves_to_something() {
        let shell = posix_shell();
        assert!(!shell.is_empty(), "shell path must never be empty");
    }

    #[cfg(windows)]
    #[test]
    fn resolved_shell_is_never_wsl_bash() {
        let shell = posix_shell();
        assert!(
            !is_wsl_bash(std::path::Path::new(shell)),
            "resolved shell {shell} must not be the WSL System32 launcher"
        );
    }

    #[cfg(windows)]
    #[test]
    fn wsl_bash_detector() {
        assert!(is_wsl_bash(std::path::Path::new(
            r"C:\Windows\System32\bash.exe"
        )));
        assert!(is_wsl_bash(std::path::Path::new(
            r"C:\WINDOWS\system32\wsl\bash.exe"
        )));
        assert!(!is_wsl_bash(std::path::Path::new(
            r"C:\Program Files\Git\bin\bash.exe"
        )));
    }
}
