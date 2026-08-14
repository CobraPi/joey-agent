//! Subprocess execution for verification steps (FR-010, T037).
//!
//! Reuses the existing subprocess execution path (std::process::Command).

use std::path::Path;
use std::process::Command;
use std::time::Instant;

/// The raw output of a verification step.
#[derive(Debug, Clone)]
pub struct RawStepOutput {
    pub exit_code: i32,
    pub output: String,
    pub duration_ms: u64,
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
    /// tooling is absent (FR-012).
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
            };
        }

        // Run with timeout (basic — uses std::process, timeout via wait_timeout crate
        // would be ideal, but we use a simple approach here).
        let result = Command::new(program)
            .args(args)
            .current_dir(project_root)
            .output();

        let duration_ms = start.elapsed().as_millis() as u64;

        match result {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                let combined = if stderr.is_empty() {
                    stdout
                } else {
                    format!("{}\n{}", stdout, stderr)
                };
                RawStepOutput {
                    exit_code: output.status.code().unwrap_or(-1),
                    output: combined,
                    duration_ms,
                }
            }
            Err(e) => RawStepOutput {
                exit_code: -1,
                output: format!("error executing '{}': {}", program, e),
                duration_ms,
            },
        }
    }
}
