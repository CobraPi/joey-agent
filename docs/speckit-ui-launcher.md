# SpecKit UI Launcher

The `joey speckit` command launches both the SpecKit Visual UI backend and frontend together with a single command.

## Usage

```bash
# Start both backend and frontend
joey speckit

# Start with a custom port
joey speckit --port 3000

# Start with a custom repo root
joey speckit --repo-root /path/to/repo

# Start and open browser automatically
joey speckit --open

# Combine options
joey speckit --port 3000 --open --repo-root /path/to/repo
```

## What it does

1. **Spawns the backend** (`joey-speckit-ui`) on the specified port (default: 4173)
   - Uses the pre-built binary if available at `target/debug/joey-speckit-ui`
   - Falls back to `cargo run -p joey-speckit-ui` if the binary isn't built
   - Sets `JOEY_SPECKIT_UI_ROOT` and `JOEY_SPECKIT_UI_PORT` environment variables

2. **Spawns the frontend** (Vite dev server)
   - Runs `npm run dev` in the `web/speckit-ui` directory
   - Requires Node.js and npm to be installed
   - Requires frontend dependencies to be installed (`cd web/speckit-ui && npm install`)

3. **Manages both processes**
   - Both processes run concurrently
   - Pressing `Ctrl+C` gracefully shuts down both servers
   - Processes are automatically cleaned up on exit

## Prerequisites

- Rust toolchain (for the backend)
- Node.js and npm (for the frontend)
- Frontend dependencies installed: `cd web/speckit-ui && npm install`

## Development workflow

Instead of running two separate commands in different terminals:

```bash
# Old way (two terminals)
# Terminal 1
cargo run -p joey-speckit-ui

# Terminal 2
cd web/speckit-ui
npm run dev
```

You can now use a single command:

```bash
# New way (one terminal)
joey speckit
```

## Implementation details

The launcher is implemented in `crates/joey-cli/src/speckit_cmd.rs` and integrates with the CLI via:

1. A new `Speckit` command variant in the `Command` enum
2. A handler that spawns both processes concurrently
3. `Drop` implementations that ensure processes are cleaned up

## Troubleshooting

### Backend not found

If you see an error about the backend binary not being found, either:
- Build it: `cargo build -p joey-speckit-ui`
- Or let the launcher use `cargo run` (slower startup but works)

### Frontend dependencies not installed

If you see an error about `package.json` or missing dependencies:

```bash
cd web/speckit-ui
npm install
```

### Port already in use

If the default port (4173) is already in use, specify a different one:

```bash
joey speckit --port 3000
```

### npm not found

Ensure Node.js and npm are installed and available in your PATH:

```bash
node --version
npm --version
```
