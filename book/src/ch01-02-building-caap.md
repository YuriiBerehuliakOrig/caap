## Building the `caap` Tool

CAAP lives in a Cargo workspace. The command-line tool you'll use throughout the
book is the `caap` binary produced by the `caap-cli` crate.

### Prerequisites

- A recent **stable Rust toolchain** with `cargo` (and `rustfmt`/`clippy` if you
  intend to work on the compiler itself).
- Optional: **`clang`**, needed only for the native build chapters
  (Chapter 13). Native-executable examples and tests self-skip when the required
  host tools are absent, so you can read along without clang installed.

You do *not* need to set `RUST_MIN_STACK` or any environment variable: the
evaluator and parser grow their stack on demand.

### Building

From the workspace root:

```bash
cargo build -p caap-cli
```

This produces the binary at `target/debug/caap`. For the examples in this book
we'll write `caap` for short; use whichever of these is convenient:

```bash
# 1. Call the built binary directly
./target/debug/caap PROGRAM

# 2. Or go through cargo (rebuilds if needed)
cargo run -p caap-cli -- PROGRAM

# 3. Or put it on your PATH for the session
export PATH="$PWD/target/debug:$PATH"
caap PROGRAM
```

### Sanity Check

Save this single line to a file called `five.caap`:

```scheme
(int_add 2 3)
```

Then run it on the bare kernel:

```bash
$ caap five.caap
5
```

If you see `5`, your toolchain works. You just ran a CAAP program: the reader
turned the text into one form, the evaluator applied the `int_add` builtin to
`2` and `3`, and the tool printed the result.

> **What just happened?** With no bootstrap argument, `caap file.caap` evaluates
> the file on the *bare kernel* and prints the final value. We'll meet the other
> run modes in “How CAAP Runs Code,” and the full invocation grammar is in
> [Appendix B](appendix-02-cli.md).

### Running the Test Suite (Optional)

If you're going to hack on CAAP itself, the local quality gate is:

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The CAAP-native and language tests live in the `caap-core` crate
(`cargo test -p caap-core`).
