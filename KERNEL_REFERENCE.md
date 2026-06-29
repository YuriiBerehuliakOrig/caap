# CAAP Kernel Reference

All mechanisms provided by the Rust kernel that are available to the standard library and user code.
Builtins are listed with their arity: `n` = exactly n args, `n+` = at least n, `n–m` = between n and m.

The main tables describe the current kernel surface. The audit notes at the end separate remaining
cleanup candidates from resolved historical gaps.

---

## 0. Surface Grammar (Source Forms)

CAAP source is a sequence of **forms**. Every form is either an atom or a parenthesised list.

### Atoms

| Category | Regex / Literal | IR representation |
|----------|-----------------|-------------------|
| Integer | `-?(?:0\|[1-9][0-9]*)` | `LiteralNode(Int)` — 64-bit signed |
| Float | integer token with `.`, `e`, or `E` | `LiteralNode(Float)` — 64-bit IEEE 754 |
| String | `"(?:[^"\\]\|\\.)*"` — JSON-style escapes | `LiteralNode(Str)` |
| Boolean | `true` \| `false` | `LiteralNode(Bool)` |
| Null | `null` | `LiteralNode(Null)` |
| Symbol | `[A-Za-z_+\-*\/<>=!?$%&:.][A-Za-z0-9_+\-*\/<>=!?$%&:.]*` | `NameNode(identifier)` |

Trivia: whitespace, `;` line comments, `#| … |#` and `/* … */` block comments.

### Lists

A list `(head a b c …)` is the fundamental application form:

- Empty list `()` → `LiteralNode(Null)`
- If `head` is one of the **special head symbols** below, the form is desugared by the frontend before reaching the evaluator. All other lists become a `CallNode(head, [a, b, c, …])`.

### Special Head Symbols (frontend desugaring)

The frontend recognises these symbols as the first element of a list and lowers them
specially — they do **not** go through normal call lowering.

| Head | Desugaring |
|------|-----------|
| `lambda` | `(lambda (p1 p2 …) body…)` → `CallNode("lambda", [Tuple("p1","p2",…), body])` |
| `bind` | `(bind ((n v)…) body…)` → `CallNode("bind", [str:n, val, …, body])` |
| `block` | `(block [label] body…)` → `CallNode("block", [body…])`, registers label in scope |
| `leave` | `(leave label [val])` → `CallNode("leave", [LiteralNode(block_id), val?])` |
| `set!` | `(set! name expr)` → internal runtime mutation call |

`bind` also accepts the flat form `(bind name val body…)` when the second element is a
plain symbol rather than a list of pairs. The two forms scope differently: the
paired form evaluates in a fresh child scope with all names pre-declared
(letrec — names vanish after the body); the flat form defines into the
*current* scope sequentially, so the name stays visible to following sibling
forms (the define-like workhorse of `.caap` code).

Multiple body forms in `lambda` or `bind` are automatically wrapped in a `(do …)`.

### Top-Level Declaration Forms

The kernel parser represents module declarations as ordinary calls. The active
stdlib loader interprets the following name-based directives; the removed v1
`stdlib.module` string directives are historical.

| Form | Meaning |
|------|---------|
| `(module name)` | Declares the module identity. |
| `(import mod alias)` | Imports the full export map of `mod` under `alias`. |
| `(use mod a b …)` | Imports selected exported symbols directly into scope. |
| `(re_export mod a b …)` | Imports selected symbols and re-exports them. |
| `(export a b …)` | Marks selected local names as public exports. |

Directive arguments are **names**, not string literals. Strings remain ordinary
data.

### Reader Directives (segmental reading)

CAAP reads a file **one top-level form at a time**; these directives are
recognised at read time (by inspection, no evaluation) and mutate the *reader*
state instead of becoming program code — so they change how every form **after**
them is read. See `docs/segmental-reader.md` for the full contract.

| Form | Effect |
|------|--------|
| `(extend_syntax "rule" "peg source")` | Replaces `rule` in the live grammar for the remainder of the file (or scope). |
| `(define_grammar "name" "rule" "peg source")` | Registers/extends a named grammar (base clone + rule overrides) without activating it. |
| `(begin_scope "name")` | Pushes the active grammar and switches to the named grammar. |
| `(end_scope)` | Pops back to the grammar in effect before the matching `begin_scope`. Unbalanced scopes are a parse error. |

---

## 1. Special Forms (Language Core)

These are the fundamental syntactic constructs of the language. They receive unevaluated AST nodes
and control evaluation order themselves.

| Name | Arity | Semantics |
|------|-------|-----------|
| `if` | 2–3 | Conditional. Evaluates condition, then one branch. |
| `and` | 0+ | Short-circuit logical AND. Returns last truthy value or first falsy. |
| `or` | 0+ | Short-circuit logical OR. Returns first truthy value or last value. |
| `do` | 0+ | Sequential evaluation. Returns last value. |
| `lambda` | 2+ | Creates a closure. First arg is parameter list literal, rest is body. |
| `bind` | 2+ | Lexical binding (`let*` semantics). Alternating name/value pairs followed by body. |
| `block` | 1+ | Named scope for structured exits. |
| `leave` | 1–2 | Exits a `block` with optional value. |
| `while` | 2 | Loop: evaluates body while condition is truthy. |
| `macro` | 2+ | `(macro (p1 p2 …) body…)` — creates a macro value. Arguments at call sites are quoted into detached syntax values before binding; the body must return syntax, which the evaluator expands in the caller's environment. |
| `throw` / `try` | 1 / 1–2 | Raise a value; catch it — `try` also catches non-fatal evaluation errors (handler gets `{message, category}`); fatal budget errors pierce. |
| `effect_scope` | 1+ | `(effect_scope effect_list body…)` — evaluates body with the active effect set replaced by effect-list. Nested scopes may only request a subset of the parent; untrusted code loses privileges but cannot regain them. `(effect_scope (list_of) ...)` is pure-only execution. |

`set!` is source syntax. The frontend lowers it to the internal `assign_lexical`
builtin so the evaluator can update the nearest enclosing mutable scope. This is
the **canonical design**, not an interim state: `set!` is the one mutation
spelling in source; the lowering target is registered `.internal()` — not
directly callable (enforced by test), excluded from `builtin_names()` and the
compiler semantic catalog.

---

## 2. Arithmetic and Comparison

All `eager_runtime`. Integer operations are 64-bit signed; floats are 64-bit IEEE 754.

**Overflow & comparison contract (2026-06-10):** integer arithmetic is *checked* —
overflow and division by zero raise clean runtime errors (no silent wrap, no
build-profile-dependent panic; `int_rem MIN -1` is defined as `0`). Ordered
comparison (`lt`/`gt`/`le`/`ge`) on incomparable types raises a type error
instead of silently returning `false`; Int↔Float mixing and Str/Str ordering
remain valid, `eq`/`ne` across types remain plain inequality, and the total
order used internally by `sequence_sort_by` is unaffected.

### Integer Arithmetic

| Name | Arity | Description |
|------|-------|-------------|
| `int_add` | 2 | Addition |
| `int_sub` | 2 | Subtraction |
| `int_mul` | 2 | Multiplication |
| `int_div` | 2 | Truncating integer division |
| `int_rem` | 2 | Remainder (sign follows dividend) |
| `int_mod` | 2 | Modulo (sign follows divisor) |
| `int_abs` | 1 | Absolute value |
| `int_and` | 2 | Bitwise AND |
| `int_or` | 2 | Bitwise OR |
| `int_xor` | 2 | Bitwise XOR |
| `int_not` | 1 | Bitwise NOT |
| `int_shl` | 2 | Left shift; shift amount must be in `0..63` |
| `int_shr` | 2 | Arithmetic right shift; shift amount must be in `0..63` |

### Numeric Conversion

| Name | Arity | Description |
|------|-------|-------------|
| `int_to_float` | 1 | Int → Float |
| `float_to_int` | 1 | Float → Int (truncates toward zero) |

### Float Arithmetic And Math

All float primitives require `Float` arguments. `float_div` rejects division by zero.

| Name | Arity | Description |
|------|-------|-------------|
| `float_add` | 2 | Addition |
| `float_sub` | 2 | Subtraction |
| `float_mul` | 2 | Multiplication |
| `float_div` | 2 | Division |
| `float_abs` | 1 | Absolute value |
| `float_min` | 2 | Minimum of two floats |
| `float_to_bits` | 1 | Exact IEEE-754 f64 bit pattern as int |
| `float_to_bits_f32` | 1 | Value narrowed to f32, its bit pattern as int |
| `bits_to_float` | 1 | Inverse of `float_to_bits`: an int's 64 bits reinterpreted as f64 |
| `bits_to_float_f32` | 1 | Inverse of `float_to_bits_f32`: an int's low 32 bits reinterpreted as f32 |
| `float_max` | 2 | Maximum of two floats |
| `sqrt` | 1 | Square root |
| `sin` / `cos` / `tan` | 1 | Trigonometric functions |
| `asin` / `acos` / `atan` | 1 | Inverse trigonometric functions |
| `atan2` | 2 | Two-argument arctangent |
| `log` / `log2` / `log10` | 1 | Natural, base-2, and base-10 logarithm |
| `exp` | 1 | `e^x` |
| `pow` | 2 | Floating-point exponentiation |
| `floor` / `ceil` / `round` | 1 | Rounding helpers |
| `float_nan?` | 1 | Tests whether a float is NaN |
| `float_inf?` | 1 | Tests whether a float is infinite |

### Equality and Ordering

| Name | Arity | Description |
|------|-------|-------------|
| `eq` | 0+ | IDENTITY equality: scalars by value, but lists/maps/closures/refs by reference (two distinct maps with equal entries are NOT `eq`). All args must be equal. For structural equality use `value_eq`. |
| `value_eq` | 0+ | STRUCTURAL equality: lists/tuples element-wise in order, maps by key SET (order-insensitive), scalars/identities like `eq`. Cycle-safe (coinductive). The structural twin of `eq`. |
| `value_compare` | 2 | TOTAL structural order over ANY two values → `-1`/`0`/`+1`. Orders by stable type-rank (null < bool < int < float < string < bytes < tuple < list < map < callables/refs) then structurally (lists/tuples lexicographically, maps by sorted key/value pairs); no int/float coercion. Cycle-safe; `0` exactly when `value_eq`. The order twin of `value_eq`. |
| `value_hash` | 1 | STRUCTURAL hash of any value → integer. `value_eq`-equal values hash equal (maps fold order-independently); deterministic across runs for data kinds; cycle-safe. |
| `lt` | 2+ | Polymorphic binary less-than (int, float, string). |
| `gt` | 2+ | Polymorphic binary greater-than. |
| `le` | 2+ | Polymorphic binary less-than-or-equal. |
| `ge` | 2+ | Polymorphic binary greater-than-or-equal. |
| `not` | 1 | Logical negation. |

---

## 3. Mutable Collections

**Iteration order contract (2026-06-12):** maps iterate in INSERTION order
(IndexMap backing); `map_keys`/`map_values`/display follow construction order
deterministically, and `map_delete` preserves the remaining order.

### Construction

| Name | Arity | Description |
|------|-------|-------------|
| `list_of` | 0+ | Creates a mutable list from arguments. |
| `map_of` | 0+ | Creates a mutable map from alternating key/value arguments. |

### Mutation (in-place)

| Name | Arity | Description |
|------|-------|-------------|
| `append` | 2+ | Appends one or more elements to a list. Returns the list. |
| `assoc` | 3+ | Sets one or more `key value` pairs on a map. Returns the map. |
| `set` | 3 | Sets `seq[index] = value` (list). Returns the value. |
| `map_delete` | 2 | `(map_delete map key)` — removes key from map in place. Returns the map. |
| `list_remove_at` | 2 | `(list_remove_at list index)` — removes element at index in place. Returns the list. |

### References (first-class mutable cells)

`RuntimeValue::Ref` — a shared mutable cell. Lets code hold a reference to a
value and mutate through it (aliasing, mutate-in-place) instead of copying, and
gives the LLVM backend a value that lowers to a pointer. Equality between
references is **cell identity**, not contents. `value_type` reports `"ref"`.

| Name | Arity | Description |
|------|-------|-------------|
| `ref` | 1 | `(ref v)` — boxes `v` into a fresh shared cell; returns the reference. |
| `deref` | 1 | `(deref r)` — reads the value currently in the cell. Errors on a non-reference. |
| `set_ref` | 2 | `(set_ref r v)` — writes `v` into the cell (all aliases observe it). Returns `v`. |

---

## 4. Sequence and Map Access

All `eager_runtime` unless noted.

### Universal Access

| Name | Arity | Description |
|------|-------|-------------|
| `get` | 2–3 | `get(seq, key[, default])`. Works on list, map, tuple, string. |
| `get_strict` | 2 | Like `get` but errors if key is absent. |
| `size` | 1 | Length of list, tuple, map, string, or number of children on a live IR node. |
| `contains` | 2 | True if key/element exists in map, list, tuple, or string. |

### Map Operations

| Name | Arity | Description |
|------|-------|-------------|
| `map_keys` | 1 | Returns a list of all keys. |
| `map_values` | 1 | Returns a list of all values. |
| `map_merge` | 2 | Merges two maps (second wins on conflict). |
| `map_of_entries` | 1 | Builds a map from a list of `[key, value]` pairs. |
| `map_update` | 3 | `map_update(map, key, fn)` — sets `map[key] = fn(old_value)`. |

### Sequence Operations

| Name | Arity | Description |
|------|-------|-------------|
| `sequence_range` | 2 | `[start, end)` integer range as a list. |
| `sequence_each` | 2 | Calls `fn(element)` for each item; returns null. |
| `sequence_each_indexed` | 2 | Calls `fn(index, element)` for each item. |
| `sequence_each_pair` | 2 | Calls `fn(a, b)` for consecutive pairs. |
| `sequence_map` | 2 | Returns a new list with `fn(element)` applied. |
| `sequence_filter` | 2 | Returns elements for which `fn(element)` is truthy. |
| `sequence_fold_left` | 3 | `fold(seq, init, fn(acc, el))` — left reduction. |
| `sequence_find` | 2 | First element for which `fn(el)` is truthy, or null. |
| `sequence_find_reverse` | 2 | Same, searching from the end. |
| `sequence_any` | 2 | True if any element satisfies predicate. |
| `sequence_all` | 2 | True if all elements satisfy predicate. |
| `sequence_count` | 2 | Number of elements satisfying predicate. |
| `sequence_index_of` | 2 | Index of first matching element, or -1. |
| `sequence_reverse` | 1 | Returns reversed copy. |
| `sequence_slice` | 2–3 | `slice(seq, start[, end])`. |
| `sequence_take` | 2 | First N elements. |
| `sequence_drop` | 2 | All but first N elements. |
| `sequence_flatten` | 1 | Flattens one level of nesting. |
| `sequence_join` | 2 | Joins list of strings with a separator. |
| `sequence_sort_by` | 2 | Sorts by `key_fn(element)` ascending. For descending order use `(sequence_reverse (sequence_sort_by seq key_fn))`. |
| `sequence_group_by` | 2 | Groups elements into a map of lists keyed by `key_fn(element)`. |
| `sequence_zip` | 2 | Zips two sequences into a list of `[a, b]` pairs. |
| `sequence_unique_by` | 1–2 | Removes duplicates. With 1 arg: by identity (replaces the removed `sequence_distinct`). With 2 args: by key function. |

> **stdlib wrappers** (in `stdlib.lib.collections.sequence`): use
> `unique`, `reverse` + `sort_by`, and `each` + `range` instead of removed
> kernel conveniences such as `sequence_distinct`, `sequence_sort_by_desc`, and
> `for_range`.

---

## 5. Strings

All `eager_runtime`.

| Name | Arity | Description |
|------|-------|-------------|
| `string_concat_many` | 0+ | Concatenates all string arguments. |
| `string_slice` | 2–3 | `slice(s, start[, end])` by char index. |
| `string_split` | 2 | Splits string by delimiter. Returns list. |
| `string_find` | 2–3 | `find(s, pattern[, start])` → index or -1. |
| `string_index_of` | 2 | Index of first occurrence of substring. |
| `string_repeat` | 2 | `repeat(s, n)` — concatenates s with itself n times. |
| `string_trim` | 1 | Strips leading/trailing whitespace. |
| `string_starts_with` | 2 | Prefix check. |
| `string_ends_with` | 2 | Suffix check. |
| `string_contains` | 2 | Substring check. |
| `string_upcase` | 1 | Converts to uppercase. |
| `string_downcase` | 1 | Converts to lowercase. |
| `string_replace` | 3 | `replace(s, pattern, replacement)`. |
| `string_lines` | 1 | Splits on newlines. Returns list. |
| `string_chars` | 1 | Explodes a string into a list of 1-character strings (unicode codepoints, not bytes; empty → empty list). One-call O(n) iteration. |
| `string_to_int` | 1 | Parses base-10 integer string. Errors if invalid. |
| `int_to_string` | 1 | Converts integer to decimal string. |
| `string_byte_length` | 1 | Length in UTF-8 bytes. |
| `string_last_segment` | 2 | Last segment after splitting by delimiter. |
| `string_pad_left` | 2–3 | `pad_left(s, width[, fill])` — right-aligns with fill char (default space). |
| `string_pad_right` | 2–3 | `pad_right(s, width[, fill])` — left-aligns. |
| `stable_hash` | 1 | Deterministic hash of any value → integer. |
| `value_hash` | 1 | Structural hash → integer (see Comparison; `value_eq`-equal values hash equal). |

---

## 6. Reflection and Dispatch

| Name | Arity | Kind | Description |
|------|-------|------|-------------|
| `value_is_null` | 1 | eager_runtime | True if value is null. |
| `value_is_bool` | 1 | eager_runtime | |
| `value_is_int` | 1 | eager_runtime | |
| `value_is_float` | 1 | eager_runtime | |
| `value_is_string` | 1 | eager_runtime | |
| `value_is_list` | 1 | eager_runtime | |
| `value_is_tuple` | 1 | eager_runtime | |
| `value_is_map` | 1 | eager_runtime | |
| `value_is_callable` | 1 | eager_runtime | True for lambdas, builtins, host functions. |
| `value_is_error?` | 1 | eager_runtime | True if value is an error signal. |
| `value_type` | 1 | eager_runtime | Returns a canonical tag: `"null"`, `"bool"`, `"int"`, `"float"`, `"string"`, `"list"`, `"tuple"`, `"map"`, `"callable"`, `"macro"`, or `"object"`. |
| `host_value_kind` | 1 | eager_runtime | Returns a string tag for opaque host objects. |
| `apply` | 2+ | runtime_sequential | `apply(fn, args_list)` — spread call. |
| `gensym` | 0–1 | eager_runtime | Returns a unique symbol string, optionally prefixed. |
| `runtime_error` | 1 | special_form | Raises a runtime error with a message string. |

---

## 6a. Syntax Values (Runtime Macros)

These are the construction and inspection primitives for detached `ExprSpec` syntax values —
the argument type used by runtime `macro` forms. They work at `eager_runtime` phase and
produce opaque syntax objects that can be passed back as macro return values or built into
larger syntax trees.

### Construction

| Name | Arity | Description |
|------|-------|-------------|
| `syntax_name` | 1 | `(syntax_name "ident")` — creates a name syntax value. |
| `syntax_literal` | 1 | `(syntax_literal value)` — wraps a liftable runtime value as literal syntax. |
| `syntax_call` | 2 | `(syntax_call callee_syntax args_list)` — creates a call syntax from callee spec and a list/tuple of arg specs. |

### Inspection

| Name | Arity | Description |
|------|-------|-------------|
| `syntax_kind` | 1 | Returns `"name"`, `"literal"`, or `"call"`. |
| `syntax_name_identifier` | 1 | Returns the identifier string of a name syntax value. Errors if not a name. |
| `syntax_literal_value` | 1 | Returns the runtime value stored in a literal syntax value. Errors if not a literal. |
| `syntax_call_callee` | 1 | Returns the callee syntax value of a call. Errors if not a call. |
| `syntax_call_args` | 1 | Returns the argument syntax values of a call as a list. Errors if not a call. |

> Syntax values share the same `ExprSpec` representation as the `ctfe_ir_*` builder specs and
> are interchangeable at CTFE boundaries. At runtime they are opaque host objects.

---

## 7. stdlib Module Layer

These are compile-time directives interpreted by `stdlib.boot.loader`, not
kernel builtins. The kernel provides generic source parsing, unit loading,
registry, query, and evaluation mechanisms; module/import/export semantics live
in stdlib.

| Form | Description |
|------|-------------|
| `(module name)` | Declares the module identity of the current compilation unit. |
| `(import module alias)` | Binds the dependency export map under `alias`. |
| `(use module sym …)` | Binds selected dependency exports directly. |
| `(re_export module sym …)` | Binds and re-exports selected dependency exports. |
| `(export sym …)` | Marks local names as public exports. |
| `(surface kit [name])` | File-level surface protocol header; consumed by the stdlib loader/analyzer. |

Resolve policy is stdlib data: explicit `declare`, `declare_root`, and
recursive `discover`. Cycles are supported only when cross-cycle uses are
delayed into function bodies.

---

## 8. Compile-Time Evaluation (CTFE) — Compiler Registry

Available during bootstrap and pass registration. `compiler` is the implicit compiler bridge object.

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_compiler_register_value` | 3 | `(compiler name value)` — publishes a value into the global compiler registry under `name`. |
| `ctfe_compiler_lookup_value` | 3 | `(compiler name default)` — retrieves a previously registered value. |
| `ctfe_compiler_builtin_semantic_entries` | 1 | `(compiler)` — returns a tuple of semantic entry maps for every public kernel builtin. Used by stdlib bootstrap to register base semantic entries without hard-coding names. |
| `ctfe_compiler_emit_event` | 2+ | Emits a named event into the build event stream. |

---

## 9. CTFE — Compiler Provider & Stage Registration

Used by the stdlib bootstrap to wire up the compilation pipeline.

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_compiler_provider_register` | 7 | Registers a provider: `(compiler name stage impl requires effects spec)`. `impl` is invoked as `(impl ctx root)` by default, or `(impl ctx)` for single-argument callbacks. The callback does not receive implicit `compiler` or `unit` handles; provider code must use `ctfe-provider-*` mechanisms such as `ctfe_provider_unit` so effects and tracking remain explicit. |
| `ctfe_compiler_stage_register` | 2-7 | Defines a pipeline stage with dependencies, family, aliases, restart stage, and input kinds. |
| `ctfe_compiler_register_semantic_policy` | 3 | Registers a named semantic policy for call classification. |
| `ctfe_compiler_fact_schema_register` | 3 | Registers a named fact schema. |
| `ctfe_compiler_fact_schema_type_bridge_register` | 3 | Bridges a fact type to a host bridge type. |
| `ctfe_compiler_register_base_semantic_entries` | 2 | Registers explicit base semantic entry descriptors. Stdlib owns builtin grouping and phase/source policy. |

---

## 10. CTFE — Compiler Introspection and Query

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_compiler_list_stages` | 1 | Returns a list of registered pipeline stage names. |
| `ctfe_compiler_list_providers` | 1 | Returns the registered provider snapshot. Stage/target filtering is stdlib policy. |
| `ctfe_compiler_provider_schedule` | 2+ | Returns provider schedule groups for a stage/target. |
| `ctfe_compiler_list_semantic_policies` | 1 | Lists all registered semantic policies. |
| `ctfe_compiler_query_execution` | 3+ | Evaluates a query and returns plan steps, invalidations, optional artifact result, resulting unit, and execution-local diagnostics. |
| `ctfe_compiler_current_bootstrap_context` | 1 | Returns `{ path, capabilities }` for the active bootstrap execution. |
| `ctfe_kernel_vocabulary` | 0 | The kernel builtin vocabulary as data: name → `{ kind, params, result, min_arity, max_arity, pure, effects }` (param/result are type-name strings, `*t` = rest). Session-cached, invalidated by `register_builtin`; callers receive a detached copy. |
| `ctfe_debug_frames` | 0 | The live closure-call stack as `[{ name: str\|null, span: span-map\|null } …]`, outermost first (span = call site). STRICTLY diagnostics-class: marked impure (pure effect scopes cannot observe frame names) and never folds; a TCO loop is one collapsed frame. |

Graph, lineage, artifact, and unit query projections are policy layered on top
of `ctfe_compiler_query_execution`. In the current tree, stdlib tooling uses
direct command/capability surfaces such as `caap.session.commands` rather than
the removed v1 `stdlib.compiler_kit` helpers.

---

## 11. CTFE — Bootstrap File Execution

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_compiler_execute_bootstrap_file` | 2-3 | `(compiler path [capabilities])` — loads and evaluates one bootstrap file. |
| `ctfe_compiler_evaluate_bootstrap_file` | 2-5 | Evaluates a bootstrap file and returns captured value/diagnostics. Optional args: initial bindings, capabilities, skip-leading-forms. |
| `ctfe_compiler_evaluate_capture` | 3-5 | `(compiler unit phase [initial_bindings] [skip_leading_forms])` — evaluates a unit and captures value/diagnostics. |
| `ctfe_compiler_register_unit` | 3 | Registers a compiled unit handle under an explicit unit id. |
| `ctfe_compiler_lookup_unit` | 2-3 | Looks up a registered compiled unit by id, returning the optional default when absent. |
| `ctfe_compiler_list_dir` | 2 | Lists direct directory entries. Recursive traversal is stdlib policy. |
| `ctfe_compiler_is_file` | 2 | Returns true when a bootstrap-relative or absolute path resolves to a file. |
| `ctfe_compiler_load_surface_file_template` | 2-3 | Parses a source file into a cached template. Optional options map: `unit_id`, `syntax_units`, `hooks`. |

---

## 12. CTFE — Compilation Unit Access

Used inside provider callbacks to inspect and mutate the unit under compilation.

### Read

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_unit_id` | 1 | Returns the module identity string of the unit. |
| `ctfe_unit_root` | 1 | Returns the root IR node of the unit. |
| `ctfe_unit_version` | 1 | Returns the current version counter (increments on IR change). |
| `ctfe_unit_top_level_forms` | 1 | Returns a list of all top-level IR nodes. |
| `ctfe_unit_top_level_symbols` | 1 | Returns a list of all top-level binding entries. |
| `ctfe_unit_symbols` | 1 | Returns all symbols defined in the unit. |
| `ctfe_unit_dependency_bindings` | 1 | Returns explicit cross-unit dependency bindings. |
| `ctfe_unit_exposed_names` | 1 | Returns names exposed by the unit for cross-unit linking. |
| `ctfe_unit_node_location` | 2 | Returns the node's identity as `(unit_id, node_id)` — **not** its source position. |
| `ctfe_unit_node_span` | 2 | Returns the node's optional source location: a map `{path, start, end, start_line, start_col, end_line, end_col}` when a span is attached (hand-written forms), or `null` for span-less synthetic nodes. Presentational only — never key caches/facts on these coordinates; `node_id` is the stable identity. |
| `ctfe_unit_to_template` | 1 | Serializes the unit's IR to a template. |
| `ctfe_unit_template_instantiate` | 2 | Instantiates an IR template into a unit. |
| `ctfe_unit_facts` | 1 | Returns the fact table for the unit. |
| `ctfe_unit_rewrite_report` | 2 | Returns a structured rewrite provenance report for a unit node. |
| `ctfe_unit_syntax_metadata_get` | 2 | Gets surface syntax metadata for a key. |

### Write (mutate unit — require write-symbols effect)

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_unit_declare_symbol!` | 3 | Adds a new symbol declaration to the unit. |
| `ctfe_unit_set_symbol_semantics!` | 4 | Sets semantic metadata for a declared symbol. |
| `ctfe_unit_set_id!` | 2 | Sets the module identity of the unit. |
| `ctfe_unit_add_dependency_binding!` | 2 | Adds a cross-unit dependency binding to the unit. |
| `ctfe_unit_add_exposed_name!` | 2 | Marks a unit-local name as exposed to cross-unit linking. |
| `ctfe_unit_syntax_rule_set!` | 3 | Sets a surface syntax rewrite rule. |
| `ctfe_unit_syntax_metadata_set!` | 3 | Sets surface syntax metadata. |
| `ctfe_unit_syntax_hook_set_inline_node!` | 3 | Registers an inline syntax lowerer from a lambda node. |
| `ctfe_unit_syntax_authoring_source_apply!` | 3 | Applies a surface authoring source. |
| `ctfe_unit_syntax_rule_define!` | 3 | Defines a named syntax rule. |
| `ctfe_unit_syntax_rule_define_inline_node!` | 3 | Defines an inline syntax node rule. |

---

## 13. CTFE — IR Node Introspection

Low-level inspection of live IR nodes. All `compile_time_pure`.

### Node Identity and Structure

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_node_kind` | 1 | Returns `"Call"`, `"Name"`, or `"Literal"`. |
| `ctfe_node_is_call` | 1 | True if node/spec raw kind is Call. Language-level structural exclusions are stdlib policy. |
| `ctfe_node_is_name` | 1 | True if node/spec raw kind is Name. Language-level structural exclusions are stdlib policy. |
| `ctfe_node_is_literal` | 1 | True if node/spec raw kind is Literal. |
| `ctfe_node_live?` | 1 | True if node still exists in the IR graph. |
| `ctfe_node_id` | 1 | Returns the numeric node ID. |
| `ctfe_node_parent` | 1 | Returns the parent node, or null for root. |
| `ctfe_node_ancestor?` | 2 | `(node ancestor)` — true if ancestor is an ancestor of node. |
| `ctfe_node_children` | 1 | Returns a list of direct child nodes. |

### Call Node Inspection

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_node_call_callee` | 1 | Returns the callee node of a Call. |
| `ctfe_node_call_args` | 1 | Returns the argument nodes of a Call as a list. |
| `ctfe_node_call_semantics` | 1 | Returns the semantic classification map, or null when no semantic classification exists. Consumers read fields with generic map primitives. |

Language-level descriptors such as scope descriptors for `bind`/`lambda` and
control descriptors for `block`/`leave` are policy, not kernel API. Current
stdlib checks and analysis build on the generic node inspection primitives
above.

### Name and Literal Access

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_node_name_identifier` | 1 | Returns the identifier string of a Name node. |
| `ctfe_node_literal_value` | 1 | Returns the runtime value stored in a Literal node. |
| `ctfe_node_to_spec` | 1 | Captures a live node as a detached `ExprSpec` (composable with the `ctfe_ir_*` builders). |

### Pattern Matching

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_node_match` | 2 | `(node pattern)` — declarative structural match of a node against a pattern spec. Returns `{"matched" bool "bindings" map}`. Pattern wildcards bind sub-nodes by name; mismatches return `{"matched" false}`. Works on both live IR nodes and detached `ExprSpec` values. |

### Resolved Binding Inspection

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_node_resolved_name_entry` | 2 | Returns the semantic entry that a Name node resolves to. |
| `ctfe_node_resolved_block` | 2 | Returns the block descriptor for a `leave` target. |
| `ctfe_call_semantics_from_entry` | 1 | Builds a call-semantics value from a semantic entry. |

---

## 14. CTFE — IR Construction

Per-kind constructors — the node kind is the **builtin name**, so a misspelled
kind fails as an unknown builtin instead of a runtime "unsupported kind" error
(mirrors the per-severity `ctfe_provider_diagnostics_*` split). Each builds a
detached raw `ExprSpec`; language builders such as lambda/bind/if/do/block/leave
live in stdlib. The result is an opaque spec value usable with
`ctfe_provider_node_replace`.

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_ir_name` | 1-2 | `(payload [metadata])` — payload requires `identifier` (string). |
| `ctfe_ir_literal` | 1-2 | `(payload [metadata])` — payload requires `value` (any liftable runtime value). |
| `ctfe_ir_call` | 1-2 | `(payload [metadata])` — payload requires `callee` (spec) and optional `args` (list of specs). |
| `ctfe_spec_span` | 1 | Optional source location of a detached spec (span map or null); `ctfe_node_to_spec` preserves spans. |
| `ctfe_spec_with_span` | 2 | `(spec span_map\|donor_spec\|null)` — a COPY of `spec` with its ROOT span set from a span map (`{start, end, start_line, start_col, end_line, end_col[, path]}`), copied from a donor spec, or cleared with null. Child spans untouched. Gives data-built (synthesized) nodes a source location — located diagnostics survive rewriting/lowering. |
| `ctfe_eval_node` | 1 | `(spec_or_node)` — evaluates a constructed spec (or live node) to a value at compile time, in the current phase/environment. The metaprogramming closure: build IR, then run it — the bridge from IR-as-data back to a value. |

The optional `metadata` arg may contain `source_span` to preserve source location.

---

## 15. CTFE — Metadata: Annotations and Facts

Annotations are per-node key/value pairs stored alongside IR nodes.  
Facts are semantic values stored in the compilation unit's fact table (keyed by namespace + node).

### Annotations

Direct node access (live node handle; no provider context/effect gate):

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_meta_annotation_get` | 2–3 | `(node key [default])` — reads annotation or returns default/null. `SemanticValue::Node` values are projected back as live node handles. |
| `ctfe_meta_annotation_set` | 3 | `(node key value)` — sets annotation in-place. Returns `value`. |
| `ctfe_meta_annotation_delete` | 2 | `(node key)` — retracts an annotation (delete twin of `_set`; same versioned-tombstone semantics as `ctfe_meta_fact_delete`). |

★ Provider-context access (within a provider callback):

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_provider_annotation_get` | 3–4 | `(ctx node key [default])` — reads annotation via provider context. Requires `read_attributes`. |
| `ctfe_provider_annotation_set` | 4 | `(ctx node key value)` — sets annotation via provider context. Requires `write_attributes`. Returns `value`. |

### Facts

Direct live-node access:

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_meta_fact_get_by_key` | 2–3 | `(node fact_key [default])` — reads a fact by full schema key. `SemanticValue::Node` values are projected back as live node handles. |
| `ctfe_meta_fact_has_by_key` | 2 | `(node fact_key)` — true if fact exists. |
| `ctfe_meta_fact_set_by_key` | 3 | `(node fact_key value)` — sets a fact by schema key. |
| `ctfe_meta_fact_delete` | 2 | `(node fact_key)` — retracts the fact from the current version onward; history stays for older-version queries and a later re-set revives it. Returns whether a fact was visible. |

★ Canonical provider-context access:

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_provider_fact_get` | 3–4 | `(ctx fact_key node [default])` — effect-tracked fact read inside providers. Requires `read_facts`. |
| `ctfe_provider_fact_set` | 4 | `(ctx fact_key node value)` — effect-tracked fact write inside providers. Requires `write_facts`. |

---

## 16. CTFE — Provider Context

Available inside provider callbacks and compile-time functions.  
`ctx` is the opaque provider context object automatically passed as the first argument. Provider callbacks are context-first: use `(lambda (ctx root) ...)` for whole-unit passes and `(lambda (ctx) ...)` when the root node is not needed.

### Context Inspection

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_provider_require_effect` | 2 | `(ctx effect)` — asserts the provider declared the named effect; errors otherwise. |
| `ctfe_provider_unit` | 1 | Returns the unit bridge object for the current unit. |

### Name and Scope Resolution

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_provider_base_resolution_scope` | 1 | Returns the base semantic scope for the unit. |
| `ctfe_resolution_scope_fork` | 1 | Forks a resolution scope for local extension. |
| `ctfe_resolution_scope_lookup` | 2-3 | `(scope name [default])` — looks up a name in a forked scope. |
| `ctfe_resolution_scope_define!` | 2 | Defines a semantic entry descriptor in a forked scope. |
| `ctfe_semantic_entry_node` | 1 | Returns the IR node associated with a semantic entry. |
| `ctfe_semantic_entry_to_map` | 1 | Serializes a semantic entry into a plain map suitable for storing in semantic facts. |

### CTFE Folding

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_provider_fold_compile_time_call` | 3 | `(ctx node scope_descriptor)` — low-level primitive that executes/materializes a compile-time call in-place; callers own fold-safety policy and provide the stdlib-owned scope descriptor callback. |
| `ctfe_provider_invoke_callback` | 3+ | `(ctx fn args...)` — invokes a callback with provider context. |

### Traversal

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_provider_traversal_walk` | 3–4 | `(ctx root callback [options])`. Options: `order` (`"preorder"`/`"postorder"`), `mode` (`"walk"`/`"find_first"`/`"filter"`/`"stateful"`), `kind` (node kind filter), `initial_state`. |

### IR Mutation (require `write_ir` effect)

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_provider_node_replace` | 3 | `(ctx node spec)` — replaces a node's subtree with an ExprSpec. Returns the new node. |
| `ctfe_provider_node_rewrite` | 4 | `(ctx node pattern callback)` — declarative match+rewrite: matches node against pattern, calls `(callback bindings node)` with the match bindings if matched, expects callback to return an `ExprSpec`, and atomically replaces the node. Returns `{"matched" bool "rewritten" bool "node" node "bindings" map "replacement" node_or_null}`. Requires `read_ir` and `write_ir`. |
| `ctfe_provider_node_erase` | 2 | `(ctx node)` — removes a node from the graph. Returns null. |

### Diagnostics (require `emit_diagnostics` effect)

Four severity levels, each a distinct kernel builtin:

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_provider_diagnostics_error` | 3-6 | `(ctx node message [code] [notes] [fixes])` — emits a hard error and halts the current provider. |
| `ctfe_provider_diagnostics_warning` | 3-6 | `(ctx node message [code] [notes] [fixes])` — emits a warning diagnostic. |
| `ctfe_provider_diagnostics_note` | 3-6 | `(ctx node message [code] [notes] [fixes])` — emits an informational note. |
| `ctfe_provider_diagnostics_hint` | 3-6 | `(ctx node message [code] [notes] [fixes])` — emits a hint (lowest severity). |

stdlib pass modules may wrap these primitives, but the kernel API is the
`ctfe_provider_diagnostics_*` family above.

## 17. CTFE — Surface Syntax

Used by syntax extension and bootstrap code to interact with parsed surface forms before lowering to IR.

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_surface_binding_get` | 2 | `(surface name)` — gets a named binding from a surface binding group. |
| `ctfe_surface_binding_group_collect` | 2 | Collects a surface binding group as a list. |
| `ctfe_surface_unwrap` | 1 | Unwraps a surface form to its inner value. |
| `ctfe_surface_form_symbol` | 1 | Creates a surface symbol form. |
| `ctfe_surface_form_integer` | 1 | Creates a surface integer literal form. |
| `ctfe_surface_form_float` | 2 | Creates a surface float literal form. |
| `ctfe_surface_form_bool` | 2 | Creates a surface boolean literal form. |
| `ctfe_surface_form_null` | 0 | Creates a surface null form. |
| `ctfe_surface_form_list` | 1 | Creates a surface list form from a list of sub-forms. |
| `ctfe_surface_form_list_prepend` | 2 | Prepends a form to a surface list form. |
| `ctfe_surface_parse_form` | 2 | Parses a string into a surface form. |
| `ctfe_surface_reparse_text` | 2 | Reparses source text with a specific grammar rule. |

---

## 18. CTFE — Grammar Mechanisms

Low-level PEG grammar construction, inspection, analysis, and parsing primitives.
All are `compile_time_pure`. Grammar objects are opaque host values; construct them
with `ctfe_grammar_new` or `ctfe_grammar_extend`, inspect them with `ctfe_grammar_describe`
/ `ctfe_grammar_rule_get`, analyse them with `ctfe_grammar_analyze` / `ctfe_grammar_conflicts`,
and parse with `ctfe_grammar_parse` / `ctfe_grammar_parse_tokens`.

### Grammar Construction

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_grammar_new` | 1 | `(source_string)` — parses a PEG source string into an opaque grammar object. Errors if the grammar text is invalid. |
| `ctfe_grammar_set_start` | 2 | `(grammar name)` — returns a new grammar with the start rule set to `name`. Original grammar is unchanged. |
| `ctfe_grammar_extend` | 2 | `(grammar rules_list)` — returns a new grammar with added or replaced rules. `rules_list` is a list of `[name src]` pairs. Original grammar is unchanged. |

### Grammar Inspection

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_grammar_describe` | 1 | `(grammar)` — returns a data map: `{"start" str "rules" list "rule_count" int "version" int "sealed" bool "metadata" map …}`. Each rule entry includes `name`, `source`, `params`, and `imports`. |
| `ctfe_grammar_rule_get` | 2 | `(grammar name)` — returns the rule descriptor map for a single rule, or null if absent. Avoids fetching the full grammar description when only one rule is needed. |

### Grammar Analysis

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_grammar_analyze` | 1 | `(grammar)` — returns PEG static analysis as a structured map: reachability, rule references, duplicate/missing rules, parameter issues, nullable repetitions, left-recursion, shadowed alternatives, overlapping prefixes, warnings, and errors. |
| `ctfe_grammar_conflicts` | 1 | `(grammar)` — normalises the grammar analysis into a tooling-friendly list of `{"kind" str "severity" str …}` diagnostic maps. |

### Parsing

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_grammar_parse` | 2–4 | `(text grammar [options] [semantics])` — parses `text` with `grammar`. Returns `{"ok" bool "tree" … "errors" list}`. `options` map: `"memo"` bool, `"memo_policy"` map, `"max_steps"` int, `"return_spans"` bool. `semantics` map: `"predicates"` and `"actions"` each a map of rule-name → closure. Closures receive `{"value" tree "rule_stack" list "pos" int}`. |
| `ctfe_grammar_parse_tokens` | 3–5 | `(text grammar tokens [options] [semantics])` — like `ctfe_grammar_parse` but drives a `tok(...)` grammar with an explicit token list (output of `ctfe_lexer_tokenize` or `ctfe_lex_token`). |
| `ctfe_grammar_parse_forms` | 3–5 | `(compiler units text [start [path]])` — one-call surface parse: merges the grammar units' rules + inline lower hooks, parses `text`, returns lowered surface forms as data (`{"ok" true "forms" […]}` or `{"ok" false "error" str}` — a text parse failure is DATA, setup problems are errors). Optional `start` rule and real `path` for the returned form spans (default `<ctfe_grammar_parse_forms>`; pass null `start` to set a path without overriding the rule). |

### Lexer Support

| Name | Arity | Description |
|------|-------|-------------|
| `ctfe_lex_token` | 4 | `(kind text start end)` — builds one explicit token map `{"kind" str "text" str "start" int "end" int}` for hand-authored token streams. |
| `ctfe_lexer_tokenize` | 2 | `(text specs)` — tokenises `text` using ordered regex token specs. Each spec is a map `{"kind" str "pattern" regex ["skip" bool]}`. Uses maximal munch with spec-order tie-breaking; specs with `"skip" true` are omitted from the result. Returns a list of token maps. |

> Left-recursive grammars require memoization; disabling `"memo"` for those grammars is rejected
> before parse execution. For large inputs prefer token-stream parsing and set
> `"max_steps"` / `"memo_policy"` explicitly.

---

## 19. Host Services

These are registered by the host runtime (`src/host.rs`) and exposed through `sys.*` modules.
They require explicit `sys` or narrower `sys.*` module capabilities; the obsolete
`host_services` alias is rejected by the kernel capability normalizer.

### sys.io — Standard I/O

| Exported Name | Description |
|---------------|-------------|
| `print` | Writes a string to stdout (no newline). |
| `println` | Writes a value + newline to stdout. |
| `write` | Writes raw bytes to stdout. |
| `eprint` | Writes a string to stderr. |
| `eprintln` | Writes a value + newline to stderr. |
| `flush_stdout` | Flushes the stdout buffer. |
| `flush_stderr` | Flushes the stderr buffer. |
| `write_string` | Writes a string to stdout. |
| `write_line` | Writes a string + newline. |
| `read_line` | Reads one line from stdin. Returns string (includes newline). |
| `read_all` | Reads all of stdin to EOF. Returns string. |

### sys.fs — File System

| Exported Name | Description |
|---------------|-------------|
| `exists` | True if path exists. |
| `is_file` | True if path is a regular file. |
| `is_dir` | True if path is a directory. |
| `metadata` | Returns a map with file metadata (size, modified, etc.). |
| `file_metadata` | Like `metadata` but follows symlinks. |
| `list_dir` | Returns a list of entries in a directory. |
| `open_dir` | Opens a directory handle for iteration. |
| `close_dir` | Closes a directory handle. |
| `dir_list` | Returns next batch of entries from a directory handle. |
| `read_text` | Reads entire file to a string. |
| `file_read_all_text` | Alias for `read_text`. |
| `file_read_line` | Reads one line from an open file handle. |
| `write_text` | Writes a string to a file (creates or truncates). |
| `append_text` | Appends a string to a file. |
| `open_file` | Opens a file for reading/writing. Returns handle. |
| `close_file` | Closes an open file handle. |
| `file_write` | Writes bytes to an open file handle. |
| `file_seek` | Seeks to a position in an open file. |
| `file_flush` | Flushes an open file handle. |
| `create_dir` | Creates a directory. |
| `create_dir_all` | Creates a directory and all parents. |
| `remove_dir` | Removes an empty directory. |
| `remove_dir_all` | Removes a directory and all contents. |
| `rename` | Renames/moves a file or directory. |
| `copy_file` | Copies a file. |
| `remove_file` | Deletes a file. |
| `canonicalize` | Returns the canonical absolute path. |

### sys.os — Operating System

| Exported Name | Description |
|---------------|-------------|
| `env_get` | `(key)` — returns environment variable value or null. |
| `env_has` | `(key)` — true if variable exists. |
| `env_keys` | Returns list of all environment variable names. |
| `env_vars` | Returns a map of all environment variables. |
| `getcwd` | Returns current working directory as string. |
| `current_dir` | Alias for `getcwd`. |

### sys.process — Subprocess Management

| Exported Name | Description |
|---------------|-------------|
| `run` | Runs a command synchronously. Returns result map. |
| `spawn` | Spawns a child process. Returns handle. |
| `wait` | Waits for a child process. Returns result map. |
| `wait_result` | Like `wait` but returns exit code only. |
| `kill` | Kills a child process. |
| `write_stdin` | Writes to a child's stdin. |
| `close_stdin` | Closes a child's stdin pipe. |
| `read_stdout` | Reads from a child's stdout pipe. |
| `read_stderr` | Reads from a child's stderr pipe. |

### sys.net — Network

| Exported Name | Description |
|---------------|-------------|
| `listen` | Binds and listens on a TCP address. Returns server handle. |
| `accept` | Accepts an incoming connection. Returns stream handle. |
| `connect` | Connects to a TCP address. Returns stream handle. |
| `read` | Reads bytes from a network stream. |
| `write` | Writes bytes to a network stream. |
| `close` | Closes a network handle. |
| `poll` | Polls a set of handles for readiness. |

---

## 20. Host Service Registration (for stdlib authors)

Used by `sys.*` module implementations to bind host functions.

| Name | Arity | Description |
|------|-------|-------------|
| `host_service_export` | 2–3 | Projects a compile-time or runtime host function after bootstrap capability validation. |
| `host_service_capability` | 1 | Mints a typed host capability bridge after validating the current bootstrap grant. |
| `host_service_capability_export` | 3–4 | Exports a capability-gated host function. |
| `host_service_libraries` | 0–1 | Returns the list of native libraries required by host services. |
| `host_service_library_catalog` | 1–2 | Returns the library catalog for a capability. |

---

## Effect Tags Reference

Effect tags are declared when registering providers and determine which provider-context
operations are callable inside a provider callback. Direct `ctfe-meta-*` helpers are not gated by
provider effect tags; they require live node handles and run at CTFE.

| Tag | Grants access to |
|-----|-----------------|
| `"read_ir"` | Read-only IR node inspection (`ctfe-node-*`, `ctfe-unit-top-level-*`). |
| `"write_ir"` | IR mutation (`ctfe_provider_node_replace`, `ctfe_provider_node_erase`, `ctfe-unit-*` top-level mutation primitives). |
| `"read_facts"` | `ctfe_provider_fact_get`. |
| `"write_facts"` | `ctfe_provider_fact_set`. |
| `"read_attributes"` | `ctfe_provider_annotation_get`. |
| `"write_attributes"` | `ctfe_provider_annotation_set`. |
| `"read_symbols"` | `ctfe_unit_symbols`. |
| `"write_symbols"` | `ctfe_unit_declare_symbol!`, `ctfe_unit_set_symbol_semantics!`, etc. |
| `"emit_diagnostics"` | `ctfe_provider_diagnostics_error`, `ctfe_provider_diagnostics_warning`, `ctfe_provider_diagnostics_note`, `ctfe_provider_diagnostics_hint`. |
| `"use_host_services"` | Enables host-service builtins (I/O, FS, net, process) within a provider. |

---

## Pipeline Stages (built-in)

Historically, v1 registered named pipeline stages in
`stdlib/kits/compiler_kit/toolchain_foundation.caap`. That file is gone. The
active stdlib load path is:

| Step | Description |
|---|---|
| `read` | parse default CAAP forms, or dispatch `(surface kit)` files to a kit lowerer |
| `expand` | run compile-time forms from `stdlib/boot/forms.caap` |
| `check` | semantic unknown-name and arity checks |
| `typecheck` | stdlib type/effect inference and verified declarations |
| `eval` | evaluate the module body and publish exports |

Native compilation then uses `stdlib.backend.prep` / `stdlib.backend.emit.llvm` /
`stdlib.backend.driver`, not the v1 provider pipeline.

---

## Theoretical Assessment

### What is indisputably necessary

**Language core (§1):** `if`, `and`, `or`, `do`, `lambda`, `bind`, `block`/`leave` — irreducible.
`while` is a convenience that could be emulated with `block`/`leave` + recursion but earns its
place by enabling efficient imperative loops without stack overhead. `macro` makes user-defined
lazy forms a language mechanism instead of a Rust `SpecialForm` privilege. `effect_scope` is the
dynamic privilege-drop primitive — necessary for safe untrusted callback execution.

**Arithmetic (§2 integers):** The four basic ops, division, and `abs`/`min`/`max`/`clamp` are all
load-bearing. The closed bitwise set (`int_and`, `int_or`, `int_xor`, `int_not`, `int_shl`,
`int_shr`) is used by stdlib hashing and bit-manipulation routines.

**Mutable collections (§3):** Mandatory. Persistent/immutable collections cannot be implemented
purely in CAAP without the ability to build mutable aggregates at the kernel boundary.

**Universal access (§4, first table):** `get`, `get_strict`, `size`, `contains` — indispensable.

**Reflection (§6):** All `value-is-*` predicates are needed for dynamic dispatch. `apply`
is the call-with-computed-args primitive (the duplicate positional spelling
`invoke` was removed 2026-06-10). `gensym` is the only hygiene primitive and is critical for
macro expansion.

**Syntax values (§6a):** `syntax_name`, `syntax_literal`, `syntax_call` are load-bearing for
runtime macros. The inspection counterparts (`syntax_kind`, `syntax-*-identifier`,
`syntax_literal_value`, `syntax_call_callee`, `syntax_call_args`) are needed to write any
non-trivial macro body.

**CTFE registry (§8):** All entries are load-bearing for the entire plugin/pass architecture.
`ctfe_compiler_builtin_semantic_entries` is the bootstrap data primitive that allows
stdlib to register kernel policies without hard-coding builtin names.

**CTFE mutation (§16, IR Mutation):** `ctfe_provider_node_replace` is the single most important
pass primitive. `ctfe_provider_node_rewrite` is the declarative match+rewrite companion that
records rewrite provenance atomically — prefer it over manual match + replace for documented
transforms.

**Grammar mechanisms (§18):** `ctfe_grammar_parse` is the general-purpose entry point for
CTFE parsing. `ctfe_grammar_new`/`ctfe_grammar_extend` allow programmatic grammar construction.
`ctfe_grammar_describe`/`ctfe_grammar_analyze`/`ctfe_grammar_conflicts` enable data-driven
tooling. The lexer trio (`ctfe_lex_token`, `ctfe_lexer_tokenize`, `ctfe_grammar_parse_tokens`)
keeps scannerless and lexer-backed parsing orthogonal.

---

### Resolved Kernel Surface Cleanups

**`set!` lowering (§1):** ✅ **Closed by decision (2026-06-10).** The current design is
canonical: `set!` is the single source spelling for lexical mutation, lowered to the
internal, non-callable `assign_lexical`. No further cleanup is planned; see §1.

**`ctfe_compiler_register_python_language_builtin` (§9):** ✅ **Done.** Replaced by
`ctfe_compiler_register_base_semantic_entries`, which accepts explicit semantic entry descriptors
instead of core-owned bridge groups or builtin policy; deprecated aliases are not kept.

**`sequence_sort_by_desc` (§4):** ✅ **Done.** Removed from kernel; stdlib wraps
`(sequence_reverse (sequence_sort_by seq key_fn))` as `sort_desc`.

**`sequence_distinct` / `sequence_unique_by` (§4):** ✅ **Done.** `sequence_distinct` removed from
kernel; `sequence_unique_by` extended to 1–2 args (1-arg form = identity key, replacing
`sequence_distinct`). Stdlib `distinct` = `(sequence_unique_by seq)`.

**`for_range` (§4):** ✅ **Done.** Removed from kernel; stdlib wraps
`(sequence_each (sequence_range start end) fn)` as `for_range`.

**Polymorphic ordering (§2):** ✅ **Done.** The canonical binary ordering
primitives are `lt`/`gt`/`le`/`ge` (backed by the `value_compare` module);
stdlib composes them for variadic ordering helpers. (Earlier drafts called these
`value_lt`/`value_gt`/… — that prefix was dropped; `lt`/`gt`/`le`/`ge` are the
real builtin names.)

---

### Resolved Correctness Gaps

**Bitwise set incomplete (§2):** ✅ **Done.** The kernel now exposes the closed integer bitwise
set: `int_and`, `int_or`, `int_xor`, `int_not`, `int_shl`, and `int_shr`.

**Float arithmetic absent (§2):** ✅ **Done.** The kernel exposes float arithmetic, elementary
math, min/max, and NaN/Infinity predicates. Polymorphic `eq` and `lt` cover equality and
ordering.

**`le`/`ge` missing (§2):** ✅ **Done.** The kernel now exposes the
complete binary polymorphic ordering family: `lt`, `gt`, `le`, and `ge`.

**No collection removal (§3):** ✅ **Done.** `map_delete (map key)` and `list_remove_at (list index)`
added as in-place mutation primitives. stdlib exposes cleaner collection
facades in `stdlib.lib.collections.map` and `stdlib.lib.collections.sequence`.

**Diagnostics reduced to one level (§16):** ✅ **Done.** Four kernel builtins now exist:
`ctfe_provider_diagnostics_error` (error), `ctfe_provider_diagnostics_warning`,
`ctfe_provider_diagnostics_note`, `ctfe_provider_diagnostics_hint`.

### Extension Notes

**Annotation/fact duality (§15):** ✅ **Partially done.** Facts are fully migrated: all call
sites that do not need effect tracking (type checker, projection passes) now use `ctfe-meta-fact-*`
directly; `ctfe-provider-fact-get/set` survive only inside the pass-kit wrapper where incremental
tracking is required. Annotations still have a dual API — `ctfe-meta-annotation-*` (no context,
no effect gate) and `ctfe-provider-annotation-*` (ctx, tracks reads/writes for the scheduler).
Removing `ctfe-provider-annotation-*` requires verifying that no provider currently relies on
annotation tracking for correctness.

**Surface syntax extension (§17):** ✅ **Partially done.** (`ctfe_surface_match`
was later removed 2026-06-10 — dead, unused by any hook.) It used to cover the
common predicate case: validate a surface form and optionally filter by `kind` and list `head`.
Future destructuring helpers could still expose body/arms directly for richer procedural macros.

**Traversal options (§16):** ✅ **Done as mechanism.** `ctfe_provider_traversal_walk` provides the
core traversal mechanism with ordering, mode, kind filtering, and state threading. Higher-level
structural searches belong in stdlib pass helpers built on top of this
primitive.

**`value_type` reflection (§6):** ✅ **Done.** `value_type` returns canonical runtime tags for all
current `RuntimeValue` variants, with closures/builtins/host functions grouped as `"callable"` and
host objects as `"object"`.
