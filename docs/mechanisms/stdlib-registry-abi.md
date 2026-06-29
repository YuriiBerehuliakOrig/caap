# The compiler-registry key ABI (`stdlib.boot.registry_contract`)

> Audit item #13 ("Boot/codegen depend too heavily on implicit compiler-registry
> dependency injection"), round-3 Wave B.

## The problem

Boot and codegen modules find their dependencies through **string keys** in the
compiler registry (`ctfe_compiler_lookup_value compiler "stdlib.load" …`), not
through `(use …)`. That keeps the bootstrap flexible and lazy, but:

- the dependency graph is **hidden** from the module system;
- a **missing** key silently degrades to an "optional skip" instead of a contract
  failure;
- an **embedder** must already know the hidden keys to compose a session.

## The contract

`stdlib/boot/registry_contract.caap` is the single formal description:

- **named constants** (`key_load`, `key_expand`, `key_emit_llvm`, …) so call sites
  stop spelling raw literals;
- **`registry_abi`** — a pure-data table: each key's `owner` (the module that
  registers it), `required` tier, an enforceable `shape_kind`, the
  `expected_members` it must expose (for map-shaped keys), and a one-line
  `shape_note`;
- **`required_tiers`** / **`required_tier?`** — the canonical tier enum
  (`core` / `codegen` / `command` / `optional`) and its membership predicate, so
  the tiers can't drift between the comment, the table, and the validators;
- **`shape_kinds`** / **`shape_kind?`** — the canonical shape enum
  (`map_of_functions` / `single_callable` / `prose_only`) and its predicate;
- **`abi_keys`** — every documented key (used by the drift-guard below);
- **`lookup_required(key)`** — a lookup that RAISES a named contract error (key +
  owner) when the key is absent, instead of returning a silent `null`;
- **`validate_keys!(keys)`** — assert a profile registered a set of keys
  (presence only);
- **`validate_shape!(key)`** / **`validate_registry_contract!(keys)`** — assert the
  LIVE registry value matches the documented `shape_kind` (for `map_of_functions`,
  that the value is a map carrying every `expected_members` name; for
  `single_callable`, that it is callable), raising on a mismatch — so a key can no
  longer pass presence while carrying an incompatible value (audit #32).

### The documented keys

| key | owner | tier |
|---|---|---|
| `stdlib.expand`, `stdlib.expand.builders` | `boot/expander.caap` | core |
| `stdlib.check` | `boot/check.caap` | core |
| `stdlib.namespace` | `boot/namespace.caap` | core |
| `stdlib.resolve` | `boot/resolve.caap` | core |
| `stdlib.gate` | `boot/gate.caap` | core |
| `stdlib.reader` | `boot/reader.caap` | core |
| `stdlib.unit_build` | `boot/unit_build.caap` | core |
| `stdlib.load` | `boot/loader.caap` | core |
| `stdlib.semantics.types.infer` | `semantics/types/infer.caap` | core |
| `stdlib.semantics.passes.registry` | `semantics/passes/registry.caap` | core |
| `stdlib.semantics.passes.bare_gate` | `semantics/passes/bare_gate.caap` | core |
| `stdlib.backend.prep` | `backend/prep.caap` | codegen |
| `stdlib.backend.emit.llvm` | `backend/emit/llvm.caap` | codegen |
| `stdlib.backend.emit.wasm` | `backend/emit/wasm.caap` | codegen |
| `caap.session.commands` | `boot/commands.caap` | core |
| `stdlib.native.emit`, `stdlib.{llvm,wasm}.emit*` | `boot/native_emit.caap` | codegen |
| `stdlib.module.{analyze_source,analyze_source_with_root,run_source,run_from_root}` | `boot/analyze.caap`, `boot/run.caap` | command |

The `core` keys are the same ones the bootstrap manifest (#9) asserts per phase
via `check_phase!`; the `codegen` keys are registered lazily by
`boot/native_emit.caap` / `load_module` only when a native/emit profile is used.

## How it stays honest

The governance test **`stdlib_registry_keys_documented`**
(`caap/tests/stdlib_governance_tests.rs`) scans production stdlib code for every
registry key LITERAL passed to a `ctfe_compiler_{lookup,register,bind}_value`
call and asserts each is documented in `registry_abi`. A new undocumented key
fails the build — so the ABI cannot silently go stale. (Dynamic keys — e.g. the
manifest's `(get entry "key")` — are out of scope; the manifest itself is the
source of truth for those, cross-checked by `check_phase!`.)

The in-language test `stdlib/lib/tests/test_registry_contract.caap` adds the
shape half (audit #32): it asserts every row's `required` is a `required_tiers`
member and every `shape_kind` is a `shape_kinds` member (the enums can't drift
from the validators), and it runs `validate_registry_contract!` over the stable
core keys present at base boot (`stdlib.expand` / `stdlib.gate` / `stdlib.load` /
`stdlib.semantics.types.infer` / `caap.session.commands`) so the LIVE values are
checked against their documented shape — not just presence. The lazily-registered
`stdlib.semantics.passes.registry`, codegen, and command keys are absent at base
boot, so they are documented but not live-checked there.

## Not done here (deliberately)

Mechanically rewriting every existing raw `"stdlib.…"` literal to a `key_*`
constant is a broad, low-value churn that risks the byte-identical codegen; the
constants + accessors are provided for new call sites and incremental migration,
not a sweep.
