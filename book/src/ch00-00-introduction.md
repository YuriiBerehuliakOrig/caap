# Introduction

Welcome to *The CAAP Programming Language*, an introductory book about CAAP.

CAAP is a programming language built around one unusual idea: **the language is
programmable from the inside.** Most languages give you a fixed grammar, a fixed
set of compiler passes, and a standard library written in some *other*, lower
language. CAAP inverts this. It starts from a tiny kernel and builds everything
else — the standard library, the type and effect checker, even new syntax — *in
CAAP*, evaluated at compile time.

## Who CAAP Is For

CAAP will feel most rewarding if you enjoy languages that take metaprogramming
seriously: Lisp and Scheme (for the homoiconic, parenthesised core), Rust (for
the typed, effect-aware, compile-to-native sensibility), and macro-heavy or
language-workbench systems (for the extensible syntax). You do not need to know
any of those to follow along — but if you do, you will recognise the lineage.

## What Makes CAAP Different

Four ideas run through the whole book:

1. **A tiny kernel.** The kernel is a handful of special forms (`if`, `bind`,
   `lambda`, `do`, `while`, `macro`, …) plus a set of builtins. Everything you
   would expect from a "real" language — modules, structs, a type checker, a
   standard library — lives *above* the kernel, written in CAAP.

2. **Homoiconic source.** A CAAP program is a sequence of *forms*: atoms and
   parenthesised lists. Code is data, and data is code. This is what makes the
   compile-time machinery possible.

3. **Compile-time evaluation (CTFE).** CAAP programs can run code *during
   compilation* that inspects and rewrites the program being compiled. Macros,
   `const` folding, a `show!`-style derive, the type and effect passes — all of
   it is ordinary CAAP running at compile time against the program's own
   intermediate representation (IR).

4. **Many surfaces, one language.** The parenthesised kernel syntax is only one
   *surface*. CAAP ships a C-like surface in which the very same program can be
   written with `{ … }` blocks and `name type = value` declarations. You can
   define your own surface with a PEG grammar.

On top of all that sits a **capability-based effect system** and a **native
backend** that compiles a typed subset of CAAP to LLVM IR and WebAssembly.

## How to Use This Book

The book is meant to be read front to back. Earlier chapters build foundations
that later chapters rely on.

- **Chapters 1–2** get you running CAAP and walk through a complete small
  program so you can see the shape of the language.
- **Chapters 3–6** cover the kernel: forms, bindings, data, functions, control
  flow, collections, strings, and errors. Everything here runs on the bare
  kernel.
- **Chapters 7–9** introduce the standard-library *tower*: modules, structs and
  types, and the effect system.
- **Chapters 10–12** are about programming the compiler itself: macros,
  compile-time evaluation, and extending the syntax.
- **Chapters 13–15** produce real artifacts: native and WebAssembly binaries,
  host I/O, and a capstone project.
- The **appendices** are reference material you will return to: a kernel cheat
  sheet, the CLI contract, a glossary, and pointers to the deeper specifications.

There are two kinds of errors in any language. *Compile-time* errors keep a
broken program from ever running; CAAP surfaces these as diagnostics with a
code like `error[CAAP-RUNTIME-001]`. *Logic* errors are when the program runs
but does the wrong thing. This book tries to help you avoid both — and, in the
chapters on CTFE, shows you how CAAP turns whole classes of logic errors into
compile-time errors you can catch before shipping.

Let's begin.
