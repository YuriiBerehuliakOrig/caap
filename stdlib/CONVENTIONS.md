# Stdlib Conventions

This document explains how to write and maintain modules in the second edition
of the CAAP standard library. The broader architecture is described in
[`README.md`](README.md); this file is the working rulebook.

## Rule Zero: The Tier Invariant

The stdlib tower is built bottom-up. Each tier may depend only on the language
and mechanisms provided by lower or equal tiers. Bare-substrate code is confined
to the seed files that make the expander possible.

Authoring rules:

- Do not import upward. Tier-2 library code must not depend on tier-4 type
  semantics or tier-5 backends.
- Prefer forms over bare substrate once the expander is available.
- Keep a single definition of each form in `boot/forms.caap`.
- Use bootstrap backfill for cross-tier signature information; do not create
  import cycles to get it.

## Module Shape

A module is directives plus body forms. All body forms run in one shared scope.

```lisp
(module stdlib.lib.foo)
(import stdlib.lib.bar bar)

(bind helper (lambda (x) ...))
(bind public_fn (lambda (x)
  (helper ((get bar "f" null) x))))

(export public_fn)
```

Directive arguments are names, not strings. The loader reads directive shape
from the AST and does not evaluate those arguments. Use strings for data: paths,
map keys, diagnostic codes, and runtime API arguments.

If a module has no `(export ...)`, the final form is the exported value. That is
useful for single-value modules, but most library modules should export names
explicitly.

## Facade Modules And Internal Leaves

Large subsystems should keep a stable facade module for public imports and move
focused implementation details into leaves under the same namespace.

Rules:

- Do not change a public facade's module name or exports during a readability
  split.
- Treat new leaves as internal unless their README explicitly documents them as
  stable API.
- Prefer service maps for callbacks from a leaf into the facade. Avoid import
  cycles.
- Split pure helpers first, then stateful dispatch.

Examples of public facades are `stdlib.frontend.clike`,
`stdlib.semantics.types.infer`, and `stdlib.backend.emit.llvm.lowering`.

## Directives

| Directive | Contract |
| --- | --- |
| `(module name)` | Sets the module identity used by the loader registry. |
| `(import mod alias)` | Binds `alias` to the full export map of `mod`. |
| `(use mod a b)` | Binds selected exports directly in scope and validates them. |
| `(re_export mod a b)` | Imports selected exports and republishes them from this module. |
| `(export a b)` | Declares the public contract; all other names are private. |

Malformed directives are load-time errors with usage diagnostics. A string
literal where a module/name token is expected is an error.

## Name Resolution

The loader resolves modules in three ways:

| Mechanism | Use |
| --- | --- |
| `declare name path` | Register one exact name/path pair. |
| `declare_root prefix base` | Resolve `prefix.a.b` as `base/a/b.caap`. |
| `discover dir` | Recursively scan `.caap` files, read `(module ...)`, and index them. |

Duplicate module names are rejected. Re-loading the same canonical file is
idempotent.

Cyclic dependencies are supported by a two-phase export map. Cross references
must be delayed inside function bodies; top-level access to a still-empty cyclic
dependency is rejected.

## Forms Available Without Import

Stdlib forms are expanded during load and do not remain in the resulting IR.

| Form | Expansion contract |
| --- | --- |
| `(cond (t b...) ... (else b...))` | Nested `if`; validates `else` placement. |
| `(when t b...)` / `(unless t b...)` | Conditional `do` with `null` fallback. |
| `(case x (v b...) ... (else b...))` | Temporary binding plus equality tests. |
| `(if_let (x e) then else)` / `(when_let (x e) b...)` | Bind plus null check. |
| `(with_map m ((a "k" [d]) ...) b...)` | Binds selected map keys with defaults. |
| `(-> x ...)` / `(->> x ...)` | Thread-first / thread-last pipelines. |
| `(for x items b...)` | Sequence iteration. |
| `(const expr)` | Compile-time evaluation to a literal when purity is proven. |
| `(defn f ((a t) ...) result [effect] body...)` | Function with name-attached signature and verified effect declaration. |
| `(struct Name (field type) ...)` | Type marker plus typed constructor. |
| `(alias name target)` | Type alias marker. |
| `(enum Name (variant value) ...)` | Named integer constants under an enum type. |
| `(union Name (member type) ...)` | Native-only overlapping storage marker. |

Do not duplicate kernel forms such as `if`, `bind`, `lambda`, `while`, `match`,
`and`, `or`, `try`, `catch`, `throw`, `ref`, `deref`, `set_ref`, `block`, or
`leave`.

## Naming

- Use `snake_case` for functions and local bindings.
- Use domain prefixes when a name would otherwise be ambiguous, such as
  `set_union` or `json_parse`.
- Predicates end in `?`.
- Mutators end in `!`, and should be rare.
- Module names use the `stdlib.<area>.<name>` shape.
- Diagnostic namespaces should be explicit and stable.

## Module Identity Policy

For normal stdlib modules, `(module ...)` must match the path under `stdlib/`:

```text
stdlib/sys/fs.caap          -> (module stdlib.sys.fs)
stdlib/lib/text/path.caap   -> (module stdlib.lib.text.path)
```

The governance test `stdlib_module_identity_check` enforces this policy in
`caap/tests/stdlib_governance_tests.rs`.

Intentional exceptions:

1. Raw bootstrap scripts execute before the `module` form is available. They do
   not carry `(module ...)`; they register identity through
   `ctfe_compiler_register_value` and document that with a `MODULE IDENTITY`
   banner.
2. A small set of loader-loaded boot modules keeps short public names for
   compatibility and command-surface identity: `stdlib.analyze`,
   `stdlib.commands`, `stdlib.run`, and related explicitly allowlisted boot
   surfaces.

### Sys Facades

The eight sys facades under `sys/` carry canonical module names:

```text
stdlib.sys.fs
stdlib.sys.io
stdlib.sys.net
stdlib.sys.os
stdlib.sys.path
stdlib.sys.process
stdlib.sys.rand
stdlib.sys.time
```

Compatibility aliases such as `sys.fs` remain supported through bootstrap root
declarations. Internal stdlib code should use canonical `stdlib.sys.*` names.
Capability names such as `sys.fs.read` are a separate access-policy namespace,
not module names.

## Comments And Local Documentation

CAAP comments start with `;` and continue to the end of the line.

Preferred file shape:

```lisp
; stdlib/lib/<area>/<name>.caap - one sentence describing the module.
;
; Rationale: what the module provides, what it intentionally does not provide,
; and how it relates to kernel mechanisms.
(module stdlib.lib.<area>.<name>)

; -- Section ---------------------------------------------------------------
; abs - magnitude.
;   x  : an int
;   -> : x when x >= 0, otherwise -x
(bind abs (lambda (x) ...))
```

Guidelines:

- Put the file banner at the top.
- Group related definitions with short section comments.
- Document public bindings with purpose, argument roles, and result shape.
- Put rationale next to code when it is needed to understand a local decision.
- Keep README files for domain contracts and navigation, not duplicated API
  bodies.
- New implementation files should normally stay below roughly 450 lines. Files
  above roughly 700 lines are candidates for a facade/leaf split unless they are
  generated catalogs, large test corpora, or deliberately flat tables.

## Directory Responsibilities

| Directory | Responsibility |
| --- | --- |
| `boot/` | Expander, forms, checker, loader, namespace/reader/resolve, session commands, opt-in sys grants and native emit setup. |
| `lib/collections/` | Lists, maps, sets, options, results, graphs, sorted/priority helpers. |
| `lib/text/` | Strings, chars, lexical paths, JSON, CSV, TOML, URL, buffers, encodings. |
| `lib/core/` | Prelude, functional helpers, equality, math, bits, float helpers. |
| `lib/diag/` | Diagnostic registry, structured errors, logs, bags, conversions. |
| `syntax/` | AST readers/builders, IR rewriting, rendering. |
| `semantics/types/` | Type descriptors, markers, effects, inference, generics. |
| `semantics/passes/` | Optional load-time analyses, transforms, and fact store. |
| `frontend/` | Opt-in surface grammars and C-like lowering. |
| `backend/` | Native prep, LLVM/WASM emit, codegen driver, common codegen metadata. |
| `storage/` | Declarative binary-format compiler. |
| `sys/` | Typed host-service facades and catalog verification. |
| `bare/` | Native-only bare-metal wrappers. |

The negative fixture corpus is outside `stdlib/`, under [`../tests/`](../tests/).
Examples live under [`../examples/`](../examples/).

## Projects

A project manifest is a map expression:

```lisp
(assoc (map_of)
  "name"  "demo_app"
  "roots" (list_of (assoc (map_of) "prefix" "demo_app" "base" "src"))
  "deps"  (list_of "../mathlib/project.caap")
  "entry" "demo_app.main")
```

`stdlib.lib.project` loads dependencies recursively, installs roots in the
loader, rejects dependency cycles, and runs the entry module. Project fixtures
live under [`../tests/proj/`](../tests/proj/).

## Native Build Shape

A native input is usually a value-shaped file without `(module ...)`: `defn`
definitions, optional `(use ...)` dependencies, a `main`, and a final expression.
The prep stage inlines dependencies into one translation unit before LLVM/WASM
emission.

Common drivers:

```bash
cargo run -p caap-cli -- stdlib/bootstrap.caap tools/s2_emit.caap FILE > out.ll
cargo run -p caap-cli -- stdlib/bootstrap.caap tools/s2_build.caap FILE OUTPUT
cargo run -p caap-cli -- stdlib/bootstrap.caap tools/s2_wasm.caap FILE > out.wat
```

The native executable exit code is the value of `main`.

## Load-Time Checks

Every loaded module passes through:

1. Expansion.
2. Optional whole-module transforms.
3. Semantic checking for unknown names and arity.
4. Type/effect checking against kernel vocabulary, `defn` signatures, imported
   signatures, and sys facade declarations.
5. Optional analysis passes.
6. Evaluation.

Diagnostics should be precise and located. Unknown or uncertain information
should not produce false positives; clear contract violations should fail load.

## Tests

In-language tests are `test_*.caap` files that import
`stdlib.lib.test` and execute assertions during load. The Rust harness discovers
them recursively, so adding a test file normally does not require editing Rust.

Use:

- targeted Rust tests for a specific subsystem,
- in-language tests for stdlib behavior,
- acceptance scripts for end-to-end CLI/native/freestanding paths.

Run `scripts/strict-gate.sh` before considering a substantial change complete.
