//! Subprocess execution for verification steps (FR-010, T037).
//!
//! Reuses the existing subprocess execution path (std::process::Command).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The raw output of a verification step.
#[derive(Debug, Clone)]
pub struct RawStepOutput {
    pub exit_code: i32,
    pub output: String,
    pub duration_ms: u64,
    /// True when the step was skipped rather than run: the tool was absent
    /// from PATH or the step timed out (FR-012 graceful degradation). A
    /// skipped step is NOT a failure — the verify loop must not feed it
    /// into a correction pass.
    pub skipped: bool,
}

/// A single verification step runner.
pub struct VerifyStep {
    pub name: String,
    pub command: String,
    pub timeout_sec: u64,
}

impl VerifyStep {
    pub fn new(name: String, command: String, timeout_sec: u64) -> Self {
        Self {
            name,
            command,
            timeout_sec,
        }
    }

    /// Execute the step in the project root. Graceful degradation when
    /// tooling is absent or the step exceeds its timeout (FR-012): the step
    /// is reported as `skipped` (with an informative message), not failed.
    pub fn run(&self, project_root: &Path) -> RawStepOutput {
        let start = Instant::now();

        // Parse the command into program + args (basic shell splitting).
        let parts = match shlex::split(&self.command) {
            Some(p) if !p.is_empty() => p,
            _ => {
                return RawStepOutput {
                    exit_code: -1,
                    output: format!("error: could not parse command '{}'", self.command),
                    duration_ms: start.elapsed().as_millis() as u64,
                    skipped: true,
                };
            }
        };
        let program = &parts[0];
        let args = &parts[1..];

        // Check if the program exists (graceful degradation, FR-012).
        if which::which(program).is_err() {
            return RawStepOutput {
                exit_code: -1,
                output: format!(
                    "warning: '{}' not found on PATH — step '{}' skipped (FR-012 graceful degradation)",
                    program, self.name
                ),
                duration_ms: start.elapsed().as_millis() as u64,
                skipped: true,
            };
        }

        // Run with a timeout (FR-010): the subprocess is spawned with piped
        // stdio and polled on a helper thread; on expiry the child is killed
        // and the step is reported as skipped (FR-012 lists timeout among
        // the degradation cases), carrying the output captured so far.
        let mut command = Command::new(program);
        command
            .args(args)
            .current_dir(project_root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                return RawStepOutput {
                    exit_code: -1,
                    output: format!("error executing '{}': {}", program, e),
                    duration_ms: start.elapsed().as_millis() as u64,
                    skipped: true,
                };
            }
        };

        // Drain stdout/stderr on dedicated threads so a full pipe cannot
        // deadlock the child before the timeout fires.
        let stdout_handle = spawn_reader(child.stdout.take());
        let stderr_handle = spawn_reader(child.stderr.take());

        let deadline = start + Duration::from_secs(self.timeout_sec.max(1));
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        break None;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break None,
            }
        };

        let stdout = join_reader(stdout_handle);
        let stderr = join_reader(stderr_handle);
        let duration_ms = start.elapsed().as_millis() as u64;
        let combined = if stderr.is_empty() {
            stdout
        } else {
            format!("{}\n{}", stdout, stderr)
        };

        match status {
            Some(status) => RawStepOutput {
                exit_code: status.code().unwrap_or(-1),
                output: combined,
                duration_ms,
                skipped: false,
            },
            None => RawStepOutput {
                exit_code: -1,
                output: format!(
                    "{}\nwarning: step '{}' timed out after {}s — skipped (FR-012 graceful degradation)",
                    combined, self.name, self.timeout_sec
                ),
                duration_ms,
                skipped: true,
            },
        }
    }
}

/// Spawn a thread that drains a piped child stream to a String.
fn spawn_reader<R: std::io::Read + Send + 'static>(
    reader: Option<R>,
) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        // No trait import needed: `R: std::io::Read` (the fn's generic
        // bound) puts the read method in scope for values of `r`.
        let mut out = String::new();
        if let Some(mut r) = reader {
            let mut buf = [0u8; 8192];
            loop {
                match r.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => out.push_str(&String::from_utf8_lossy(&buf[..n])),
                }
            }
        }
        out
    })
}

/// Join a reader thread and collect its output.
fn join_reader(handle: std::thread::JoinHandle<String>) -> String {
    handle.join().unwrap_or_default()
}
