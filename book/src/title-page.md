# The CAAP Programming Language

*by The CAAP Project*

This version of the text assumes you are building `caap` from the workspace in
this repository with a recent stable Rust toolchain.

This book teaches CAAP: a small, homoiconic language whose kernel is deliberately
tiny, whose standard library is written in CAAP itself, whose **surface syntax is
extensible**, and which lets programs **participate in their own compilation**
through compile-time evaluation (CTFE). A typed subset compiles to native code
and WebAssembly.

> **Note on examples.** Code blocks marked *“runnable on the bare kernel”* can be
> saved to a file and run directly with `caap file.caap` — they were checked
> against the `caap` tool built from this repository. Examples that use the
> standard library, types, surfaces, or the native backend run on the *bootstrap
> tower* and are built with the `tools/*.caap` programs; those are drawn from the
> real, tested example corpus in `examples/`.
