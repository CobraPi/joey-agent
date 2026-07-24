# Hooks Configuration

Joey Agent supports PreToolUse hooks — user-defined shell commands that fire
before a tool executes. This is a safety mechanism that can allow, deny,
halt, or rewrite tool calls.

## Configuration

Add hooks to `~/.joey/config.yaml`:

```yaml
hooks:
  - name: "prevent-force-push"
    event: "PreToolUse"
    matcher: "terminal"           # regex matching tool name; empty = all
    command: "check-no-force-push"

  - name: "lint-before-write"
    event: "PreToolUse"
    matcher: "write_file|patch"
    command: "my-linter --check"
```

## Hook Protocol

The hook command receives JSON on stdin:

```json
{
  "event": "PreToolUse",
  "tool_name": "terminal",
  "tool_input": { "command": "git push --force" },
  "session_id": "abc123",
  "cwd": "/home/user/project"
}
```

### Exit Codes

| Exit Code | Effect |
|-----------|--------|
| 0         | **Allow** — proceed normally |
| 2         | **Deny** — block this tool call (error result to model) |
| 49        | **Halt** — stop the entire turn |
| Other     | Treated as allow (hook error logged) |

### Input Rewriting

Exit 0 with JSON on stdout to rewrite the tool arguments:

```json
{"updated_input": {"extra_flag": true}}
```

The patch is shallow-merged into the tool arguments.

### Deny with Reason

Exit 2 with JSON on stdout to provide a reason:

```json
{"reason": "Force push is not allowed in this project"}
```

Or simply print a message to stderr/stdout — the first non-empty line is used.

## Matcher Syntax

The `matcher` field is a regex matched against the tool name. Examples:

- `terminal` — matches the terminal tool only
- `write_file|patch` — matches write_file OR patch
- `^web_` — matches any tool starting with "web_"
- empty/missing — matches ALL tools
