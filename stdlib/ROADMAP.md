# Stdlib Roadmap

This file is a backlog plus historical ledger. Completed items document why a
decision was made and where the canonical behavior now lives; open items record
the blocker, trigger, and rough effort.

Effort labels:

- `S`: hours.
- `M`: about a day.
- `L`: several days.
- `XL`: needs an owner-level design decision.

For current behavior, prefer the README in the relevant subtree. This roadmap is
not the primary specification.

## Current Snapshot

- Tiers 0 through 5 are active.
- The metaprogramming layer is active.
- LSP/DAP command surfaces are wired through `caap.session.commands`.
- Native codegen, sys facades, generic call-site typing, optional pass
  registration, facts, and C-like surface modules are all in the active tree.
- The remaining large design tails are full HM-style generic inference,
  additional kernel substrate where explicitly blocked, and external/editor
  integration polish.

## A. Language And Types

### A1. Partial Evaluation - DONE

Goal: treat compile-time, runtime, and dual-phase execution as one partial
evaluation mechanism.

Implemented pieces:

- `const` folds proven-pure functions through effect inference and purity gates.
- `semantics/passes/peval.caap` performs whole-module const propagation,
  constfold, simplify, and DCE to a fixed point.
- `semantics/passes/pe.caap` adds binding-time annotations and call-site
  specialization.
- `static!(name, indices)` and the source form `(static_params name idx...)`
  mark static parameters; calls with static literal arguments are specialized
  by inline plus `peval`.
- Copy propagation was added to `peval`.

Open tail: keep PE optional because fixed-point work costs load time. Compose
`stdlib/boot/peval.caap` after bootstrap when the session wants the pass.

### A2. Type Variables In Generics - DONE

Call-site parametric polymorphism is implemented for signatures such as:

```lisp
(defn map ((xs (list T)) (f callable)) (list T) ...)
```

Implemented pieces:

- Registry helpers: `type_var?`, `ctor_of`, `type_args`, `subst_type`.
- Inference helpers: `unify`, `bind_var!`, `check_call_sig`,
  `finalize_result`.
- `T` binds to argument element type at the call site and is substituted into
  the result type.
- Non-generic signatures short-circuit through the previous path.
- Collection helpers have real generic signatures where the result can be
  expressed at call-site precision.

Open tail: full HM inference, variance, body-local inference for generic
functions, and generic typing of bare kernel facade heads.

### A2.1. Native Generics Through Monomorphization - DONE

Native generics are explicit and monomorphized before codegen.

Implemented pieces:

- `semantics/passes/monomorph.caap` specializes generic `defn` templates with
  compile-time `type` or `field` parameters.
- `preresolve` can infer omitted compile-time arguments from declared receiver
  types.
- The pass runs in `backend/prep`, not on the eval path.
- `examples/urun/ur_list.caap` uses generic intrusive lists for zero-allocation
  RTOS structures.

Open tail: recursive or mutually-recursive generic templates and stricter scoped
field substitution.

### A3. Adoptable-Width `ref` - KERNEL-BLOCKED

Problem: `(bind x (ref <literal>))` has no type slot, and the lowered artifact is
shared by eval and native paths. Inserting native-only casts into the initializer
is not valid for eval, while retargeting storage after emission requires
rewriting already-emitted stores.

Temporary workaround: use `(ref (cast 0 u64))` in native-only code. C-like
parameter widths already flow through signatures.

Needed substrate: a typed-ref channel or source spec annotation that both eval
and native paths can interpret.

### A4. Nested Aggregates In Native - DONE

The emitter was already general enough for nested struct fields and struct
elements. Tests pin `%Seg = type { %Point, %Point, i32 }`, insert/extract chains,
and stack arrays of structs. Eval/native parity is pinned.

### A5. C-Like Surface Phase 2 - DONE

Implemented:

- Type declarations such as `Round type = { secret i32, attempts i32 }`.
- Struct literals with named fields and declaration-order lowering.
- Dot read and assignment.
- String interpolation.
- `break [loop]` lowering through generated flags.
- Surface modules with `(surface <kit> <name>)`.

### A6. Else-Less `if` In Value Position - DONE

C-like lowering rejects an else-less `if` in a value tail position because it
would implicitly return `0` on the false branch. Statement-position else-less
`if` remains valid and lowers to a `null`/`0` fallback as appropriate.

### A7. Borrow Checking - DONE

`semantics/passes/borrow.caap` implements a flow pass over the AST with inert
runtime primitives:

- `(move x)`
- `(borrow x (lambda (r) ...))`
- `(borrow_mut x (lambda (r) ...))`

It enforces affine moves and the aliasing rule: one mutable borrow or many read
borrows. The companion `escape` and `alias` analyses cover borrowed-handle
escape and alias-through-bind cases.

Open research tail: lifetime inference, non-lexical lifetimes, lifetime
parameters, and variance.

### A8. Passes And Analyses - DONE

The optional pass framework is active in `semantics/passes.registry`.

Analyses:

- `lint`: unused bindings/parameters, shadowing, unreachable code, constant
  conditions.
- `callgraph`: dependency graph, dead definitions, recursion facts.
- `escape`: borrowed-handle escape through returns, closures, and alias binds.
- `naming`: boolean-computing functions without a `?` suffix.
- `match_check`: unreachable arms and enum exhaustiveness.
- `borrow`, `alias`: ownership and alias analyses.

Transforms:

- `constfold`: bottom-up pure literal folding.
- `dce`: dead local binding removal when the value is total and side-effect free.
- `simplify`: dead branches and capture-safe beta rewriting.
- `peval`: fixed-point partial evaluation core.
- `pe`: binding-time specialization.
- `preresolve`, `monomorph`, `struct_monomorph`: native generic preparation.

Project linters over raw forms:

- `imports`: unused `use`/`import` symbols.
- `tiers`: stdlib tower invariant.

## B. Loader And Infrastructure

### B1. Loader Cache - INSTRUMENTED

`load_log` records per-module elapsed time when a time service is available.
The data showed that eager codegen-layer loading was the largest avoidable
cost; that layer is now lazy. A bootstrap image or type-harvest cache remains a
future optimization, gated by real startup data from larger projects.

### B2. Pass Unregister - DONE

`unregister_pass!` and `unregister_transform!` remove one entry by name. Explicit
priority ordering remains unnecessary until a real conflict appears; dependency
metadata exists for sessions that need it.

### B3. Cross-Module Analysis Roots - DONE

`analyze_source_with_root` declares roots by convention and returns import
definition data with source paths and spans.

### B4. Structured Diagnostics For DAP - DONE

`run_source_checked` returns either `{ok:true, value}` or
`{ok:false, diagnostics:[...]}`. It does not throw across the DAP command
boundary.

### B5. Spans In Lowered Surface Specs - DONE

C-like lowering stamps lowered specs with token spans. Loader diagnostics for
surface files are fully located.

### B6. Named Surface Modules - DONE

`(surface <kit> <name>)` marks a surface file as a module. Discovery indexes the
file by the second header argument, and load does not auto-run `main`.

## C. Kernel Requests - WIRED

### C1. Tail-Call Optimization - DONE

Self-recursive tail calls run at constant depth. Mutual recursion is not part of
this contract.

### C2. `string_chars` - DONE

`lib/text/string.chars` wraps the kernel primitive. JSON parsing moved to a
pre-split character cursor.

### C3. `ctfe_spec_with_span` - DONE

Lowered surface specs and IR rewrites preserve source spans when rebuilding
nodes. This keeps post-transform diagnostics precise.

### C4. `value_eq` - DONE

`lib/equal.deep_eq` wraps structural equality. `syntax.ir.node_eq` keeps its own
structural node comparison because specs and values have different identity
rules.

### C5. `ctfe_debug_frames` - AVAILABLE

This is diagnostic-class and impure. Consumers can use it for REPL/tracing work
when needed.

### C6. Vocabulary Cache - DONE

Vocabulary is cached once per session and invalidated through builtin
registration.

### C7. Real Paths - DONE

Surface parsing and C-like diagnostics carry real file paths.

### Available But Deliberately Unused

- Multicapture in the PEG engine. C-like lowering stays token-stream based for
  better diagnostics and complete lowering control.

## D. v1 Ports Rebuilt As Libraries

Completed ports:

- `lib/text/json`: parse/stringify/pretty, data-shaped parse errors, full
  `\uXXXX` decode including surrogate pairs, and control-character encoding.
- `lib/diag/error`: structured error values and normalization helpers.
- `lib/collections/set`, `lib/core/functional`, `lib/core/prelude`.
- `derive` registry with memoized generation.
- Diagnostic-code registry.
- Module graphs, load events, and DOT rendering.

These ports are intentionally cleaner than v1: they use `try`, typed helpers,
load-time signatures, and library-level diagnostics.

## E. Outside Stdlib

Completed or available in the repository:

- VS Code launch configuration points at `stdlib/bootstrap.caap`.
- Kernel reference includes newer primitives such as `value_eq`,
  `string_chars`, `ctfe_spec_with_span`, `ctfe_kernel_vocabulary`,
  `ctfe_debug_frames`, and `ctfe_grammar_parse_forms`.
- LSP consumes `caap.session.commands`.
- Startup optimization directions are measured: daemon mode, image cache, and
  lazy bootstrap layers.

## Deliberately Rejected

- Re-importing the v1 architecture.
- Runtime variable-name introspection as a language feature.
- First-class phase values.
- Value provenance that breaks alpha equivalence or native parity.
- Sized-int wrap semantics in eval; eval remains the reference semantics, while
  native code wraps according to native type width.
- Pass priorities or fixed points without a real consumer.
