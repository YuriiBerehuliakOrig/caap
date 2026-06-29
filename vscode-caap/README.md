# CAAP for VS Code

Syntax highlighting and language intelligence for CAAP, powered by the
`caap-lsp` language server.

## What you get

- **Bootstrap TextMate highlighting** — comments, strings, numbers,
  booleans/null, special forms (`lambda`, `bind`, `if`, `do`, …) and defining
  forms (`define-class`, `register-module`, …) work the moment a `.caap` file
  is opened.
- **Semantic highlighting** — once `caap-lsp` parses the file with the real
  CAAP surface grammar, every leaf is reclassified (functions vs variables vs
  parameters vs types vs namespaces vs operators, plus the clike control
  keywords and native type tokens in surface bodies), including identifiers
  introduced by user-defined grammar extensions.
- **Diagnostics** — parse errors from `caap-core` are surfaced inline.
- **Outline** — `bind` (single and multi-binding), `define-class`,
  `define-interface`, `defmacro`, `register-module`, and similar forms are
  surfaced in the Outline view and breadcrumbs.
- **Hover** — shows the inferred shape of the binding at the cursor.
- **Go to definition** — jumps to the binding name for symbols defined in the
  current file.

The grammar-extension story is the key reason this extension uses an LSP
instead of a richer static TextMate grammar: when CAAP user code adds new
syntax via `extend-grammar`/authoring DSL, the LSP server re-parses with the
extended grammar that is actually in scope, and the semantic-tokens stream
reflects the user's syntax — no static rules to maintain.

## Building from source

```sh
# Build the language server
cargo build -p caap-lsp

# Build the VS Code extension
cd vscode-caap
npm install
npm run compile
```

Then press **F5** in VS Code with this folder open to launch an Extension
Development Host.

## Settings

| Setting | Default | Description |
|---------|---------|-------------|
| `caap.server.path` | `""` | Absolute path to the `caap-lsp` binary. When empty, the extension looks for `target/debug/caap-lsp`, `target/release/caap-lsp`, and finally `caap-lsp` on `$PATH`. |
| `caap.trace.server` | `off` | LSP wire-trace level (`off`, `messages`, `verbose`). |

## Limitations

This is an MVP. It does **not yet** provide:

- Cross-file go-to-definition (only same-file bindings).
- Completion.
- Incremental parsing / cross-file dependency invalidation when one CAAP file
  extends another's grammar.
- Refactorings.

These are tractable follow-ups on the same architecture.
