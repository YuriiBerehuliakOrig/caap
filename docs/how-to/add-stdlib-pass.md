# How to add an stdlib semantic pass

The loader's pre-eval pipeline is **expand → check → typecheck → user passes**.
[stdlib/semantics/passes/registry.caap](../../stdlib/semantics/passes/registry.caap)
(`stdlib.semantics.passes.registry`) is the user's seat at that table: register a
pass and every subsequently loaded module is walked by it, with findings failing
the load exactly like the built-in phases (same located protocol, same sink). The
prep gate runs the SAME registry before native codegen, so a pass guards both the
loader and the backend.

## First decide: analysis or transform?

One mechanism per granularity (read the registry header — it states this):

- **per-head rewrite** → a `define_form` (forms ARE compile-time functions); not a
  pass.
- **analysis** (read-only walk, reports findings) → a **pass**:
  `install_pass!` / `register_pass!`. This is the common case.
- **whole-module IR rewrite** → a **transform**: `install_transform!` /
  `register_transform!` (constfold / simplify / dce / peval are transforms).

## The pass contract

A pass is a plain function `(located sink) -> null`:

- `located` — `[{node, loc}]`, the module's EXPANDED top-level forms, in order.
- `sink` — append located finding STRINGS (`"path:line:col: message"`); use
  `note!` (from the registry) for the common append. A non-empty sink fails the
  load.

## The four touch points

1. **A new file** `stdlib/semantics/passes/<name>.caap` — mirror an existing one
   ([escape.caap](../../stdlib/semantics/passes/escape.caap) or
   [borrow.caap](../../stdlib/semantics/passes/borrow.caap)):

   ```
   (module stdlib.semantics.passes.<name>)
   (use stdlib.syntax.ast …)                       ; the node accessors you need
   (use stdlib.semantics.passes.registry note! install_pass!)

   ; check_module — the pass entry: walk each located form, note! findings.
   (bind check_module (lambda (located sink) …))

   ; register! — install as a load-time pass (idempotent).
   (bind register! (lambda () (install_pass! "<name>" check_module)))

   (export check check_module register!)
   ```

2. **Ordering (only if it consumes another pass's output)** — register with a
   `deps_of after before requires` map via the `_with!` constructors. `requires`
   names a pass that MUST be installed (fails loudly if absent); `after`/`before`
   are soft hints. Default (`install_pass!`) is append-order. Example from borrow:
   `(install_pass_with! "borrow" run (deps_of (list_of) (list_of) (list_of "alias")))`.

3. **Cross-pass facts (optional)** — an analysis records what it learned with
   `fact!`/`fact_typed!`; a later pass or the codegen driver reads it back with
   `fact_of`/`fact_typed_of`. See the registry's "facts" section.

4. **A test** — `stdlib/lib/tests/test_<name>.caap`, exercising the pass's exported
   `check` DIRECTLY on hand-built forms (the pattern in
   [test_borrow.caap](../../stdlib/lib/tests/test_borrow.caap) /
   [test_escape.caap](../../stdlib/lib/tests/test_escape.caap)). This file joins
   the in-language corpus automatically.

## Activation

Defining `register!` does not run it — a session activates a pass by CALLING
`register!` (the registry module must be loaded). Built-in TRANSFORMS are wired in
the boot layer (e.g. [boot/native_emit.caap](../../stdlib/boot/native_emit.caap)
and `boot/peval.caap` call peval's `register!`); analysis passes export `check` and
are exercised directly by their `test_*.caap`. Wire your `register!` wherever the
pass should be live (a boot file for an always-on pass; the test/session otherwise).
Sessions that never load the registry kit pay nothing.

## Verify (run these, in order)

`cargo build -p caap-cli` first.

```bash
# 1. the new pass + test still parse
python3 scripts/caap_refactor.py check stdlib/semantics/passes/<name>.caap \
    stdlib/lib/tests/test_<name>.caap

# 2. the in-language corpus (includes your test_<name>.caap)
cargo test -p caap-core --test stdlib_loader_tests \
    stdlib_run_all_in_language_tests -- --ignored

# 3. if the pass guards native builds too, confirm the prep gate still runs clean
cargo test -p caap-core --test stdlib_codegen_tests \
    stdlib_native_prep_gate_rejects_typo_before_codegen
```
