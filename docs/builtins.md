# CAAP Kernel Builtins Reference

Grounded in `caap/src/builtins/*.rs` registration sites — **351 builtins**
(the *public* registry surface: names registered `.internal()`, such as the
`set!` lowering target `assign_lexical`, are excluded). Identifiers are
snake_case except the two byte-conversion arrows (`string->bytes`,
`bytes->string`). This doc is enforced by
`caap/tests/builtins_doc_bijection_tests.rs`: every registered builtin must
appear here, every table entry must be registered, and the count above must
match the registry.

**Reading the tables**
- **arity** — `n` exact · `n..m` range · `n+` unbounded.
- **phase** — derived from the registration metadata constructor:
  - `comptime·pure` (`compile_time_pure`), `comptime·impure`, `comptime·eff`
    (effect-bearing), `comptime·registry` (mutates compiler registry),
    `comptime·write-ir`, `comptime·read-files`, `comptime·diag` — usable only at
    **compile time** (in macros / providers / bootstrap).
  - `runtime` (`eager_runtime`), `runtime·mut` (`runtime_mutation`), `runtime·seq`
    (`runtime_sequential`) — ordinary functions; pure ones are also foldable at
    compile time when their arguments are static (see
    [dual-phase execution](mechanisms/dual-phase-execution.md)).
  - `special-form` — evaluated specially (lazy / non-standard argument
    evaluation); the canonical control forms.

See also the [conceptual mechanism pages](mechanisms/README.md).

---

## Substrate ↔ policy boundary

Builtins are **substrate, never policy** (principles
[#1 Minimal Semantic Kernel](principles.md) and
[#14 Core Provides Substrate, Stdlib Owns Policy](principles.md)). Concretely:

- **The IR stays `Name | Literal | Call`.** No builtin adds an IR node kind; a
  language construct (`if`, `lambda`, `match`, a type system, OOP, a pass) is a
  `Call` whose callee names it, defined in stdlib `.caap` — not in core. This is
  locked at compile time by `ir_kernel_stays_name_literal_call_only`
  (`caap/src/ir.rs`): the exhaustive matches there fail to build if the kernel
  grows a fourth node.
- **Every compile-time (`ctfe_*`) primitive is a classified *mechanism*.** The
  `KernelPrimitiveClass` vocabulary (`caap/src/builtins/mod.rs`) is entirely
  mechanism/integration/introspection — there is no `Policy` class, because core
  owns *how* (query, provider, unit, grammar, IR construction, metadata,
  semantic resolution, surface, syntax-tree), not *what*. The test
  `compiler_and_provider_kernel_primitives_are_explicitly_classified` enforces a
  bijection: a new `ctfe_*` builtin that is not classified — or a classification
  with no registered builtin — fails the suite. Adding one forces you to name
  which mechanism it is, which is the moment to ask "is this mechanism or policy?"
- **The richest CTFE surface — the `compiler_*` builtins — is mechanism too.**
  It is a large query/provider/unit *substrate* that stdlib passes drive; it must
  not encode a specific language's semantics. When extending it, the builtin
  should expose a capability, and the meaning should live in a stdlib pass.

The grammar-engine surface (incremental parsing, AST/diff, validation, registry,
prefix/profiled) lives in `caap/src/builtins/grammar/engine.rs`; it is mechanism
over `caap-peg`, classified under `GrammarMechanism`.

---

## Control flow & special forms

`control_flow.rs` (13 public) + `effects.rs` (1). Special forms control their own
argument evaluation; in the IR they appear as `Call`s whose callee is a `Name`.

| builtin | arity | phase | summary |
|---|---|---|---|
| `bind` | 2+ | special-form | Introduce bindings. **Two scoping behaviours, selected by the first argument's shape**: the paired form `(bind ((n v)…) body)` evaluates in a fresh child scope with all names pre-declared (letrec — mutual recursion works; names vanish after the body); the flat form `(bind name v rest…)` defines into the *current* scope sequentially (the name stays visible to following sibling forms — the define-like workhorse, ~1.6k uses across `.caap`). Both are load-bearing; the shape is the intent signal. |
| `lambda` | 2+ | special-form | Create a closure: `(lambda (params…) body)`. |
| `do` | 0+ | special-form | Evaluate forms in sequence; value is the last. |
| `if` | 2..3 | special-form | Conditional. Two-arg (no else) is compile-time-only — the LLVM backend rejects it. |
| `while` | 2 | special-form | `(while cond body)` loop. |
| `block` | 1+ | special-form | Labeled block usable as a `leave` target. |
| `leave` | 1..2 | special-form | Exit an enclosing `block` (optionally with a value). |
| `and` | 0+ | special-form | Short-circuit logical AND. |
| `or` | 0+ | special-form | Short-circuit logical OR. |
| `match` | 1+ | special-form | Pattern match / dispatch. |
| `macro` | 2+ | special-form | Define a macro (a compile-time form transformer). |
| `throw` | 1 | special-form | Raise a catchable value. |
| `try` | 1..2 | special-form | Evaluate a body, catching `throw`n values **and (non-fatal) evaluation errors** — the handler gets the thrown value as-is, or `{"message", "category"}` for an error. FATAL errors (step/depth budget exhaustion) pierce `try`: catching them would void the resource guarantee. |
| `effect_scope` | 1+ | special-form | Delimit an effect scope (capability boundary). |

> Local mutation is the surface form `(set! name v)` — the frontend lowers it to
> the *internal* `assign_lexical` builtin, which is not directly callable
> (enforced by test). Collection mutation is `append`/`assoc`/`set`.

## Arithmetic, comparison & boolean

`arithmetic.rs` (49). All `runtime` (pure → foldable when arguments are static).
**Defined semantics:** i64 arithmetic is *checked* — overflow (`int_add`/`int_sub`/
`int_mul`/`int_abs`, `int_div` of `MIN / -1`) and division by zero are clean
runtime errors (catchable by `try` since 2026-06-12), never a silent wrap or build-profile-dependent panic. Ordered
comparison (`lt`/`gt`/`le`/`ge`) on incomparable types (bool, null, collections,
callables, refs — or int vs string) is a type error, not a silent `false`;
the numeric Int↔Float mix and Str/Str stay comparable. `eq`/`ne` across types
remain plain inequality, and `sequence_sort_by` keeps its separate total order.

| builtin | arity | summary |
|---|---|---|
| `int_add` `int_sub` `int_mul` `int_div` `int_rem` `int_mod` | 2 | Integer arithmetic (`div`/`rem`/`mod`). |
| `int_abs` | 1 | Integer absolute value. |
| `int_and` `int_or` `int_xor` `int_not` | 1–2 | Bitwise integer ops. |
| `int_shl` `int_shr` | 2 | Bit shifts left / right. |
| `int_to_float` `float_to_int` | 1 | Numeric conversions. |
| `float_add` `float_sub` `float_mul` `float_div` | 2 | Floating-point arithmetic (`float_div` rejects division by zero). |
| `float_abs` `float_min` `float_max` | 1–2 | Float absolute value / minimum / maximum. |
| `float_nan?` `float_inf?` | 1 | Float classification predicates. |
| `sqrt` `exp` `log` `log2` `log10` | 1 | Root, exponential, logarithms. |
| `pow` | 2 | **Float** power (`base.powf(exp)`; both args must be floats — integer power is a stdlib loop). |
| `sin` `cos` `tan` `asin` `acos` `atan` | 1 | Trigonometry. |
| `atan2` | 2 | Two-argument arctangent. |
| `floor` `ceil` `round` | 1 | Float rounding. |
| `float_to_bits` `float_to_bits_f32` | 1 | Exact IEEE-754 bit patterns as int (f64 bits; value narrowed to f32 bits) — the substrate for lossless codegen emission (`0xH…` LLVM constants). |
| `bits_to_float` `bits_to_float_f32` | 1 | Inverse bit-casts: an int bit pattern reinterpreted as f64 / f32 — lets the integer-only eval byte runtime round-trip floats (e.g. storage float fields), preserving eval = native parity. |
| `eq` `ne` | 0+ | Equality / inequality (variadic chain). |
| `value_eq` | 0+ | **Structural** equality (variadic chain): lists/tuples element-wise, maps by key set (order-insensitive), scalars/identities exactly like `eq`; cycle-safe. Kernel twin of the stdlib `deep_eq` facade. |
| `value_compare` | 2 | **Total** structural order over ANY two values → `-1`/`0`/`+1`. Orders by a stable type-rank (null < bool < int < float < string < bytes < tuple < list < map < callables/refs) then structurally within a kind (lists/tuples lexicographically, maps by sorted key/value pairs); no int/float coercion. Cycle-safe and consistent with `value_eq` (`0` exactly when `value_eq`). The order twin of `value_eq` — the keystone for sorted collections, structural map keys, and cache fingerprints. |
| `value_hash` | 1 | **Structural** hash of any value → int. `value_eq`-equal values hash equal (maps fold order-independently); deterministic across runs for data kinds (identity-typed values contribute only their kind tag). Cycle-safe. |
| `lt` `gt` `le` `ge` | 0+ | Ordered comparisons (variadic chain). |
| `not` | 1 | Logical negation. |

> `and`/`or` are **special forms** (short-circuiting) — see Control flow.

## Strings

`strings.rs` (19). All `runtime` (pure → foldable; note folding only materializes
scalar results, so a string-producing fold is left as a runtime call).

| builtin | arity | summary |
|---|---|---|
| `string_concat_many` | 0+ | Concatenate any number of strings. |
| `string_slice` | 2..3 | Substring by byte range. |
| `string_chars` | 1 | The string's characters as a list of 1-char strings — O(n) iteration (the per-index slice loop is O(n²)). |
| `string_split` | 2 | Split on a separator → list. |
| `string_lines` | 1 | Split into lines. |
| `string_replace` | 3 | Replace occurrences. |
| `string_find` | 2..3 | Index of a substring (optional start). |
| `string_contains` `string_starts_with` `string_ends_with` | 2 | Substring predicates. |
| `string_trim` | 1 | Trim surrounding whitespace. |
| `string_upcase` `string_downcase` | 1 | Case conversion. |
| `string_repeat` | 2 | Repeat a string n times. |
| `string_last_segment` | 2 | Last segment after a separator (e.g. namespace tail). |
| `string_byte_length` | 1 | Length in bytes. |
| `string_to_int` `string_to_float` | 1 | Parse to number. |
| `int_to_string` | 1 | Format an integer. |
| `stable_hash` | 1 | Deterministic content hash (stable across runs). |

## Bytes

`bytes.rs` (5). Immutable binary blobs (`bytes`), registered via the `eager()`
helper. The two converters keep arrow names. **Deliberately a conversion-only
bridge**: sys host services traffic in binary (`SysValue::Bytes`), so the
language can convert and measure — in-language binary *processing*
(slice/search/concat) is out of scope until a consumer exists; revisit then.

| builtin | arity | summary |
|---|---|---|
| `bytes_length` | 1 | Length of a bytes value. |
| `bytes_from_list` | 1 | Build bytes from a list of byte integers. |
| `bytes_to_list` | 1 | Explode bytes to a list of integers. |
| `string->bytes` | 1 | UTF-8 encode a string to bytes. |
| `bytes->string` | 1 | Decode bytes as UTF-8 to a string. |

## Collections & sequences

`mutable.rs` (10) + `sequences.rs` (32). Map/list constructors, the sequence
algorithm substrate the stdlib `sequence` module wraps, and first-class
references.

### Construction & mutation (`mutable.rs`)

| builtin | arity | phase | summary |
|---|---|---|---|
| `list_of` | 0+ | runtime | Build a list from its arguments. |
| `map_of` | 0+ | runtime | Build a map from alternating key/value args. |
| `append` | 2+ | runtime·mut | Append item(s) to a list in place. |
| `assoc` | 3+ | runtime·mut | Set key→value in a map in place. |
| `set` | 3 | runtime·mut | Set an index/key (mutating). |
| `map_delete` | 2 | runtime·mut | Remove a key from a map. |
| `list_remove_at` | 2 | runtime·mut | Remove an element at an index. |

### References (`mutable.rs`)

First-class mutable cells (`RuntimeValue::Ref`). **Why:** so the language can
*hold a reference to a value and mutate through it* — aliasing and
mutate-in-place — instead of copying maps/lists on every update; and so the LLVM
backend has a value to lower to a pointer. Equality is **cell identity**, not
contents. See [reference_tests.rs](../caap/tests/reference_tests.rs).

| builtin | arity | phase | summary |
|---|---|---|---|
| `ref` | 1 | runtime | Box a value into a fresh shared cell → a reference. |
| `deref` | 1 | runtime | Read the value currently in the cell (errors on a non-reference). |
| `set_ref` | 2 | runtime·mut | Write a new value into the cell; every alias sees it. |

> **Iteration order contract (2026-06-12):** runtime maps iterate in
> **insertion order** (IndexMap backing) — `map_keys`/`map_values`, display,
> and every walk follow construction order, deterministically. `map_delete`
> preserves the remaining order. Struct-like maps keep their declared field
> order without sorting.

### Access & algorithms (`sequences.rs`)

| builtin | arity | summary |
|---|---|---|
| `get` | 2..3 | Indexed/keyed lookup with optional default. |
| `get_strict` | 2 | Lookup that errors on a missing key. |
| `contains` | 2 | Membership / has-key test. |
| `size` | 1 | Length of a list/map/string. |
| `map_keys` `map_values` | 1 | Keys / values of a map. |
| `map_of_entries` | 1 | Build a map from `[k v]` pairs. |
| `map_merge` | 0+ | Merge maps (later wins). |
| `map_update` | 3..4 | Apply a function to the value at a key. |
| `sequence_range` | 2 | Half-open integer range `[start,end)`. |
| `sequence_map` `sequence_filter` `sequence_fold_left` | 2–3 | Map / filter / left fold. |
| `sequence_each` `sequence_each_indexed` `sequence_each_pair` | 2 | Iterate (value / index / pair). |
| `sequence_find` `sequence_find_reverse` | 2 | First / last match. |
| `sequence_any` `sequence_all` `sequence_count` | 2 | Existential / universal / count by predicate. |
| `sequence_index_of` | 2 | Index of first predicate match (or -1). |
| `sequence_take` `sequence_drop` `sequence_slice` | 2–3 | Prefix / suffix / slice. |
| `sequence_reverse` | 1 | Reverse order. |
| `sequence_flatten` | 1 | Flatten one level. |
| `sequence_zip` | 1+ | Zip sequences into pairs. |
| `sequence_sort_by` | 2 | Stable sort by a key function (O(n log n) substrate). |
| `sequence_unique_by` | 1..2 | Stable distinct (optional key function). |
| `sequence_group_by` | 2 | Group items by key → map of key→list. |
| `sequence_join` | 2 | Join a sequence of strings with a separator. |

## Reflection & value identity

`reflect.rs` (8).

| builtin | arity | phase | summary |
|---|---|---|---|
| `value_type` | 1 | runtime | Stable type tag (`"int"`/`"string"`/`"map"`/`"callable"`/…). See [value model](mechanisms/value-and-type-model.md). |
| `host_value_kind` | 1 | runtime | Finer kind of a host object. |
| `value_to_string` | 1 | runtime | Human-readable rendering of any value. |
| `gensym` | 0..1 | runtime | Fresh unique symbol (optional prefix). |
| `apply` | 2+ | runtime·seq | Apply a callable: `(apply f fixed… arg_list)` spreads the last argument (a list). The one call-with-computed-args primitive — the positional-spread spelling (`invoke`) was removed as a duplicate. |
| `runtime_error` | 1 | runtime | Raise an error (`CAAP-RUNTIME-001`); fails a fold conservatively at compile time. |
| `ctfe_kernel_vocabulary` | 0 | The callable vocabulary as data: name → `{kind, params, result, min_arity, max_arity, pure, effects}`. `params`/`result` use the stdlib type notation (`int`/`float`/`string`/`bool`/`list`/`map`/`bytes`/`ref`/`callable`/`null`/`any`; a `*`-prefixed final param = remaining args of that type); undeclared builtins surface the polymorphic `["*any"] → "any"`. **Why:** load-time checkers in the language drop their manual signature tables. |
| `ctfe_debug_frames` | 0 | **Diagnostics-class only**: the live closure-call stack, outermost first — `[{name, span}…]` (span = call site). Marked impure (pure scopes cannot observe names; never folded); host callbacks/sub-evaluators absent; a TCO loop is one frame. For REPL and pass tracing — never semantics. |

---

## Compiler registry, queries & host services

### Registry & values — `compiler_registry.rs` (4)

| builtin | arity | phase | summary |
|---|---|---|---|
| `ctfe_compiler_register_value` | 3 | comptime·registry | Register a named value in the compiler session. |
| `ctfe_compiler_lookup_value` | 2..3 | comptime·pure | Look up a registered value (optional default). |
| `ctfe_compiler_builtin_semantic_entries` | 1 | comptime·pure | The semantic entries for builtins. |
| `ctfe_compiler_emit_event` | 4..5 | comptime·registry | Emit a build/trace event. |

### Stage / provider / schema registration — `compiler_providers.rs` (6)

See [provider pipeline](mechanisms/provider-pass-pipeline.md).

| builtin | arity | summary |
|---|---|---|
| `ctfe_compiler_stage_register` | 2..7 | Register a pipeline stage (deps + family). |
| `ctfe_compiler_provider_register` | 4..7 | Register a provider into a stage (deps, effects, spec). |
| `ctfe_compiler_register_semantic_policy` | 3..4 | Register a semantic policy. |
| `ctfe_compiler_register_base_semantic_entries` | 2 | Register base semantic entries. |
| `ctfe_compiler_fact_schema_register` | 3..5 | Register a fact predicate schema. |
| `ctfe_compiler_fact_schema_type_bridge_register` | 3 | Register a fact-type bridge. |

### Queries & bootstrap — `compiler_query_runtime.rs` (14)

| builtin | arity | summary |
|---|---|---|
| `ctfe_compiler_execute_bootstrap_file` | 2..3 | Execute a bootstrap `.caap` file (side-effecting). |
| `ctfe_compiler_evaluate_bootstrap_file` | 2..5 | Evaluate a bootstrap file and return its result. |
| `ctfe_compiler_load_surface_file_template` | 2..3 | Load a surface-file template. |
| `ctfe_compiler_query_execution` | 3..5 | Run the pipeline (or a sub-range) and return unit + facts. |
| `ctfe_compiler_evaluate_capture` | 3..5 | Evaluate capturing value + diagnostics. |
| `ctfe_compiler_lookup_unit` | 2..3 | Look up a registered unit. |
| `ctfe_compiler_register_unit` | 3 | Register a unit in the catalog. |
| `ctfe_compiler_list_stages` | 1 | List registered stages. |
| `ctfe_compiler_list_providers` | 1 | List registered providers. |
| `ctfe_compiler_list_semantic_policies` | 1 | List semantic policies. |
| `ctfe_compiler_provider_schedule` | 2..3 | Inspect/derive provider schedule. |
| `ctfe_compiler_current_bootstrap_context` | 1 | The current bootstrap context. |
| `ctfe_compiler_is_file` | 2 | Does a path exist as a file (compile-time FS read). |
| `ctfe_compiler_list_dir` | 2 | List a directory (compile-time FS read). |

### Host services — `host_services.rs` (5)

The bridge to host capabilities (IO, time, …) exposed to `sys.*` modules under
capability control.

| builtin | arity | summary |
|---|---|---|
| `host_service_export` | 2..3 | Export a host service binding. |
| `host_service_capability` | 1 | Declare/inspect a host capability. |
| `host_service_capability_export` | 3..4 | Export a capability-gated host service. |
| `host_service_libraries` | 0..1 | The available host service libraries. |
| `host_service_library_catalog` | 1..2 | Catalog of a host service library. |

---

## CTFE: IR nodes, units, facts & annotations

`compiler_units_runtime.rs` (57) + `ir_builders.rs` (6). The read/inspect + build
surface for compile-time metaprogramming. See
[CTFE & surface forms](mechanisms/ctfe-and-surface-forms.md).

### Node inspection (`ctfe_node_*`, all comptime·pure)

| builtin | arity | summary |
|---|---|---|
| `ctfe_node_kind` | 1 | `"name"`/`"literal"`/`"call"`. |
| `ctfe_node_is_name` `ctfe_node_is_literal` `ctfe_node_is_call` | 1 | Kind predicates. |
| `ctfe_node_id` | 1 | Node id. |
| `ctfe_node_name_identifier` | 1 | Identifier of a `Name`. |
| `ctfe_node_literal_value` | 1 | Value of a `Literal`. |
| `ctfe_node_call_callee` `ctfe_node_call_args` | 1 | Callee / args of a `Call`. |
| `ctfe_node_children` | 1 | Child node ids. |
| `ctfe_node_parent` `ctfe_node_ancestor?` | 1–2 | Parent / ancestor test. |
| `ctfe_node_live?` | 1 | Still present in the graph? |
| `ctfe_node_to_spec` | 1 | Convert a live node to a construction spec. |
| `ctfe_node_match` | 2 | Structural pattern match. |
| `ctfe_node_resolved_name_entry` | 2..3 | `caap.fact.resolved_name` entry. |
| `ctfe_node_resolved_block` | 2..3 | `caap.fact.resolved_block` fact. |
| `ctfe_node_call_semantics` | 2 | `caap.fact.call_semantics`. |
| `ctfe_call_semantics_from_entry` | 1 | Call semantics from a semantic entry. |

### Unit query & mutation (`ctfe_unit_*`)

Pure queries: `ctfe_unit_id`, `ctfe_unit_version`, `ctfe_unit_root`,
`ctfe_unit_top_level_forms`, `ctfe_unit_top_level_symbols`, `ctfe_unit_symbols`,
`ctfe_unit_exposed_names`, `ctfe_unit_dependency_bindings`, `ctfe_unit_facts`,
`ctfe_unit_node_location`, `ctfe_unit_node_span`, `ctfe_unit_rewrite_report`,
`ctfe_unit_syntax_metadata_get`.

> `ctfe_unit_node_location` (arity 2) returns just `(unit_id, node_id)` — node
> *identity*. `ctfe_unit_node_span` (arity 2) returns the node's optional source
> location — a map `{path, start, end, start_line, start_col, end_line, end_col}`
> when a span is attached (hand-written forms carry one), or `null` for span-less
> synthetic nodes (surface/macro/derive-generated). **Why:** so diagnostics can
> print `file:line:col` instead of an opaque node id. Presentational only —
> `node_id` stays the stable identity (principles #9/#10); never key caching or
> facts on these coordinates.

Mutations (impure / registry): `ctfe_unit_append_top_level!`,
`ctfe_unit_set_top_level_forms!`, `ctfe_unit_set_root!`, `ctfe_unit_set_id!`,
`ctfe_unit_declare_symbol!`, `ctfe_unit_set_symbol_semantics!`,
`ctfe_unit_add_exposed_name!`, `ctfe_unit_add_dependency_binding!`,
`ctfe_unit_erase_detached!`, `ctfe_unit_to_template`,
`ctfe_unit_template_instantiate`, and the syntax-rule family
`ctfe_unit_syntax_rule_define!`, `ctfe_unit_syntax_rule_define_inline_node!`,
`ctfe_unit_syntax_rule_set!`, `ctfe_unit_syntax_rule_params_set!`,
`ctfe_unit_syntax_metadata_set!`,
`ctfe_unit_syntax_hook_set_inline_node!`, `ctfe_unit_syntax_authoring_source_apply!`.

> Synthesis gotcha: a declared symbol's `SymbolEntry` should carry `node_id =
> None`; prefer `ctfe_provider_synthesize_internal_definition!`.

### Facts & annotations (`ctfe_meta_*`)

| builtin | arity | phase | summary |
|---|---|---|---|
| `ctfe_meta_fact_get_by_key` | 2..3 | comptime·pure | Read a fact by key. |
| `ctfe_meta_fact_has_by_key` | 2 | comptime·pure | Fact presence test. |
| `ctfe_meta_fact_set_by_key` | 3 | comptime·impure | Write a fact. |
| `ctfe_meta_fact_delete` | 2 | comptime·impure | Retract a fact from the current version onward (history stays for older-version queries; a later re-set revives it). Returns whether a fact was visible. **Why:** without retraction a pass could never take back a wrong fact — `set null` is still a present fact. |
| `ctfe_meta_annotation_get` | 2..3 | comptime·pure | Read a node annotation. |
| `ctfe_meta_annotation_set` | 3 | comptime·impure | Write a node annotation (shares the fact keyspace). |
| `ctfe_meta_annotation_delete` | 2 | comptime·impure | Retract an annotation (the delete twin of `_set`, same semantics as `ctfe_meta_fact_delete`). |

### IR construction

| builtin | arity | summary |
|---|---|---|
| `ctfe_ir_name` | 1..2 | Build a `Name` node spec: `(ctfe_ir_name (map_of "identifier" "x") [metadata])`. |
| `ctfe_ir_literal` | 1..2 | Build a `Literal` node spec: `(ctfe_ir_literal (map_of "value" 42) [metadata])`. |
| `ctfe_ir_call` | 1..2 | Build a `Call` node spec: `(ctfe_ir_call (map_of "callee" c "args" (list_of …)) [metadata])`. |
| `ctfe_spec_span` | 1 | Optional source location of a **detached** spec (mirror of `ctfe_unit_node_span`): the canonical span map, or `null` when absent — never fabricated. `ctfe_node_to_spec` preserves spans, so located diagnostics survive detachment. |
| `ctfe_spec_with_span` | 2 | A copy of a detached spec with its ROOT span set: from a span map (`{start, end, start_line, start_col, end_line, end_col[, path]}`), copied from a donor spec, or cleared with `null`. The located-diagnostics channel for synthesized (data-built) nodes; child spans untouched. |
| `ctfe_eval_node` | 1 | Evaluate a live node (or detached spec) to a value at compile time. **Why:** the primitive that lets a pass *fold* a sub-expression — the bridge from IR-as-data back to a value. |

---

## Provider context

`provider_context_runtime.rs` (20 `BuiltinInfo` + 4 diagnostics). The `ctfe_provider_*`
surface — operations a provider runs through its `ctx` (effect-checked against the
provider's declared capabilities). See [provider pipeline](mechanisms/provider-pass-pipeline.md).

| builtin | arity | phase | summary |
|---|---|---|---|
| `ctfe_provider_unit` | 1 | comptime·pure | The unit handle for this provider. |
| `ctfe_provider_traversal_walk` | 3..4 | comptime·eff | Walk nodes (optional `"kind"` filter), calling a visitor. |
| `ctfe_provider_node_replace` | 3 | comptime·write-ir | Replace a node with a spec. |
| `ctfe_provider_node_rewrite` | 4 | comptime·eff | Rewrite a node (richer than replace). |
| `ctfe_provider_node_erase` | 2 | comptime·write-ir | Erase a node. |
| `ctfe_provider_synthesize_internal_definition!` | 3 | comptime·eff | Add a top-level `(bind name value null)` + declare its symbol. |
| `ctfe_provider_fact_get` `ctfe_provider_fact_set` | 3–4 | comptime·pure/impure | Read / write a fact (effect-checked). |
| `ctfe_provider_annotation_get` `ctfe_provider_annotation_set` | 3–4 | comptime·pure/impure | Read / write a node annotation (effect-checked). |
| `ctfe_provider_evaluate_call!` | 3 | comptime·eff | Evaluate a call under a step/depth budget (the fold engine). |
| `ctfe_provider_fold_compile_time_call` | 2 | comptime·eff | Fold a compile-time call. |
| `ctfe_provider_invoke_callback` | 2+ | comptime·impure | Invoke a CAAP callback from a provider. |
| `ctfe_provider_require_effect` | 2 | comptime·pure | Assert a declared effect is present. |
| `ctfe_provider_base_resolution_scope` | 1 | comptime·pure | The base name-resolution scope. |
| `ctfe_resolution_scope_fork` | 1 | comptime·pure | Fork a child resolution scope. |
| `ctfe_resolution_scope_define!` | 2 | comptime·impure | Define a binding in a scope. |
| `ctfe_resolution_scope_lookup` | 2..3 | comptime·pure | Look up a name in a scope. |
| `ctfe_semantic_entry_node` | 2..3 | comptime·pure | The defining node of a semantic entry. |
| `ctfe_semantic_entry_to_map` | 1 | comptime·pure | Decode a semantic entry to a map. |
| `ctfe_provider_diagnostics_error` `ctfe_provider_diagnostics_warning` `ctfe_provider_diagnostics_note` `ctfe_provider_diagnostics_hint` | 3..6 | comptime·diag | Emit a diagnostic at error / warning / note / hint severity: `(ctx node message code [notes] [fixes])`. |

---

## Surface forms & syntax-form accessors

`surface.rs` (13) + `syntax.rs` (8). See
[surface grammar & lowering](mechanisms/surface-grammar-and-lowering.md).

### Surface-form CTFE (`ctfe_surface_*`, comptime·pure)

| builtin | arity | summary |
|---|---|---|
| `ctfe_surface_parse_form` | 2 | Parse text → surface form. |
| `ctfe_surface_reparse_text` | 2 | Re-parse a text region. |
| `ctfe_surface_unwrap` | 1 | Unwrap a surface form to its neutral value. |
| `ctfe_surface_form_symbol` `ctfe_surface_form_string` `ctfe_surface_form_integer` `ctfe_surface_form_float` `ctfe_surface_form_bool` `ctfe_surface_form_null` `ctfe_surface_form_list` | 1..3 | Construct surface forms (symbol / string / integer / float / boolean / null / list) — every literal kind a lower hook may need to emit. The list constructor's optional delimiter (`"paren"`/`"bracket"`/`"brace"`) defaults to null — not bracket-delimited. |
| `ctfe_surface_form_list_prepend` | 3..4 | Prepend into a surface list form. |
| `ctfe_surface_binding_get` | 2..3 | Read a binding form. |
| `ctfe_surface_binding_group_collect` | 1..2 | Collect a binding group. |
| `ctfe_source_ast_json` | 1 | Source text → span-carrying JSON AST (the tools/ast_json.caap surface; doubles as a parse check). |
| `ctfe_source_canonicalize` | 1 | Source text → canonical rendering (the tools/canonicalize.caap surface). |

### Syntax-form accessors (`syntax_*`, runtime)

| builtin | arity | summary |
|---|---|---|
| `syntax_kind` | 1 | Kind tag of a parsed form. |
| `syntax_name` `syntax_name_identifier` | 1..2 / 1 | Name form (optional trailing *origin* syntax value donates its span) / its identifier. |
| `syntax_literal` `syntax_literal_value` | 1..2 / 1 | Literal form (optional *origin* span donor) / its value. |
| `syntax_call` `syntax_call_callee` `syntax_call_args` | 2..3 / 1 | Call form — inherits its span from the first spanned child (callee, then args) unless an explicit *origin* overrides / callee / args. |

---

## Grammar / PEG

`grammar.rs` (11) + `grammar_builder.rs` (34) + `grammar/engine.rs` (23) + 1 in `compiler_query_runtime.rs`. All
`comptime·pure`. See
[surface grammar & lowering](mechanisms/surface-grammar-and-lowering.md) for the
combinator semantics.

### Grammar objects & parsing (`grammar.rs`)

| builtin | arity | summary |
|---|---|---|
| `ctfe_grammar_new` | 1 | Create a grammar. |
| `ctfe_grammar_extend` | 2 | Extend a grammar with rules. |
| `ctfe_grammar_set_start` | 2 | Set the start rule. |
| `ctfe_grammar_rule_get` | 2..3 | Get a rule. |
| `ctfe_grammar_describe` | 1 | Describe a grammar. |
| `ctfe_grammar_analyze` | 1 | Analyze a grammar. |
| `ctfe_grammar_conflicts` | 1 | Report rule conflicts. |
| `ctfe_grammar_parse` | 2..4 | Parse text with a grammar. |
| `ctfe_grammar_parse_tokens` | 3..5 | Parse a token stream. |
| `ctfe_lexer_tokenize` | 2 | Tokenize text. |
| `ctfe_lex_token` | 4 | Build/inspect a token. |

### PEG combinators & builder (`grammar_builder.rs`)

Core combinators: `ctfe_peg_lit`, `ctfe_peg_regex`, `ctfe_peg_char_class`,
`ctfe_peg_ref`, `ctfe_peg_imported_ref`, `ctfe_peg_seq`, `ctfe_peg_choice`,
`ctfe_peg_plus`, `ctfe_peg_action`, `ctfe_peg_call`, `ctfe_peg_island`,
`ctfe_peg_raw_block`, `ctfe_peg_token_ref`. (The dead, never-test-covered
`star`/`opt`/`and`/`not`/`predicate` were removed 2026-06-10; the textual rule
path expresses them as `e*`, `e?`, `&e`, `!e`.)

Zero-argument terminals: `ctfe_peg_dot` (any char), `ctfe_peg_cut` (commit
point — no backtracking past it), `ctfe_peg_newline`, `ctfe_peg_indent`,
`ctfe_peg_dedent` (layout terminals).

Word terminals: `ctfe_peg_keyword`, `ctfe_peg_soft_keyword` (word-boundary
literals; soft keywords stay usable as identifiers elsewhere).

Wrappers (expr → expr): `ctfe_peg_eager` (no backtracking inside),
`ctfe_peg_no_trivia` (disable trivia skipping inside).

Labelled (string + expr): `ctfe_peg_named` (label a sub-expression),
`ctfe_peg_capture` (named capture), `ctfe_peg_expected` (error-message
annotation), `ctfe_peg_grammar_scope` (resolve refs against a named grammar).

Separated repetition (element + separator): `ctfe_peg_sep_plus`,
`ctfe_peg_interspersed`.

Parametric rules: `ctfe_peg_param` (reference a rule parameter inside a
parametric rule body).

Builder: `ctfe_peg_builder`, `ctfe_peg_builder_rule`,
`ctfe_peg_builder_parametric_rule`, `ctfe_peg_builder_import`,
`ctfe_peg_builder_build`.

### Incremental parsing & grammar engine (`grammar/engine.rs`)

Mechanism over the `caap-peg` engine (classified `GrammarMechanism`). **Why this
exists:** the same engine that reads CAAP source is exposed to compile-time code,
so a stdlib pass or a tool can parse, re-parse incrementally, validate, and
introspect grammars *as data* — editor/LSP-grade capability without baking any
specific language's grammar into core. The reader directives (`extend_syntax`,
`define_grammar`, scoped grammars) and the LSP/DAP tooling sit on this surface.

**Parsing to a tree** — go beyond a yes/no match to a structured, inspectable AST:

| builtin | arity | summary |
|---|---|---|
| `ctfe_grammar_parse_forms` | 3..5 | **One-call surface parse**: `(compiler grammar_units text [start [path]])` — merges the units' grammars + inline lower hooks, parses the text, returns `{ok, forms}` (rich surface-form maps: `rule` = producing rule, honest `delimiter` — `paren`/`bracket`/`brace` or null, trivia-free `span`, `raw_text`) or `{ok:false, error}`; a text parse failure is data, setup problems are hard errors. The optional `path` becomes the spans' source path (default `<ctfe_grammar_parse_forms>`); pass a null `start` to set a path without overriding the rule. **Why:** language-building kits parse user programs in their own grammar without files or unit ceremony. |
| `ctfe_grammar_parse_ast` | 2..4 | Parse text → a structured AST (typed tree, not just a match). |
| `ctfe_grammar_parse_ast_tolerant` | 2..3 | Parse to a best-effort AST, tolerating errors (partial trees for tooling). |
| `ctfe_grammar_parse_recover` | 2..3 | Parse with error recovery — continue past failures and collect diagnostics. |
| `ctfe_grammar_parse_prefix` | 2..4 | Parse the longest valid prefix (completion / partial input). |
| `ctfe_grammar_parse_profiled` | 2..4 | Parse while collecting rule hit/timing stats — grammar perf tuning. |
| `ctfe_ast_to_map` | 1 | Decode an AST node to a plain map for inspection/transport. |

**Incremental re-parsing** — re-read only what changed (the editor/LSP path):

| builtin | arity | summary |
|---|---|---|
| `ctfe_grammar_parse_cache` | 0 | Obtain a reusable parse cache (memoizes packrat results across parses). |
| `ctfe_grammar_parse_incremental` | 3..5 | Parse reusing a prior cache + edits, re-parsing only affected spans. |
| `ctfe_grammar_apply_edits` | 2 | Apply text edits to a cached parse state. |
| `ctfe_ast_changed_ranges` | 3 | Diff two ASTs → the changed source ranges. |
| `ctfe_ast_reparse_incremental` | 3 | Re-parse against edited text, reusing unchanged subtrees. |

**Grammar registry** — name and reuse grammars across parses/scopes (the
substrate the scoped-grammar reader and imported-rule refs use):

| builtin | arity | summary |
|---|---|---|
| `ctfe_grammar_registry` | 0 | The grammar registry handle. |
| `ctfe_grammar_registry_register` | 3 | Register a named grammar. |
| `ctfe_grammar_registry_list` | 1..2 | List registered grammars (optional filter). |
| `ctfe_grammar_parse_with_registry` | 3..5 | Parse resolving imported-rule references against a registry. |

**Editing & analysis** — treat a grammar as mutable, introspectable data
(powers `extend_syntax`, validation, and determinism checks):

| builtin | arity | summary |
|---|---|---|
| `ctfe_grammar_replace_rule` | 3 | Replace a rule's definition (live grammar mutation — `extend_syntax`). |
| `ctfe_grammar_remove_rule` | 2 | Remove a rule. |
| `ctfe_grammar_set_metadata` | 3..4 | Attach metadata to a grammar / rule. |
| `ctfe_grammar_validate` | 1..2 | Validate a grammar (undefined refs, ill-formed rules). |
| `ctfe_grammar_diff` | 2 | Structural diff between two grammars. |
| `ctfe_grammar_signature` | 1 | Stable content signature of a grammar (cache key / determinism). |
| `ctfe_grammar_rule_graph` | 1 | The rule dependency graph. |
| `ctfe_grammar_nullable_rules` | 1 | Rules that can match empty (left-recursion / ambiguity analysis). |

---

*Coverage: all 338 public builtins are listed (machine-checked by `builtins_doc_bijection_tests.rs`). Phase tags are derived
mechanically from the registration metadata constructor; descriptions are
grounded in the registration site and handler. For exact argument semantics of an
individual primitive, read its `BuiltinInfo`/`eager` registration in the cited
`caap/src/builtins/*.rs` file.*
