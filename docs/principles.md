# CAAP Principles

This document records the fundamental CAAP principles: decisions that shape the
architecture and should not be bypassed for local convenience. New features and
refactors should be checked against these principles before they are accepted.

## 1. Minimal Semantic Kernel

The compiler core contains the smallest practical set of semantic primitives.

The IR has exactly three node kinds: `Name`, `Literal`, and `Call`. There is no
`IfNode`, `LambdaNode`, `LoopNode`, or `MatchNode`. Those constructs are calls
whose callee policies define their behavior.

Why: every new core primitive ties the kernel to one language shape. CAAP is a
platform for building languages, not a single language frozen into Rust.

Violation smell: "add `MatchNode` to the IR" or "make async a core node." The
preferred answer is a stdlib pass, builtin, or grammar extension.

## 2. Callee-Defined Semantics

Operation semantics are defined by the callee, not by the node shape.

`(if cond then else)` is a `Call` whose callee is the name `if`. Laziness,
branching, lexical binding, special evaluation, and phase access are callee
metadata and dispatch behavior.

Why: the IR remains stable. New constructs require new semantic registrations,
not new graph shapes.

## 3. Policy-Driven Behavior

Each callable carries explicit semantic policy.

Important policy dimensions:

- `EvalPolicy`: eager, lazy, sequential, or special-form evaluation.
- `ControlPolicy`: conditional branch, structured exit, or ordinary call.
- `ScopePolicy`: whether the call introduces lexical binding.
- `PhasePolicy`: runtime, compile-time, or dual access.
- `EffectPolicy`: tags such as `mutation`, `io`, `write_ir`, or
  `request_restart`.
- `FoldPolicy`: whether compile-time folding is valid relative to runtime
  behavior.

Why: passes should consume policy instead of hard-coded builtin name lists.

## 4. Libraries Over Language Features

Most language features belong in libraries.

Types, generics, pattern matching, surface grammars, optimization passes,
native codegen, and module semantics are stdlib policy. The kernel supplies
substrate and controlled bridges.

Current examples:

- `stdlib.semantics.types.*` owns type/effect checking.
- `stdlib.semantics.passes.*` owns optional analyses and transforms.
- `stdlib.frontend.*` owns opt-in surface languages.
- `stdlib.backend.*` owns native/WASM codegen policy.

Why: libraries can evolve, be replaced, or be ignored. Kernel features cannot.

## 5. Explicit Metaprogramming

Runtime macros cover local lazy syntax transforms; CTFE covers compiler-phase
program transformation.

`macro` is a runtime value constructor. Macro arguments are quoted as syntax
values, the macro returns syntax, and the evaluator expands that syntax in the
caller environment.

CTFE executes ordinary CAAP code at compile time with explicit access to IR,
semantic registries, diagnostics, facts, and provider contexts through `ctfe-*`
APIs.

Why: user-defined lazy forms should not require Rust-only `SpecialForm`
privilege, while compiler rewrites still need explicit phase, effect, and IR
capability boundaries.

## 6. Explicit Bootstrap

A bare compiler session knows nothing until bootstrap code registers behavior.

`CompilerHost::new_session()` starts empty: no stdlib modules, no language
surface, no module system, no sys APIs, and no hidden autoloading.

`stdlib/bootstrap.caap` builds the session by explicitly loading the expander,
forms, loader, type/effect layer, and command surface.

Why: a platform can host multiple policy layers only if startup state is
explicit. Tests also become deterministic because they start from a known empty
session.

## 7. Module System Is A Library

`import`, `use`, `re_export`, `export`, roots, discovery, and project semantics
belong to the stdlib loader, not the kernel.

The kernel provides unit registration, compiler registry access, source loading
substrate, query APIs, and cross-unit link mechanisms. It does not decide what a
module is.

Why: module systems are language-specific. CAAP keeps that policy replaceable.

## 8. System APIs Are Explicit Modules

File system, I/O, network, OS, process, path, random, and time APIs are typed
modules behind capability policy. They are not ambient builtins.

There is no global `read_file` builtin. Correct access goes through sys facades
such as `stdlib.sys.fs` or trusted bootstrap/tooling exports with explicit
capabilities.

Why: ambient system APIs make compile-time/runtime isolation and effect
tracking unenforceable.

## 9. Deterministic Compilation

The same input plus the same bootstrap must produce the same result.

This affects data structure choices, traversal order, fingerprints, stable IDs,
artifact cache keys, and diagnostics.

Why: incremental compilation, distributed caching, reproducible tests, and
meaningful traces all require determinism.

## 10. Stable Identity

IR nodes, surface forms, semantic entries, and artifacts need stable identity
across compilation phases and incremental runs.

Numeric allocation order is not enough. Stable identity must be derived from
source position, unit identity, and semantics where appropriate.

Why: passes must be able to recognize "the same thing" after rewrites and cache
replay.

## 11. No Silent Compatibility Fallbacks

Contract violations must become diagnostics or errors, not magical alternate
behavior.

A fallback is allowed only when it is part of the documented public contract and
has tests. Parser recovery can return partial trees because that is parser UX;
a compiler query API should not silently reinterpret malformed arguments.

Why: hidden correction masks bugs, breaks invariants, and makes artifacts hard
to explain.

## 12. Diagnostics And Trace Are Architecture

Diagnostics, notes, warnings, cache hits, phase transitions, provider events,
and bootstrap events are observable compiler behavior.

Significant phases should expose:

- structured diagnostics with code, severity, span, and message;
- trace events with kind, source/target metadata, cache status, and elapsed
  time where useful;
- tests when the event or diagnostic is public CLI/API behavior.

Why: a query pipeline with CTFE and user passes is not maintainable without
structured visibility.

## 13. Specification Over Local Convenience

An implementation should follow the architecture contract, not convenient local
shortcuts.

Stable public concepts include `Unit`, compiler host/session, provider,
artifact, phase, effect, bootstrap order, and module ownership. Missing features
must be explicitly missing; incomplete shims are not acceptable substitutes.

Why: CAAP is a platform contract, not just the current Rust layout.

## 14. Core Provides Substrate, Stdlib Owns Policy

The core may expose bridges and adapters so CAAP stdlib code can control the
compiler. Those bridges must not become a second implementation of stdlib
policy.

Correct split:

- core: register units, run queries, expose provider contexts, load source
  templates, enforce effect/capability substrate;
- stdlib loader/project: define modules, imports, roots, dependencies, and
  exports;
- stdlib backend: define native lowering and linking policy;
- stdlib types/passes: classify types, effects, purity, facts, and rewrites;
- CLI: launch bootstrap, render diagnostics, and call bootstrapped commands.

Why: duplicated policy creates drift between Rust and CAAP.

## Quick Checklist

| Question | Principle |
| --- | --- |
| Am I adding a new IR node? | 1, 2 |
| Am I baking language semantics into the kernel? | 1, 4 |
| Am I autoloading behavior at startup? | 6 |
| Am I adding an ambient system API? | 8 |
| Am I inventing a new metaprogramming path? | 5 |
| Am I moving import/export semantics into core? | 7 |
| Can output depend on unstable map/set order? | 9 |
| Am I hiding bad API usage behind fallback behavior? | 11 |
| Is visible compiler behavior covered by diagnostics or trace? | 12 |
| Does this diverge from the architecture contract? | 13 |
| Is a bridge starting to make policy decisions? | 14 |
