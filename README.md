# CAAP v1

Rust workspace for the CAAP core port, PEG parser port, standard library
bootstraps, and runnable CAAP examples. The repository is organized as a
single Cargo workspace so the core, parser, and stdlib-facing tests can be
verified together.

## Layout

- `caap/` - CAAP core library and `caap` CLI binary.
- `peg/` - PEG/parser support crate.
- `stdlib/` - CAAP standard library bootstrap sources.
- `example/` - source-level examples, including the purity pass demo.

## Prerequisites

- Rust stable toolchain with `cargo`, `rustfmt`, and `clippy`.
- Optional: `clang`, `ar`, and `runtime/csys` sources for
  `compile --target native-exe`. If the runtime sources are absent, the native
  executable test self-skips.

## Verification

Run the full local quality gate:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Run a focused crate test:

```bash
cargo test -p caap-core-port
cargo test -p caap-peg-port
```

## CLI Examples

```bash
cargo run --manifest-path caap/Cargo.toml -- compile \
  --bootstrap stdlib/bootstrap.caap \
  --target check \
  example/purity_pass_demo/demo.caap
```

```bash
cargo run --manifest-path caap/Cargo.toml -- run \
  --bootstrap stdlib/bootstrap.caap \
  example/purity_pass_demo/demo.caap
```

## Notes

- `Cargo.lock` is committed because this workspace ships runnable binaries and
  integration tests.
- Native executable builds cache generated C runtime artifacts under
  `.caap_build/`, which is ignored.
- The root `.git` directory in this checkout may need to be initialized or
  restored before normal Git operations work.
