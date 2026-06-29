## What Is CAAP?

CAAP is a small language with a big idea. To understand the language you first
need a picture of how it is layered, because almost every feature lives at a
specific layer and behaves differently depending on where it sits.

### The Three Layers

```text
            ┌──────────────────────────────────────────────┐
            │  Surfaces      C-like syntax, your own PEG     │
            ├──────────────────────────────────────────────┤
            │  Tower (stdlib)  modules · structs · types ·  │
            │                   effects · macros · backends  │   written in CAAP,
            ├──────────────────────────────────────────────┤   loaded at compile time
            │  Kernel        forms · special forms · builtins │
            └──────────────────────────────────────────────┘
```

**The kernel** is the language proper. It is tiny on purpose: a reader that
turns text into *forms*, a small set of *special forms* (`if`, `bind`,
`lambda`, `do`, `while`, `block`, `macro`, `try`, `effect_scope`, …), and a set
of *builtins* (arithmetic, comparisons, collections, strings, reflection). You
can run kernel programs directly. Nothing in the kernel knows about modules,
structs, types, or the standard library.

**The tower** — the standard library, called *stdlib* in the source — is
written *in CAAP* and loaded by a **bootstrap** at compile time. When you run a
program "on the tower," the compiler first evaluates the bootstrap, which
registers modules, the `struct` and `defn` forms, the type and effect checker,
the macro/derive machinery, and the native backends. Only then is your program
compiled against all of that.

**Surfaces** are alternative *syntaxes* for the same language. The default
surface is the parenthesised kernel syntax you'll see throughout this book. The
tower also ships a **C-like surface** in which a program is written with curly
braces and `name type = value` declarations — and is still exactly the same
CAAP underneath. You can define new surfaces with a PEG grammar.

### Homoiconic: Code Is Data

A CAAP program is a sequence of **forms**. A form is either an *atom* (a number,
string, boolean, `null`, or a symbol) or a *list* written in parentheses:

```text
(int_add 2 3)
```

That list has a head, `int_add`, and two arguments, `2` and `3`. *Every* compound
construct in CAAP — a function call, a binding, a conditional, a loop — is a list
with a head symbol. Because the program is just nested lists, programs can build
and inspect programs as ordinary data. This property, called *homoiconicity*, is
the foundation of CAAP's compile-time machinery (Chapters 10–12).

### Programmable at Compile Time

Here is the idea that gives the language its character. In most languages the
compiler is a black box: it has a fixed set of passes and you cannot add your
own. In CAAP, **compile-time evaluation (CTFE)** lets ordinary CAAP code run
*during compilation* with access to the program's own intermediate
representation. The type checker, the effect checker, `const` folding, macros,
and a `show!`-style derive are all just CAAP programs registered as compiler
*providers*. When you learn to write one, you are extending the compiler.

This is why the standard library can be written in CAAP and why the syntax can
be extended from within: the layers above the kernel are not privileged native
code, they are CAAP that runs at compile time.

### What This Buys You

- A language you can grow from inside, without forking a compiler.
- A type and effect system delivered as a library, not baked into the kernel.
- Multiple front-end syntaxes over one semantic core.
- A path to fast, freestanding native and WebAssembly binaries for a typed
  subset.

The next sections get you running code so these ideas stop being abstract.
