# SpecKit UI Launcher - Feature Summary

## Overview

The `joey speckit` command has been added to launch both the SpecKit Visual UI backend and frontend together with a single command. This eliminates the need to run two separate terminals during development.

## Quick Start

```bash
# Build the workspace (first time)
cargo build --workspace

# Launch both backend and frontend
./target/debug/joey speckit

# Or install and use globally
cargo install --path .
joey speckit
```

## Command Options

```bash
joey speckit [OPTIONS]

Options:
  -p, --port <PORT>            Port for the backend server (default: 4173)
      --repo-root <REPO_ROOT>  Repo root to serve specs from (default: current directory)
      --open                   Open browser automatically on startup
  -h, --help                   Print help
```

## What Changed

### New Files Created

1. **`crates/joey-cli/src/speckit_cmd.rs`** - The launcher implementation
   - Spawns both backend and frontend processes
   - Manages process lifecycle (cleanup on exit)
   - Handles Ctrl+C gracefully
   - Auto-detects whether to use pre-built binary or `cargo run`

2. **`docs/speckit-ui-launcher.md`** - Detailed documentation

3. **`test_speckit_integration.sh`** - Integration test script

### Files Modified

1. **`crates/joey-cli/src/main.rs`**
   - Added `speckit_cmd` module
   - Added `Speckit` command variant
   - Added command handler
   - Updated help text

2. **`crates/joey-cli/Cargo.toml`**
   - Added `open = "5"` dependency for browser auto-opening

## Implementation Details

### Backend Spawning

The launcher first looks for a pre-built binary at `target/debug/joey-speckit-ui`. If found, it runs it directly. If not found, it falls back to `cargo run -p joey-speckit-ui --quiet`.

### Frontend Spawning

The launcher runs `npm run dev` in the `web/speckit-ui` directory. This requires:
- Node.js and npm installed
- Frontend dependencies installed (`cd web/speckit-ui && npm install`)

### Process Management

Both processes are wrapped in structs with `Drop` implementations that ensure they are killed when the launcher exits. This prevents zombie processes.

## Testing

Run the integration test script:

```bash
./test_speckit_integration.sh
```

This verifies:
- Command exists and shows help
- Command is listed in main help
- Backend binary exists (or cargo is available)
- Frontend directory exists
- Frontend dependencies are installed
- package.json exists

## Before vs After

### Before (Two terminals required)

```bash
# Terminal 1
cargo run -p joey-speckit-ui

# Terminal 2
cd web/speckit-ui
npm run dev
```

### After (Single command)

```bash
joey speckit
```

## Troubleshooting

### "npm not found"
Install Node.js from https://nodejs.org/

### "Frontend dependencies not installed"
```bash
cd web/speckit-ui
npm install
```

### "Port already in use"
```bash
joey speckit --port 3000
```

### "Backend binary not found"
Either build it or let the launcher use `cargo run`:
```bash
cargo build -p joey-speckit-ui
```

## Architecture

```
joey speckit (CLI command)
    ├── spawn_backend() → joey-speckit-ui (Rust, HTTP + WebSocket)
    └── spawn_frontend() → npm run dev (Vite, TypeScript)
```

Both processes run concurrently and are managed by the launcher. Pressing Ctrl+C shuts down both gracefully.

## Future Enhancements

Potential improvements for future iterations:

1. **Hot reload detection** - Automatically restart backend on Rust code changes
2. **Health checks** - Verify both servers started successfully
3. **Log separation** - Optionally separate backend/frontend logs
4. **Port auto-detection** - Find an available port if default is in use
5. **Production mode** - Support running built frontend instead of dev server
6. **Configuration file** - Store default port, repo root, etc. in config

## Dependencies

### Added to `joey-cli/Cargo.toml`
- `open = "5"` - For opening the browser automatically

### External dependencies required
- Node.js and npm (for frontend)
- Frontend dependencies in `web/speckit-ui/node_modules`

## Compatibility

- Works on macOS, Linux, and Windows
- Requires Rust toolchain for backend
- Requires Node.js for frontend
- Backward compatible - doesn't affect any existing joey commands
