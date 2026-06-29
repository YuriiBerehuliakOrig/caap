# Appendix C: Glossary

**Atom.** A single indivisible token: an integer, float, string, boolean, `null`,
or symbol.

**Form.** The unit of CAAP source — either an atom or a parenthesised list. Code
is a sequence of forms.

**List.** A parenthesised sequence of forms `(head args…)`. By default it means
*apply `head` to `args`*; some heads are special forms.

**Special form.** A list head the evaluator treats specially because it controls
evaluation order itself: `if`, `bind`, `lambda`, `do`, `while`, `block`, `leave`,
`macro`, `try`, `effect_scope`, `set!`, `and`, `or`.

**Builtin.** A primitive function provided by the kernel (e.g. `int_add`,
`get`, `string_concat_many`).

**Kernel.** The core language: the reader, special forms, and builtins. Untyped;
no modules; no ambient authority. Run with `caap file.caap`.

**Tower.** The standard library (stdlib), written in CAAP and loaded at compile
time by a bootstrap. Adds modules, typed `defn`/`struct`, type and effect
checking, macros, surfaces, and the native backends.

**Bootstrap.** A CAAP program, run with the `sys` capability, that brings the
tower up. A *composed* bootstrap runs several in sequence.

**Surface.** A concrete syntax over the language. The default is the
parenthesised notation; the C-like kit is another; you can define your own with
a PEG grammar. A surface changes spelling, not semantics.

**Segmental reader.** The reader reads one top-level form at a time, so *reader
directives* (`extend_syntax`, `define_grammar`, `begin_scope`, `end_scope`) can
change how the following forms are read.

**Homoiconic.** Code is represented as ordinary data (nested lists), so programs
can build and inspect programs. The basis of CAAP's metaprogramming.

**CTFE — Compile-Time Evaluation.** CAAP running during compilation with access
to the program's IR. How the type checker, effect checker, `const` folding,
macros, and derives are implemented.

**IR (Intermediate Representation).** The compiler's internal tree of *nodes*
that CTFE code inspects and rewrites.

**ExprSpec / syntax value.** A detached fragment of program structure — a name,
literal, or call — built and inspected with `syntax_*` (kernel) or `sym`/`lit`/
`calln` (tower), and compiled to a callable with `eval_ir`.

**Macro.** A construct (`macro`, or a tower form registered with `define_form`)
that receives unevaluated syntax and returns syntax to be expanded.

**Provider / Stage.** A *stage* is a step in the load pipeline; a *provider* is a
compile-time function registered against a stage. Lints, desugarings, and
checkers are providers.

**Annotation / Fact.** Two stores passes use to communicate: annotations are
per-IR-node key/value pairs; facts are versioned semantic values in the unit's
fact table, keyed by a schema namespace and node. Signatures and effects are
stored as facts.

**Effect (tag).** A label for what code *does* — mutate escaping state, read or
write files, use host services, emit events. Inferred and checked; declared
effects are verified.

**Capability.** Authority to perform a host effect, granted by the launcher
(`sys`). The bare kernel has none.

**Host service.** A runtime-provided side-effecting operation, grouped under
`sys.*` (io, fs, os, process, net, time).

**Budget.** A hard, *fatal* resource limit that pierces `try`: an evaluation-depth
budget (recursion/iteration) and an allocation budget (default 64 MiB for
sandboxed code). Keeps untrusted compile-time code from hanging or OOM-ing the
host.

**NMV (Name–Type–Value).** The declaration order the typed forms follow:
`defn name ((p T) …) Ret body`, signature at the name.

**Reference cell (`ref`).** A first-class shared mutable box; read with `deref`,
written with `set_ref`. Lowers to a pointer in native code.
