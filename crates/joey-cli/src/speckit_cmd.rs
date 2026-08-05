//! `joey speckit` — launch both the speckit-ui backend and frontend together.

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use tokio::signal::ctrl_c;

/// Args for `joey speckit`
#[derive(clap::Args, Debug, Default)]
pub struct SpeckitArgs {
    /// Port for the backend server (default: 4173)
    #[arg(long = "port", short = 'p')]
    pub port: Option<u16>,

    /// Repo root to serve specs from (default: current directory)
    #[arg(long = "repo-root")]
    pub repo_root: Option<PathBuf>,

    /// Open browser automatically on startup
    #[arg(long = "open")]
    pub open: bool,
}

/// Run the speckit-ui launcher: spawn backend + frontend concurrently
pub async fn speckit_command(args: SpeckitArgs) -> Result<i32> {
    let repo_root = args
        .repo_root
        .unwrap_or_else(|| std::env::current_dir().expect("current dir"));

    let port = args.port.unwrap_or(4173);

    // Verify the web/speckit-ui directory exists
    let frontend_dir = repo_root.join("web").join("speckit-ui");
    if !frontend_dir.exists() {
        eprintln!(
            "Error: Frontend directory not found at {}",
            frontend_dir.display()
        );
        eprintln!("Ensure you're running from the joey-agent repository root.");
        return Ok(1);
    }

    // Verify package.json exists
    let package_json = frontend_dir.join("package.json");
    if !package_json.exists() {
        eprintln!(
            "Error: package.json not found at {}",
            package_json.display()
        );
        eprintln!("The frontend may not be installed. Run: cd web/speckit-ui && npm install");
        return Ok(1);
    }

    println!("🚀 Starting SpecKit UI...");
    println!("   Backend: joey-speckit-ui (port {})", port);
    println!("   Frontend: Vite dev server");
    if args.open {
        println!("   Browser: will open automatically");
    }
    println!();

    // Spawn the backend (joey-speckit-ui)
    let _backend = spawn_backend(&repo_root, port).context("Failed to spawn backend")?;

    // Give the backend a moment to start
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Spawn the frontend (npm run dev)
    let _frontend = spawn_frontend(&frontend_dir).context("Failed to spawn frontend")?;

    println!("✓ Backend and frontend are running");
    println!("  Press Ctrl+C to stop both servers");
    println!();

    // Open browser if requested
    if args.open {
        let url = format!("http://127.0.0.1:{}", port);
        if let Err(e) = open::that(&url) {
            eprintln!("Warning: Failed to open browser: {}", e);
        } else {
            println!("📖 Opened {}", url);
        }
    }

    // Wait for Ctrl+C
    match ctrl_c().await {
        Ok(()) => {
            println!("\n🛑 Shutting down...");
            // Kill processes will be handled by Drop
            Ok(0)
        }
        Err(e) => {
            eprintln!("Error waiting for Ctrl+C: {}", e);
            Ok(1)
        }
    }
}

/// Spawn the joey-speckit-ui backend
fn spawn_backend(repo_root: &PathBuf, port: u16) -> Result<BackendProcess> {
    let (backend_binary, use_cargo) = find_backend_binary()?;

    println!("  Starting backend: {}", backend_binary.display());

    let mut cmd = Command::new(&backend_binary);

    if use_cargo {
        // Run via cargo run -p joey-speckit-ui
        cmd.args(["run", "-p", "joey-speckit-ui", "--quiet"]);
    }

    let mut child = cmd
        .env("JOEY_SPECKIT_UI_ROOT", repo_root)
        .env("JOEY_SPECKIT_UI_PORT", port.to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .with_context(|| format!("Failed to spawn backend binary: {}", backend_binary.display()))?;

    // Give it a moment to ensure it started
    std::thread::sleep(std::time::Duration::from_millis(100));

    if let Some(status) = child.try_wait()? {
        return Err(anyhow::anyhow!(
            "Backend exited immediately with status: {:?}",
            status
        ));
    }

    Ok(BackendProcess {
        child: Some(child),
        _binary: backend_binary,
    })
}

/// Spawn the Vite frontend dev server
fn spawn_frontend(frontend_dir: &PathBuf) -> Result<FrontendProcess> {
    // Check if npm is available
    let npm = which::which("npm").context("npm not found. Please install Node.js and npm.")?;

    println!("  Starting frontend: npm run dev");

    let mut child = Command::new(&npm)
        .arg("run")
        .arg("dev")
        .current_dir(frontend_dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .context("Failed to spawn npm run dev")?;

    // Give it a moment to ensure it started
    std::thread::sleep(std::time::Duration::from_millis(100));

    if let Some(status) = child.try_wait()? {
        return Err(anyhow::anyhow!(
            "Frontend exited immediately with status: {:?}",
            status
        ));
    }

    Ok(FrontendProcess {
        child: Some(child),
    })
}

/// Find the joey-speckit-ui binary
fn find_backend_binary() -> Result<(PathBuf, bool)> {
    // First, try the cargo build output directory
    let target_dir = std::env::current_dir()
        .context("Failed to get current directory")?
        .join("target")
        .join("debug");

    let binary_name = if cfg!(target_os = "windows") {
        "joey-speckit-ui.exe"
    } else {
        "joey-speckit-ui"
    };

    let binary_path = target_dir.join(binary_name);

    if binary_path.exists() {
        return Ok((binary_path, false));
    }

    // Try running from cargo directly
    if let Ok(cargo) = which::which("cargo") {
        // We'll run via cargo run instead
        return Ok((cargo, true));
    }

    Err(anyhow::anyhow!(
        "Could not find joey-speckit-ui binary at {}.
Please build it first: cargo build -p joey-speckit-ui",
        binary_path.display()
    ))
}

/// Wrapper for the backend process that kills it on drop
struct BackendProcess {
    child: Option<Child>,
    _binary: PathBuf,
}

impl Drop for BackendProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Wrapper for the frontend process that kills it on drop
struct FrontendProcess {
    child: Option<Child>,
}

impl Drop for FrontendProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
