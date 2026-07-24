# LSP Configuration

Joey Agent supports Language Server Protocol (LSP) integration for code
intelligence (diagnostics, go-to-definition, references, document symbols).

## Setup

Add LSP server configurations to `~/.joey/config.yaml`:

```yaml
lsp:
  rust:
    command: "rust-analyzer"
    file_types: ["rs"]
  python:
    command: "pylsp"
    file_types: ["py"]
  typescript:
    command: "typescript-language-server"
    args: ["--stdio"]
    file_types: ["ts", "tsx", "js", "jsx"]
  go:
    command: "gopls"
    file_types: ["go"]
```

## Available Tools

When LSP servers are configured and running, these tools become available:

- **lsp_diagnostics** — Get errors and warnings for a file
- **lsp_definition** — Go to the definition of a symbol
- **lsp_references** — Find all references to a symbol
- **lsp_symbols** — List document symbols (functions, classes, types)

All tools are conditionally registered — if no LSP server matches the file
type, the tools are hidden from the model's tool list.
