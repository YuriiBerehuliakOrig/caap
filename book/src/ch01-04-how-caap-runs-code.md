## How CAAP Runs Code

CAAP has no subcommands and no flags. The whole command-line interface is two
shapes:

```text
caap PROGRAM
caap BOOTSTRAP PROGRAM [ARG...]
```

Despite that small surface, there are **three** ways a program ends up running,
and knowing which one you're in explains almost every "unknown name" error a
newcomer hits.

### Mode 1: The Bare Kernel

```bash
caap program.caap
```

The file is read and evaluated against the kernel alone. You get special forms
and builtins — arithmetic, comparisons, lists, maps, strings, `bind`, `lambda`,
`if`, `while`, and so on — and nothing else. A non-`null` final value is
printed, and successful bare evaluation exits `0`.

This mode has no modules and no `sys` authority. If you write `(module …)`,
`(use …)`, or call `println` here, you'll get `unknown name`, because those are
not kernel features — they belong to the tower.

Use `-` as the program to read from standard input.

### Mode 2: The Bootstrap Tower

```bash
caap stdlib/bootstrap.caap program.caap
```

The first argument is a **bootstrap**: a CAAP program run *with the `sys`
capability* that brings the standard library up at compile time. The bootstrap
registers modules, the `struct`/`defn` forms, the type and effect passes, the
macro and derive machinery, the surfaces, and the native backends.

Multiple bootstraps compose: a "composed bootstrap" is just a file that executes
several bootstrap files in turn, each with `sys` authority. This replaces the
repeated `--bootstrap` flags that older CLIs used.

> **A note on running tower programs directly.** The directives `(module …)`,
> `(import …)`, and `(use …)` are interpreted by the stdlib **loader** as it
> brings a module in — they are not script statements. So a standalone file
> full of `(use …)` directives is consumed by being *loaded as a module* or *fed
> to a tool* (below), not by being evaluated line by line. In practice you run
> tower programs through the `tools/*.caap` programs.

### Mode 3: The Tools (Emit and Build)

The native and WebAssembly backends are driven by small CAAP programs in
`tools/`, run on a composed bootstrap:

| Tool | What it does |
|------|--------------|
| `tools/s2_emit.caap` | Emit LLVM IR text for a program. |
| `tools/s2_build.caap` | Compile and link a native executable (needs `clang`). |
| `tools/s2_wasm.caap` | Emit WebAssembly text (WAT). |
| `tools/ast_json.caap` | Dump a file's parsed AST as JSON. |
| `tools/canonicalize.caap` | Re-print a file in canonical form. |

A native build looks like this (Chapter 13 walks through it):

```bash
caap COMPOSED_BOOTSTRAP tools/s2_build.caap program.caap
```

where `COMPOSED_BOOTSTRAP` brings up stdlib plus the native emitter. The result
is a freestanding executable whose exit code is whatever its `main` returns.

### The Reader Reads One Form at a Time

One more thing worth knowing early: CAAP uses a **segmental reader**. It reads a
file *one top-level form at a time*, and certain *reader directives* —
`(extend_syntax …)`, `(define_grammar …)`, `(begin_scope …)`, `(end_scope)` —
take effect at read time and change how the *following* forms are read. This is
what makes syntax extensions (Chapter 12) possible without a separate
preprocessor: the grammar can change partway through a file.

### Choosing a Mode

- Learning the kernel (Chapters 3–6)? Use **Mode 1** — every example is a file
  you run with `caap file.caap`.
- Using the standard library, types, structs, or surfaces (Chapters 7–12)? You
  are on the **tower** (Mode 2/3); examples come from the tested corpus.
- Producing a binary (Chapter 13)? Use the **tools** (Mode 3).

The full contract — exit codes, `cli.main`, stdin — is in
[Appendix B](appendix-02-cli.md). Next, a complete small program to see the
language working as a whole.
