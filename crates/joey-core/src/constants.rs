//! Shared constants and path resolution (port of upstream `hermes_constants.py`).
//!
//! Import-safe module with no heavy dependencies.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};

use crate::branding;

/// Response ID for partial stream stubs used during error recovery.
pub const PARTIAL_STREAM_STUB_ID: &str = "partial-stream-stub";
pub const FINISH_REASON_LENGTH: &str = "length";

pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

static HOME_OVERRIDE: RwLock<Option<PathBuf>> = RwLock::new(None);
static PROFILE_FALLBACK_WARNED: AtomicBool = AtomicBool::new(false);

/// Set a process-local home override (used for per-profile scoping) and
/// return the previous value. Upstream uses a Python `ContextVar` for
/// per-task scoping; the Rust port scopes per process, which matches how
/// the CLI and gateway spawn profile work into separate processes.
pub fn set_home_override(path: Option<PathBuf>) -> Option<PathBuf> {
    let mut guard = HOME_OVERRIDE.write().expect("home override lock");
    std::mem::replace(&mut guard, path)
}

pub fn get_home_override() -> Option<PathBuf> {
    HOME_OVERRIDE.read().expect("home override lock").clone()
}

/// RAII guard that restores the previous home override when dropped.
pub struct HomeOverrideGuard {
    previous: Option<PathBuf>,
}

impl HomeOverrideGuard {
    pub fn new(path: PathBuf) -> Self {
        let previous = set_home_override(Some(path));
        Self { previous }
    }
}

impl Drop for HomeOverrideGuard {
    fn drop(&mut self) {
        set_home_override(self.previous.take());
    }
}

fn platform_default_home() -> PathBuf {
    #[cfg(windows)]
    {
        let base = std::env::var("LOCALAPPDATA")
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home_dir().join("AppData").join("Local"));
        base.join(branding::WINDOWS_DIR_NAME)
    }
    #[cfg(not(windows))]
    {
        user_home_dir().join(branding::HOME_DIR_NAME)
    }
}

/// The OS user's home directory (`~`), independent of any joey profile.
pub fn user_home_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn home_from_env() -> PathBuf {
    match std::env::var(branding::ENV_HOME) {
        Ok(val) if !val.trim().is_empty() => PathBuf::from(val.trim()),
        _ => platform_default_home(),
    }
}

fn warn_profile_fallback_once() {
    if PROFILE_FALLBACK_WARNED.swap(true, Ordering::SeqCst) {
        return;
    }
    let fallback_home = platform_default_home();
    let active_path = fallback_home.join("active_profile");
    let active = std::fs::read_to_string(&active_path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if !active.is_empty() && active != "default" {
        // Straight to stderr: called before logging may be configured.
        eprintln!(
            "[{} fallback] {} is unset but active profile is {:?}. Falling back to {}, \
             which is the DEFAULT profile — not {:?}. Any data this process writes will \
             land in the wrong profile. The subprocess spawner should pass {} explicitly.",
            branding::ENV_HOME,
            branding::ENV_HOME,
            active,
            fallback_home.display(),
            active,
            branding::ENV_HOME,
        );
    }
}

/// Return the joey home directory. Resolution order: process-local override →
/// `JOEY_HOME` env var → platform-native default (`~/.joey`).
pub fn joey_home() -> PathBuf {
    if let Some(over) = get_home_override() {
        return over;
    }
    let env_set = std::env::var(branding::ENV_HOME)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    if !env_set {
        warn_profile_fallback_once();
    }
    home_from_env()
}

/// Return the joey home for the running process, ignoring the override.
/// Use for machine/process-level assets that must not follow per-profile scoping.
pub fn process_joey_home() -> PathBuf {
    home_from_env()
}

/// Return the root joey directory for profile-level operations.
///
/// Standard installs: the platform-native home. Custom `JOEY_HOME` outside
/// the native home: that path itself, unless it is `<root>/profiles/<name>`
/// in which case the grandparent is the root.
pub fn default_root() -> PathBuf {
    let native_home = platform_default_home();
    let env_home = std::env::var(branding::ENV_HOME).unwrap_or_default();
    if env_home.trim().is_empty() {
        return native_home;
    }
    let env_path = PathBuf::from(env_home.trim());
    let native_canon = native_home.canonicalize().unwrap_or(native_home.clone());
    if let Ok(env_canon) = env_path.canonicalize() {
        if env_canon.starts_with(&native_canon) {
            return native_home;
        }
    } else if env_path.starts_with(&native_home) {
        return native_home;
    }
    if env_path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n == "profiles")
        .unwrap_or(false)
    {
        if let Some(root) = env_path.parent().and_then(|p| p.parent()) {
            return root.to_path_buf();
        }
    }
    env_path
}

/// True iff `path` exists and has content worth honouring (see upstream
/// `_legacy_path_has_content`): populated dirs and plain files count, empty
/// dirs and dangling symlinks don't, uninspectable paths count (don't orphan).
pub fn legacy_path_has_content(path: &Path) -> bool {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    if meta.file_type().is_symlink() {
        match std::fs::metadata(path) {
            Ok(target) if target.is_dir() => {}
            Ok(_) => return true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
            Err(_) => return true,
        }
    } else if !meta.is_dir() {
        return true;
    }
    match std::fs::read_dir(path) {
        Ok(mut iter) => iter.next().is_some(),
        Err(_) => true,
    }
}

/// Resolve a joey subdirectory with backward compatibility: prefer the
/// legacy location when it exists with content, else the new layout.
pub fn joey_dir(new_subpath: &str, old_name: &str) -> PathBuf {
    let home = joey_home();
    let old_path = home.join(old_name);
    if legacy_path_has_content(&old_path) {
        return old_path;
    }
    home.join(new_subpath)
}

/// User-friendly display string for the current home (`~/.joey` style).
pub fn display_joey_home() -> String {
    let home = joey_home();
    if let Some(user_home) = dirs::home_dir() {
        if let Ok(rel) = home.strip_prefix(&user_home) {
            let rel_str = rel.to_string_lossy();
            return if rel_str.is_empty() {
                "~/".to_string()
            } else {
                format!("~/{}", rel_str)
            };
        }
    }
    home.display().to_string()
}

/// Chmod `0o700` on the parent directory of `path`, but only if safe.
/// Refuses `/` and top-level directories to avoid bricking hosts when a
/// path env var resolves somewhere unexpected.
pub fn secure_parent_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let parent = match path.parent() {
            Some(p) => p.canonicalize().unwrap_or_else(|_| p.to_path_buf()),
            None => return,
        };
        if parent == Path::new("/") || parent.components().count() < 3 {
            return;
        }
        let _ = std::fs::set_permissions(&parent, std::fs::Permissions::from_mode(0o700));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

// ─── Well-Known Paths ────────────────────────────────────────────────────────

/// Path to `config.yaml` under the joey home.
pub fn config_path() -> PathBuf {
    joey_home().join("config.yaml")
}

/// Path to the skills directory under the joey home.
pub fn skills_dir() -> PathBuf {
    joey_home().join("skills")
}

/// Path to the `.env` file under the joey home.
pub fn env_path() -> PathBuf {
    joey_home().join(".env")
}

fn packaged_data_dir(name: &str) -> Option<PathBuf> {
    // Wheel data-files have no Rust analog; honor an exe-adjacent share dir
    // (used by packaged installs: <prefix>/share/joey-agent/<name>).
    let exe = std::env::current_exe().ok()?;
    let prefix = exe.parent()?.parent()?;
    let candidate = prefix.join("share").join(branding::PACKAGE_NAME).join(name);
    candidate.exists().then_some(candidate)
}

fn env_dir_override(var: &str) -> Option<PathBuf> {
    std::env::var(var)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// The optional-skills directory, honoring package-manager overrides.
pub fn optional_skills_dir(default: Option<&Path>) -> PathBuf {
    env_dir_override(branding::ENV_OPTIONAL_SKILLS)
        .or_else(|| packaged_data_dir("optional-skills"))
        .or_else(|| default.map(Path::to_path_buf))
        .unwrap_or_else(|| joey_home().join("optional-skills"))
}

/// The optional-mcps directory (approved MCP servers shipped with the repo).
pub fn optional_mcps_dir(default: Option<&Path>) -> PathBuf {
    env_dir_override(branding::ENV_OPTIONAL_MCPS)
        .or_else(|| packaged_data_dir("optional-mcps"))
        .or_else(|| default.map(Path::to_path_buf))
        .unwrap_or_else(|| joey_home().join("optional-mcps"))
}

/// The bundled skills directory for source and packaged installs.
pub fn bundled_skills_dir(default: Option<&Path>) -> PathBuf {
    env_dir_override(branding::ENV_BUNDLED_SKILLS)
        .or_else(|| packaged_data_dir("skills"))
        .or_else(|| default.map(Path::to_path_buf))
        .unwrap_or_else(|| joey_home().join("skills"))
}

// ─── Environment Detection ───────────────────────────────────────────────────

/// True when running inside a Termux (Android) environment.
pub fn is_termux() -> bool {
    if std::env::var("TERMUX_VERSION").map(|v| !v.is_empty()).unwrap_or(false) {
        return true;
    }
    std::env::var("PREFIX")
        .map(|p| p.contains("com.termux/files/usr"))
        .unwrap_or(false)
}

/// True when running inside WSL. Cached for the process lifetime.
pub fn is_wsl() -> bool {
    static DETECTED: OnceLock<bool> = OnceLock::new();
    *DETECTED.get_or_init(|| {
        std::fs::read_to_string("/proc/version")
            .map(|v| v.to_lowercase().contains("microsoft"))
            .unwrap_or(false)
    })
}

/// True when running inside a container (Docker, Podman, Kubernetes, LXC).
/// Cached for the process lifetime.
pub fn is_container() -> bool {
    static DETECTED: OnceLock<bool> = OnceLock::new();
    *DETECTED.get_or_init(|| {
        if Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists() {
            return true;
        }
        if std::env::var("KUBERNETES_SERVICE_HOST").map(|v| !v.is_empty()).unwrap_or(false) {
            return true;
        }
        const CGROUP_MARKERS: [&str; 6] =
            ["docker", "podman", "/lxc/", "kubepods", "containerd", "crio"];
        if let Ok(cgroup) = std::fs::read_to_string("/proc/1/cgroup") {
            if CGROUP_MARKERS.iter().any(|m| cgroup.contains(m)) {
                return true;
            }
        }
        // cgroup v2 collapses /proc/1/cgroup to "0::/"; the runtime still
        // shows up in the mount table.
        if let Ok(mountinfo) = std::fs::read_to_string("/proc/self/mountinfo") {
            if ["kubepods", "containerd", "crio"].iter().any(|m| mountinfo.contains(m)) {
                return true;
            }
        }
        false
    })
}

/// Convert a Windows drive path (`C:\...`) to its `/mnt/<drive>/...` form.
pub fn windows_path_to_wsl(path: &str) -> Option<String> {
    let trimmed = path.trim();
    let mut chars = trimmed.chars();
    let drive = chars.next()?;
    if !drive.is_ascii_alphabetic() || chars.next()? != ':' {
        return None;
    }
    let sep = chars.next()?;
    if sep != '\\' && sep != '/' {
        return None;
    }
    let tail: String = chars.collect::<String>().replace('\\', "/");
    Some(format!("/mnt/{}/{}", drive.to_ascii_lowercase(), tail))
}

/// Convert a `\\wsl.localhost\<distro>\...` (or legacy `\\wsl$\...`) UNC path
/// to a POSIX path inside the distro.
pub fn wsl_unc_path_to_posix(path: &str) -> Option<String> {
    let normalized = path.trim().replace('/', "\\");
    let lower = normalized.to_lowercase();
    let rest = if let Some(r) = lower.strip_prefix("\\\\wsl.localhost\\") {
        &normalized[normalized.len() - r.len()..]
    } else if let Some(r) = lower.strip_prefix("\\\\wsl$\\") {
        &normalized[normalized.len() - r.len()..]
    } else {
        return None;
    };
    let mut parts = rest.splitn(2, '\\');
    let _distro = parts.next()?;
    let tail = parts.next().unwrap_or("").replace('\\', "/");
    Some(if tail.is_empty() { "/".to_string() } else { format!("/{}", tail) })
}

/// Normalize a cross-boundary cwd when joey itself runs inside WSL.
pub fn translate_cwd_for_wsl_backend(cwd: &str) -> String {
    if !is_wsl() {
        return cwd.to_string();
    }
    if let Some(t) = wsl_unc_path_to_posix(cwd) {
        return t;
    }
    if let Some(t) = windows_path_to_wsl(cwd) {
        return t;
    }
    cwd.to_string()
}

// ─── Subprocess HOME Contract ────────────────────────────────────────────────

fn norm_home_path(path: &str) -> String {
    let raw = path.trim();
    if raw.is_empty() {
        return String::new();
    }
    let expanded = shellexpand::tilde(raw).to_string();
    let abs = std::fs::canonicalize(&expanded).unwrap_or_else(|_| PathBuf::from(&expanded));
    let s = abs.to_string_lossy().to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

fn profile_home_path(env: Option<&indexmap::IndexMap<String, String>>) -> Option<String> {
    let home = get_home_override()
        .map(|p| p.to_string_lossy().to_string())
        .or_else(|| env.and_then(|e| e.get(branding::ENV_HOME).cloned()))
        .or_else(|| std::env::var(branding::ENV_HOME).ok())
        .filter(|s| !s.trim().is_empty())?;
    let profile_home = Path::new(&home).join("home");
    profile_home
        .is_dir()
        .then(|| profile_home.to_string_lossy().to_string())
}

fn env_or_process(env: Option<&indexmap::IndexMap<String, String>>, key: &str) -> String {
    env.and_then(|e| e.get(key).cloned())
        .or_else(|| std::env::var(key).ok())
        .unwrap_or_default()
        .trim()
        .to_string()
}

fn real_home_candidates(env: Option<&indexmap::IndexMap<String, String>>) -> Vec<String> {
    let mut candidates = Vec::new();
    let explicit = env_or_process(env, branding::ENV_REAL_HOME);
    if !explicit.is_empty() {
        candidates.push(explicit);
    }
    let home = env_or_process(env, "HOME");
    if !home.is_empty() {
        candidates.push(home);
    }
    #[cfg(unix)]
    {
        if let Some(dir) = dirs::home_dir() {
            candidates.push(dir.to_string_lossy().to_string());
        }
    }
    let userprofile = env_or_process(env, "USERPROFILE");
    if !userprofile.is_empty() {
        candidates.push(userprofile);
    }
    let drive = env_or_process(env, "HOMEDRIVE");
    let hpath = env_or_process(env, "HOMEPATH");
    if !drive.is_empty() && !hpath.is_empty() {
        candidates.push(format!("{}{}", drive, hpath));
    }
    candidates
}

/// Return the OS user's real home directory, avoiding the joey profile HOME.
pub fn get_real_home(env: Option<&indexmap::IndexMap<String, String>>) -> String {
    let profile_home = profile_home_path(env);
    let mut seen = std::collections::HashSet::new();
    for candidate in real_home_candidates(env) {
        let key = norm_home_path(&candidate);
        if key.is_empty() || !seen.insert(key.clone()) {
            continue;
        }
        let is_profile = profile_home
            .as_deref()
            .map(|ph| norm_home_path(ph) == key)
            .unwrap_or(false);
        if !is_profile {
            return candidate;
        }
    }
    "/tmp".to_string()
}

/// Return a subprocess `HOME` override, if one should be applied.
/// Policy is controlled by `terminal.home_mode` (bridged to `TERMINAL_HOME_MODE`):
/// `auto` (default), `real`, or `profile`.
pub fn get_subprocess_home(env: Option<&indexmap::IndexMap<String, String>>) -> Option<String> {
    let profile_home = profile_home_path(env);
    let mut mode = env_or_process(env, "TERMINAL_HOME_MODE").to_lowercase();
    if mode.is_empty() {
        mode = "auto".to_string();
    }
    mode = match mode.as_str() {
        "isolated" | "profile_home" | "profile-home" => "profile".to_string(),
        "host" | "user" | "real_home" | "real-home" => "real".to_string(),
        other => other.to_string(),
    };

    if mode == "profile" {
        return profile_home;
    }

    let real_home = get_real_home(env);
    let current_home = env_or_process(env, "HOME");
    if mode == "real" {
        return (norm_home_path(&real_home) != norm_home_path(&current_home)).then_some(real_home);
    }

    if profile_home.is_some() && is_container() {
        return profile_home;
    }
    let current_is_profile = profile_home
        .as_deref()
        .map(|ph| norm_home_path(ph) == norm_home_path(&current_home))
        .unwrap_or(false);
    if current_is_profile {
        return (norm_home_path(&real_home) != norm_home_path(&current_home)).then_some(real_home);
    }
    None
}

/// Apply joey's subprocess HOME contract to `env` in place.
pub fn apply_subprocess_home_env(env: &mut indexmap::IndexMap<String, String>) {
    let real_home = get_real_home(Some(env));
    if !real_home.is_empty() {
        env.insert(branding::ENV_REAL_HOME.to_string(), real_home);
    }
    if let Some(home) = get_subprocess_home(Some(env)) {
        env.insert("HOME".to_string(), home);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_path_conversion() {
        assert_eq!(
            windows_path_to_wsl("C:\\Users\\joey\\code"),
            Some("/mnt/c/Users/joey/code".to_string())
        );
        assert_eq!(windows_path_to_wsl("/already/posix"), None);
    }

    #[test]
    fn wsl_unc_conversion() {
        assert_eq!(
            wsl_unc_path_to_posix("\\\\wsl.localhost\\Ubuntu\\home\\joey"),
            Some("/home/joey".to_string())
        );
        assert_eq!(
            wsl_unc_path_to_posix("\\\\wsl$\\Ubuntu\\"),
            Some("/".to_string())
        );
        assert_eq!(wsl_unc_path_to_posix("C:\\nope"), None);
    }

    #[test]
    fn home_override_guard_restores() {
        let before = get_home_override();
        {
            let _guard = HomeOverrideGuard::new(PathBuf::from("/tmp/joey-test-profile"));
            assert_eq!(get_home_override(), Some(PathBuf::from("/tmp/joey-test-profile")));
        }
        assert_eq!(get_home_override(), before);
    }
}
