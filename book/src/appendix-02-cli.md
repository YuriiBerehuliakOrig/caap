# Appendix B: The CLI Contract

The `caap` tool has no subcommands and no flags. Its entire surface is two
shapes:

```text
caap PROGRAM
caap BOOTSTRAP PROGRAM [ARG...]
```

## `caap PROGRAM` — Bare Kernel

Evaluates `PROGRAM` on the bare kernel (special forms + builtins only). Use `-`
as the program to read source from standard input.

The final value is reported as follows in bare mode:

| Result | Behaviour |
|---|---|
| integer | printed to stdout, exits `0` |
| `null` | prints nothing, exits `0` |
| any other non-null value | printed to stdout |

There is no module system, no `sys` authority, and no standard library in this
mode. `(module …)`, `(use …)`, and `println` produce `unknown name` here — they
belong to the tower.

## `caap BOOTSTRAP PROGRAM [ARG...]` — Tower

Executes `BOOTSTRAP` *with the `sys` capability*, then:

- if the bootstrap registered a `cli.main`, calls it as `(cli.main program
  args)`;
- otherwise evaluates `PROGRAM` as a bootstrap-style script.

In launcher mode, `null` exits `0`, an integer result becomes the process exit
code, and any other non-null value is printed to stdout. This matches the native
binary contract for programs whose `main` returns an integer.

**Composed bootstraps.** Running several bootstraps is just a file that executes
each with `sys` authority, e.g.:

```scheme
(do
  (ctfe_compiler_execute_bootstrap_file compiler "…/stdlib/bootstrap.caap"     (list_of "sys"))
  (ctfe_compiler_execute_bootstrap_file compiler "…/stdlib/boot/native_emit.caap" (list_of "sys")))
```

This replaces the repeated `--bootstrap` flags older CLIs used.

## The Tools

The native and inspection workflows are CAAP programs in `tools/`, run on a
(composed) bootstrap. The program *being processed* is passed as an argument and
is resolved relative to the tool, so pass an **absolute path** to be safe.

| Tool | Purpose |
|---|---|
| `tools/s2_emit.caap` | emit LLVM IR text |
| `tools/s2_build.caap` | build a native executable (needs `clang`) |
| `tools/s2_wasm.caap` | emit WebAssembly text (WAT) |
| `tools/ast_json.caap` | dump a file's parsed AST as JSON |
| `tools/canonicalize.caap` | reprint a file canonically |
| `tools/bare.caap` | an empty-policy bootstrap (fast; no stdlib) |

Example — emit IR for a program:

```bash
caap COMPOSED_BOOTSTRAP tools/s2_emit.caap /abs/path/program.caap
```

## Diagnostics and Exit Codes

Diagnostics are written to standard error with stable codes
(`error[CAAP-RUNTIME-001]: …`). A program that raises an uncaught error exits
non-zero (commonly `70` for an evaluation error, `2` for a host/IO error such as
a missing file). In launcher mode, a successful integer result becomes the exit
code. In bare mode, integer results are printed and exit `0`.
