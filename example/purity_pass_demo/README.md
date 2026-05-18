# Same-file purity pass demo

This example keeps the custom pass in the same CAAP source file that is being
compiled. The only bootstrap argument is the stdlib seed; the source file uses
the bootstrap-loaded `stdlib.pass-kit` registry helpers to register
`example.purity-pass.check` and the manual CTFE entry
`example.purity-pass.check-now` during compile-time evaluation.

The demo is a direct source-file example, not a module-root package. The
source file loads `sys.io`, imports the real `println` module export, and the
pass reads existing call semantics (`effect_policy`) instead of matching the
name syntactically or writing its own effect facts.

The demo pass treats the checked top-level lambdas as pure by contract. The
default policy is advisory so the same run can show compile-time diagnostics
and runtime output:

- impure call inside a checked function: warning
- missing call semantics/effect metadata: warning
- `main` is not checked directly, so it can call a function that the pass
  reports separately.

The source file emits two compile-time invocation diagnostics:

- `example.purity.invocation: manual invocation: example.purity-pass.check-now`
- `example.purity.invocation: pipeline invocation: example.purity-pass.check`

The first one comes from an explicit `(example.purity-pass.check-now)` call in
`main`, folded by CTFE before runtime. The second one comes from the registered
provider callback when the compiler pipeline invokes it on `validate_graph`.
Both paths call the same CAAP function, `purity-pass-implementation`.

Compile/check:

```bash
cargo run --manifest-path caap/Cargo.toml -- compile \
  --bootstrap stdlib/bootstrap.caap \
  --target check \
  example/purity_pass_demo/demo.caap
```

Expected result: compile exits successfully and emits both invocation
diagnostics plus warning `example.purity.impure_call_in_function` for
`impure-print`. The diagnostic is attached to the impure call and includes the suggestion:
`mark callee as pure, remove the side effect, or refactor the function`.

Run:

```bash
cargo run --manifest-path caap/Cargo.toml -- run \
  --bootstrap stdlib/bootstrap.caap \
  example/purity_pass_demo/demo.caap
```

Expected result: the same compile-time diagnostics and warning are emitted, and
runtime executes the imported `sys.io.println` call.
To make the pass strict, change `impure_call_severity` in `demo.caap` from
`"warning"` to `"error"`.

Emit LLVM IR for the runtime subset:

```bash
cargo run --manifest-path caap/Cargo.toml -- llvm-ir \
  --bootstrap stdlib/bootstrap.caap \
  example/purity_pass_demo/demo.caap
```

Expected result: LLVM IR contains runtime functions such as `main`,
`pure-add`, `pure-with-nested-lambda`, and `impure-print`. Compile-time setup
forms, provider registration, and `purity-pass-implementation` are not emitted.

Native executable compile uses the same default emitter:

```bash
cargo run --manifest-path caap/Cargo.toml -- compile \
  --bootstrap stdlib/bootstrap.caap \
  --target native-exe \
  -o /tmp/caap_purity_demo \
  example/purity_pass_demo/demo.caap
```
