# Value & Type Model

**Source:** [values.rs](../../caap/src/values.rs) (`RuntimeValue`),
[ir.rs:215](../../caap/src/ir.rs#L215) (`Node`),
[reflect.rs](../../caap/src/builtins/reflect.rs) (`value_type`); the active
stdlib type layer in [stdlib/semantics/types/](../../stdlib/semantics/types/).

CAAP has two distinct "type" surfaces: the **runtime value model** (built into
the kernel) and the **stdlib `types` layer** (a library that adds structs,
enums, generics, and a checker on top of the runtime model).

## Runtime values

`RuntimeValue` ([values.rs:103](../../caap/src/values.rs#L103)) is the universe
of values the evaluator manipulates:

```
Null · Bool · Int(i64) · Float(f64) · Str · Bytes · Tuple
Closure · Macro · Builtin · HostFunction · HostObject · Ref
List · Map · UninitializedTopLevel
```

### `value_type`

`value_type` ([reflect.rs](../../caap/src/builtins/reflect.rs)) maps a value to a
stable string tag — the primary way CAAP code branches on shape:

| Tag | Runtime values |
|---|---|
| `"null"` | `Null`, `UninitializedTopLevel` |
| `"bool"` | `Bool` |
| `"int"` | `Int` |
| `"float"` | `Float` |
| `"string"` | `Str` |
| `"bytes"` | `Bytes` |
| `"list"` | `List` |
| `"tuple"` | `Tuple` |
| `"map"` | `Map` |
| `"ref"` | `Ref` |
| `"callable"` | `Closure`, `Macro`, `Builtin`, `HostFunction` |
| `"object"` | `HostObject` |

Companion reflection builtins: `host_value_kind` (finer host-object kind),
`value_to_string`, `gensym` (fresh symbol).

### Collections

- **List** (`list`) — ordered, mutable-in-place via `append` / `assoc` /
  `list_remove_at` (these carry the `runtime_mutation` effect).
- **Map** (`map`) — string-keyed; `map_of`, `get`, `assoc`, `contains`,
  `map_keys`, `map_values`, `map_merge`, `map_update`, `map_delete`,
  `map_of_entries`.
- **Tuple** (`tuple`) — immutable fixed sequence; surfaces e.g. as lambda
  parameter lists in the IR.
- **Bytes** (`bytes`) — immutable binary blob, mirroring `Str`
  ([bytes.rs](../../caap/src/builtins/bytes.rs)).

There is **no separate record/struct value kind** in the kernel: a "record" is a
`Map`. Structs/enums are a stdlib construction (below).

## The IR node model

Programs (and the data passes manipulate) are trees of exactly **three** node
kinds ([ir.rs:215](../../caap/src/ir.rs#L215)):

| Node | Holds |
|---|---|
| `Name` | an identifier (a variable / function reference) |
| `Literal` | a constant value |
| `Call` | a callee node + argument nodes |

Everything else — `bind`, `lambda`, `if`, `do`, `while` — is a `Call` whose
callee is a `Name` for a **special form** (see the
[kernel reference](../builtins.md) "Control flow & special forms"). This
minimal IR is what makes CAAP uniformly inspectable and rewritable.

## The stdlib `types` layer

[stdlib/semantics/types/](../../stdlib/semantics/types/) builds a structural type/effect system
**on top of** the runtime model — it does not change the kernel. Submodules
(see the [stdlib reference](../stdlib-reference.md) for exact exports):

| Module | Role |
|---|---|
| `stdlib.semantics.types.registry` | primitive/sized/pointer/struct/alias/enum/generic descriptors |
| `stdlib.semantics.types.records` | inert marker records emitted by forms |
| `stdlib.semantics.types.effects` | effect-tag inference and ownership analysis |
| `stdlib.semantics.types.infer` | type/effect walker and module signature store |

Because these are libraries, type checking is stdlib loader policy: expanded
modules are checked before evaluation, and signatures/effects travel through
`use` / `import` across module boundaries.
