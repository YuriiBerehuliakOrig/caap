# Playbook: Add A Runtime Builtin

Use this when CAAP needs a new builtin callable from runtime code, such as a
value, string, sequence, or collection operation.

## Checklist

1. Add the handler in the appropriate module under
   [`caap/src/builtins/`](../../caap/src/builtins/):

   ```rust
   fn my_op_eager(args: Vec<RuntimeValue>) -> Result<RuntimeValue, crate::values::EvalSignal> {
       ...
   }
   ```

2. Register it in that module's `pub fn register(ev: &mut Evaluator)`:

   ```rust
   ev.register_builtin(BuiltinInfo {
       name: "my-op".to_string(),
       metadata: crate::values::BuiltinMetadata::eager_runtime(),
       min_arity: 2,
       max_arity: Some(2),
       handler: crate::values::BuiltinHandler::Eager(Box::new(my_op_eager)),
   });
   ```

3. If you added a new builtins file, call `<module>::register(ev)` from
   `register_all()` in
   [`caap/src/builtins/mod.rs`](../../caap/src/builtins/mod.rs).

## Key Symbols

- `Evaluator::register_builtin` stores the builtin and makes it available
  through `builtin_info(name)`.
- `BuiltinMetadata` selects phase/effect shape: `eager_runtime()`,
  `runtime_mutation()`, `compile_time_pure()`, and
  `.with_builtin_effect(...)`.
- `BuiltinHandler::Eager` receives evaluated arguments.
- `BuiltinHandler::Special` receives `(ev, call, env)` for lazy/special forms.

## Example

Use `int-add` in `caap/src/builtins/arithmetic.rs` as a minimal runtime builtin
example.

## Verification

Run:

```bash
cargo test -p caap-core
scripts/strict-gate.sh
```

For a smoke test, run a small `.caap` file through the debug binary.
