# CAAP Standard Library

`stdlib/` is the active language-policy layer for CAAP. The Rust kernel provides
the evaluator, IR, CTFE primitives, host-service boundaries, and builtin
vocabulary; the standard library turns that substrate into a usable language
tower: forms, modules, type and effect checking, passes, sys facades, surface
grammars, and native code generation.

The key design rule is:

> A form is a compile-time function `(ctx node) -> node`, not a textual
> substitution.

The expander executes the form at compile time, gives it a capability-aware
context, and expects an IR tree in return. That makes forms powerful enough to
evaluate proven-pure subexpressions, inspect imports, preserve spans, and feed
the same lowered tree to both eval and compile paths.

Deferred work and historical milestones live in [`ROADMAP.md`](ROADMAP.md).
The current restructuring policy for readable, facade-based growth is in
[`../docs/stdlib-structure-plan.md`](../docs/stdlib-structure-plan.md).

## Why This Stdlib Exists

The old v1 library had an ergonomic layer registered as compile-pipeline
normalizers. Module loading, however, happened through raw compile-time eval and
did not run the normalization stage, so source modules had to be written close
to the bare substrate.

The current stdlib fixes that split. Forms are ordinary compile-time functions
registered by `boot/forms.caap` and invoked by `boot/expander.caap` on the load
path. The same expanded AST is then checked, typechecked, optionally transformed,
evaluated, or prepared for native code generation.

| Mechanism | Eval/load path | Compile path | Can compute/check? |
| --- | --- | --- | --- |
| Kernel `macro` value | yes | no | no, syntax in/syntax out |
| v1 normalizer | no | yes | yes |
| Current form function | yes | yes | yes |

Example: `(const (int_mul (int_add 1 2) 4))` becomes the literal `12` at load
time, but only when the loader can prove the expression pure through the
effect/signature layer.

## Tower Tiers

| Tier | Area | Responsibility | Status |
| --- | --- | --- | --- |
| 0 | `boot/` | Expander engine, checker, loader skeleton, per-form located gates. | Active |
| 1 | `boot/forms.caap` | Core forms: `const`, `cond`, `when`, `unless`, pipelines, `for`, `defn`, `struct`, `alias`, `enum`, `union`. | Active |
| 2 | `lib/`, `syntax/` | Collections, text, core helpers, diagnostics, projects, tests, AST readers/builders/rewriters/rendering. | Active |
| 3 | `boot/loader.caap`, `boot/commands.caap` | Module registry, `import`/`use`/`re_export`/`export`, roots, discovery, projects, LSP/DAP command surface. | Active |
| 4 | `semantics/types`, `semantics/passes`, `storage/` | Type/effect pass, generics, facts, load-time analyses/transforms, declarative binary-format compiler. | Active |
| 5 | `frontend/`, `backend/`, `bare/` | Opt-in surface grammars, C-like lowering, native prep, LLVM/WASM emit, freestanding/bare-metal wrappers. | Active |

The compile-heavy layers are lazy. A normal bootstrap does not eagerly load
LLVM/WASM emission or the surface codegen driver; `boot/native_emit.caap`, the
`tools/s2_*` drivers, or an explicit `load_module` brings them in.

## Directory Map

```text
stdlib/
  bootstrap.caap       bootstrap order and foundation signature backfill
  boot/                expander, forms, checker, loader, commands, run/analyze helpers
  lib/                 reusable tier-2 libraries
    collections/       sequence, map, set, option, result, graph, sorted structures
    text/              string, char, path, JSON, CSV, TOML, URL, buffers, encodings
    core/              prelude, functional helpers, equality, math, bits, floats
    diag/              structured errors, diagnostic registry, logs, bags
    tests/             in-language `test_*.caap` corpus
  syntax/              AST readers/builders, IR rewrites, renderer
  semantics/           type/effect system, pass registry, analyses, transforms
  sys/                 typed capability-gated facades over caap-sys services
  frontend/            opt-in surface grammars and C-like surface lowering
  backend/             native prep, LLVM/WASM emitters, build/link driver
  storage/             binary-format compiler as a library
  bare/                native-only bare-metal wrappers for MMIO, CPU, atomics, critical sections
```

## Domain READMEs

| Document | Scope |
| --- | --- |
| [`lib/collections/README.md`](lib/collections/README.md) | Sequence/map/set/option/result/graph/sorted helpers and value shapes. |
| [`lib/core/README.md`](lib/core/README.md) | Small dependency-light core helpers and prelude policy. |
| [`lib/diag/README.md`](lib/diag/README.md) | Shared diagnostic values, bags, and conversions. |
| [`lib/crypto/README.md`](lib/crypto/README.md) | Pure deterministic crypto helpers. |
| [`lib/numeric/README.md`](lib/numeric/README.md) | Larger numeric domains such as bignum and decimal. |
| [`lib/text/README.md`](lib/text/README.md) | Strings, chars, lexical paths, JSON, escape handling. |
| [`syntax/README.md`](syntax/README.md) | AST as data: read, build, rewrite, render. |
| [`semantics/types/README.md`](semantics/types/README.md) | Type descriptors, markers, effects, inference, generics. |
| [`semantics/passes/README.md`](semantics/passes/README.md) | Load-time pass/transform framework and fact store. |
| [`boot/README.md`](boot/README.md) | Bootstrap files, loader bring-up, and opt-in boot profiles. |
| [`frontend/README.md`](frontend/README.md) | Surface/front-end facade modules and internal leaves. |
| [`backend/README.md`](backend/README.md) | Native prep and LLVM/WASM codegen pipeline. |
| [`sys/README.md`](sys/README.md) | Typed sys facades, grants, and catalog verification. |
| [`storage/README.md`](storage/README.md) | Declarative binary-format compiler with eval and native backends. |
| [`bare/README.md`](bare/README.md) | Native-only bare-metal primitives and wrappers. |

## Bootstrap Flow

The bootstrap builds the compiler session from the bottom up:

1. Load the expander seed and register `define_form`.
2. Load core forms and semantic checker support.
3. Load the module loader, namespace, reader, resolver, and command surface.
4. Load foundational library modules.
5. Load the type/effect layer and backfill signatures for modules that were
   needed before inference existed.
6. Leave codegen, surface frontends, sys grants, and native emitters lazy unless
   a tool or module asks for them.

Raw bootstrap scripts that execute before `(module ...)` is available register
their identity explicitly. Loader-loaded boot modules carry normal module forms.
The full identity policy is in [`CONVENTIONS.md`](CONVENTIONS.md).

## Module Model

A module file is directives plus body forms in one shared scope.

```lisp
(module stdlib.lib.example)
(use stdlib.lib.collections.option some none)

(defn answer () int
  42)

(export answer)
```

Directive arguments are names, not strings. The loader reads the directive shape
from the AST rather than evaluating it. Strings remain data: paths, map keys, and
runtime API arguments.

Supported directives:

| Directive | Meaning |
| --- | --- |
| `(module name)` | Gives the module its registry identity. |
| `(import mod alias)` | Binds `alias` to the whole export map of `mod`. |
| `(use mod a b)` | Binds selected exports directly in scope and validates them. |
| `(re_export mod a b)` | Imports selected exports and republishes them. |
| `(export a b)` | Declares the public contract; everything else stays private. |

Modules resolve by explicit declaration, root declaration, or discovery:

- `declare name path`
- `declare_root prefix base`
- `discover dir`

Cyclic dependencies are supported with a two-phase export map, but cross-links
must be delayed inside function bodies. Initialization-time access to an
unfinished cyclic dependency is rejected.

## Facade Modules

Large subsystems expose stable facade modules and keep focused implementation
leaves behind them. Import public behavior from the facade unless a subtree
README explicitly marks a leaf as stable.

Examples:

- `stdlib.frontend.clike` is the public C-like surface entrypoint.
- `stdlib.semantics.types.infer` is the loader-facing type-pass entrypoint.
- `stdlib.backend.emit.llvm.lowering` is the LLVM lowering entrypoint consumed
  by the emitter.

This keeps user-facing import paths stable while allowing implementation files
to split by domain.

## Forms And The Load Gate

The loader runs:

```text
read -> expand -> transforms -> check -> typecheck -> passes -> eval
```

All pre-eval phases run per top-level form, so every diagnostic can carry a
`path:line:col` location.

Important forms include:

| Form | Contract |
| --- | --- |
| `const` | Compile-time evaluate a proven-pure expression into a literal. |
| `cond`, `when`, `unless`, `case`, `if_let`, `when_let` | Structured control sugar expanded before checking. |
| `->`, `->>` | Threading forms. |
| `for`, `with_map` | Iteration and map destructuring helpers. |
| `defn` | Function definition with name-attached signature and optional effect declaration. |
| `struct`, `alias`, `enum`, `union` | Type markers consumed by type and native passes. |

Kernel special forms such as `if`, `bind`, `lambda`, `while`, `match`,
`try`/`catch`, `throw`, `ref`, `deref`, and `set_ref` are not duplicated in
stdlib forms.

## Type And Effect Layer

`semantics/types` owns type/effect policy. The kernel remains dynamically typed;
stdlib descriptors and markers add load-time checking.

Key properties:

- Types are string names with descriptors: primitives, sized ints/floats,
  aliases, structs, enums, unions, containers, pointers, and type variables.
- `defn` signatures are attached to names by inert markers.
- Calls are checked inside a module and across imported names.
- Sized literals are range checked.
- Struct field access is checked when the receiver type is known.
- Branches join conservatively.
- Function effects are inferred as tag sets, not a pure/impure boolean.
- Mutation of fresh local state is not treated as an external mutation effect.
- Declared effects are verified overrides.
- Generic signatures are instantiated at call sites.

The pass framework in `semantics/passes` adds optional analyses and whole-module
transforms. It includes the shared fact store used by callgraph, match checking,
type inference, optimization, and native preparation.

## Sys Facades And Capabilities

`sys/` contains typed facades over host-service libraries such as `io`, `fs`,
`os`, `time`, `net`, `process`, `rand`, and `path`.

The facades are declaration-only until the embedder grants a capability handle.
`verify_sys` compares the facade declarations against the live host-service
catalog, so missing operations, arity drift, type drift, return-type drift, and
effect drift are caught at startup.

Both canonical module names (`stdlib.sys.fs`) and compatibility aliases
(`sys.fs`) resolve to the same facade. Capability names such as `sys.fs.read`
are a different namespace: they describe host access policy, not module identity.

## Native And Freestanding Codegen

Native codegen lives in `backend/` and is loaded lazily. The current pipeline is:

```text
surface/lowered source -> declare -> prep -> emit -> link
```

Highlights:

- `native_meta.caap` is the shared vocabulary for native heads and types.
- `prep.caap` gates code before LLVM/WASM emit and can enable an opt-in strict
  native profile.
- LLVM emission supports typed `defn` ABI, control flow, refs, structs, strings,
  bits, MMIO, externs, globals, typed pointers, arrays, casts, function pointers,
  inline asm, and freestanding object emission.
- WASM emission is a sibling backend with an explicit unsupported-head set.
- `driver.caap` provides helpers such as `compile_ir`, `compile_file`,
  `link_ir!`, and freestanding surface compilation.

For CLI usage, prefer:

```bash
cargo run -p caap-cli -- stdlib/bootstrap.caap tools/s2_emit.caap FILE > out.ll
cargo run -p caap-cli -- stdlib/bootstrap.caap tools/s2_build.caap FILE OUTPUT
cargo run -p caap-cli -- stdlib/bootstrap.caap tools/s2_wasm.caap FILE > out.wat
```

## Tests

In-language tests are `test_*.caap` files that import
`stdlib.lib.test` and run assertions at load time. The Rust harness scans the
test tree recursively; adding a new in-language test normally requires no Rust
registration.

The most relevant harnesses are under [`../caap/tests/`](../caap/tests/):

- `stdlib_loader_tests.rs`
- `stdlib_types_tests.rs`
- `stdlib_passes_tests.rs`
- `stdlib_codegen_tests.rs`
- `stdlib_sys_tests.rs`
- `stdlib_governance_tests.rs`

Negative `.caap` fixtures live in [`../tests/`](../tests/) and are not examples.

## Authoring Rules

Use [`CONVENTIONS.md`](CONVENTIONS.md) for the detailed rules. In short:

- Respect the tier invariant.
- Prefer stdlib policy over kernel additions.
- Keep modules explicit about imports and exports.
- Preserve spans through rewrites where possible.
- Use typed errors and diagnostics instead of fallback behavior.
- Write tests at the layer where behavior is visible.
