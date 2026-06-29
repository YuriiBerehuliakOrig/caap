# Playbook: Add A Host System Service

Use this when CAAP needs a new system capability across the `caap-sys-runtime`
FFI boundary, such as a new `fs.*`, `os.*`, or `path.*` operation.

The change normally spans two crates:

- `caap-sys-runtime` for implementation and runtime catalog;
- `caap` for contract and policy.

## Checklist

1. Add a `CatalogEntry` to `EXPORT_CATALOG` in
   [`caap-sys-runtime/src/catalog.rs`](../../caap-sys-runtime/src/catalog.rs):

   ```rust
   CatalogEntry::new("fs", "my-operation", 1, Some(1)),
   ```

2. Add the capability/effect mapping in `capability_effect()` in the same file.

3. Implement the operation in the matching runtime module, such as
   `caap-sys-runtime/src/fs.rs`:

   ```rust
   "my-operation" => {
       let p = args.require_str(0, "fs.my-operation")?;
       Ok(SysValue::Str(...))
   }
   ```

4. If this is a new runtime library, add `pub mod <lib>;` in
   `caap-sys-runtime/src/lib.rs`.

5. Add the host export contract in `known_host_export_contract()` in
   [`caap/src/host/fn_misc.rs`](../../caap/src/host/fn_misc.rs): signature,
   return type, effect, and capability/policy tags.

6. Update policy in [`caap/src/host/sys_policy.rs`](../../caap/src/host/sys_policy.rs)
   when the operation needs authorization or result filtering.

No separate registration step is needed. `register_default_system_libraries()`
iterates the catalog and registers delegated exports.

## Example Flow

`fs.read-text` touches:

```text
catalog.rs -> fs.rs -> fn_misc.rs -> sys_policy.rs
```

## Verification

Run:

```bash
cargo test -p caap-sys-runtime
cargo test -p caap-core --test host_system_tests
```
Also test a small compile-time example when practical.
