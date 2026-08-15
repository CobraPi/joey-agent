# joey-mcp — Model Context Protocol Client

Stdio JSON-RPC 2.0 MCP client (port of upstream `tools/mcp_tool.py` client
side). Spawns each MCP server as a subprocess with a filtered environment,
performs the `initialize` handshake (protocol versions `2024-11-05` …
`2025-11-25`), lists tools with `nextCursor` pagination (capped at 50
pages), and calls them.

## Tool naming

Discovered tools are exposed to the agent as `mcp__<server>__<tool>`.
Provenance is tracked in a per-client map, never parsed back out of the
name. **Collision safety**: server names that sanitize identically
(`my-server` vs `my_server` vs `my.server`) are deterministically
disambiguated at connect time via a process-global wire-prefix registry —
the first claimant keeps the plain prefix, later ones get `_2`, `_3`
suffixes — with a retrievable warning (`McpClient::take_warnings`) so the
CLI can surface the rename. Without this, colliding servers silently
rerouted each other's tool calls.

**Frame safety**: server-framed JSON-RPC lines are read with a 32 MiB cap
(`read_line_bounded`) — a hostile or buggy server streaming an unbounded
line fails the call cleanly instead of growing memory without limit.

## Configuration

`mcp_servers:` in `~/.joey/config.yaml` (gated on `JOEY_SAFE_MODE`;
`${ENV_VAR}` interpolation supported). Project-level
`.joey/mcp.json` / `.mcp.json` are also read. Per-server keys:

| Key | Meaning |
|---|---|
| `command` | executable to spawn (stdio transport) |
| `args` | argument list (`--args` in the CLI, must come last) |
| `env` | environment for the subprocess (filtered) |
| `url`, `headers`, `transport: sse` | HTTP/SSE servers — transport itself not yet ported |
| `timeout` | per-call timeout (default 300s) |
| `connect_timeout` | handshake timeout (default 60s) |
| `keepalive_interval` | keepalive ping cadence |
| `idle_timeout_seconds` / `max_lifetime_seconds` | process recycling |
| `supports_parallel_tool_calls` | declare parallel safety |
| `tools.include` / `tools.exclude` | tool filtering |
| `sampling` | sampling config passthrough |

## Security and sanitization

- Server entries pass `validate_mcp_server_entry` (suspicious-entry
  detection for prompt-injection-style config) before use — the CLI refuses
  to add a suspicious config.
- Tool-call results and errors are rendered into upstream's
  `{"result": ...}` / `{"error": ...}` envelope with error sanitization.
- Input schemas are normalized (`normalize_mcp_input_schema`,
  `strip_nullable_unions`) before being shown to the model.
- All `mcp_*` tool results are wrapped as untrusted content by the agent
  dispatch layer (see [security.md](security.md)).

## CLI (`joey mcp ...`)

- `add <name> (--url URL [--transport T] | --command CMD [--env K=V]...
  [--args arg...]) [--connect-timeout S]` — writes `mcp_servers.<name>` to
  config.yaml; a child `--profile` inside `--args` is respected as the
  server's own flag
- `remove|rm <name>`, `list|ls` (table), `test <name>` (spawn + handshake +
  discovery with timing)
- `catalog` / `login` / `reauth` / `picker` / `install` / `configure` /
  `serve` — recognized but deferred (exit 1)
