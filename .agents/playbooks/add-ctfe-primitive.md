# Playbook: Add A CTFE Primitive

Use this for a builtin that runs during compilation: compiler registry access,
provider/stage registration, IR inspection or construction, annotations, facts,
or provider-context operations.

First check [`KERNEL_REFERENCE.md`](../../KERNEL_REFERENCE.md), especially the
CTFE sections, to avoid duplicating an existing primitive.

## Checklist

1. Choose the builtins module under
   [`caap/src/builtins/`](../../caap/src/builtins/):

   - `compiler_registry.rs`
   - `compiler_providers.rs`
   - `compiler_query.rs`
   - `compiler_units.rs`
   - `provider_context.rs`
   - `ir_builders.rs`

2. Register the primitive in that module's `pub fn register(ev)`:

   ```rust
   ev.register_builtin(BuiltinInfo {
       name: "ctfe-my-primitive".to_string(),
       metadata: crate::values::BuiltinMetadata::compile_time_pure(),
       min_arity: 1,
       max_arity: Some(1),
       handler: crate::values::BuiltinHandler::Special(Box::new(|ev, call, env| {
           ...
       })),
   });
   ```

3. Choose metadata intentionally:

   - `compile_time_pure()` for pure reads;
   - `compile_time_compiler_registry()` for registry mutation;
   - `compile_time_provider()` for provider-context access;
   - `.with_builtin_effect(...)` for explicit effect gates.

4. Add the primitive to `KERNEL_PRIMITIVE_CLASSIFICATIONS` in
   [`caap/src/builtins/mod.rs`](../../caap/src/builtins/mod.rs). Tests enforce
   classification completeness.

5. Update [`KERNEL_REFERENCE.md`](../../KERNEL_REFERENCE.md) when the public
   primitive surface changes.

## Verification

Run:

```bash
cargo test -p caap-core
scripts/strict-gate.sh
```

Also test a small compile-time example when practical.
