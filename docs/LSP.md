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
  ruby:
    command: "solargraph"
    args: ["stdio"]
    file_types: ["rb"]
  php:
    command: "intelephense"
    args: ["--stdio"]
    file_types: ["php"]
  csharp:
    command: "OmniSharp"
    args: ["-lsp"]
    file_types: ["cs"]
  c:
    command: "clangd"
    file_types: ["c", "h"]
  cpp:
    command: "clangd"
    file_types: ["cpp", "cc", "cxx", "hpp"]
  haskell:
    command: "haskell-language-server"
    args: ["--lsp"]
    file_types: ["hs"]
  bash:
    command: "bash-language-server"
    args: ["start"]
    file_types: ["sh", "bash"]
```

Any language with a language-server implementation works — the entries above
correspond to the languages joey parses with dedicated tree-sitter grammars
(see `crates/joey-neurocode/src/parse/registry.rs`).

## Available Tools

When LSP servers are configured and running, these tools become available:

- **lsp_diagnostics** — Get errors and warnings for a file
- **lsp_definition** — Go to the definition of a symbol
- **lsp_references** — Find all references to a symbol
- **lsp_symbols** — List document symbols (functions, classes, types)

All tools are conditionally registered — if no LSP server matches the file
type, the tools are hidden from the model's tool list.
