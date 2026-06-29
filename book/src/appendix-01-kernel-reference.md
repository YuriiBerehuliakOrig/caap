# Appendix A: Kernel Quick Reference

A condensed cheat sheet for the bare kernel. Everything here runs with
`caap file.caap`. For the exhaustive catalog (including the whole `ctfe_*`
compile-time surface), see `KERNEL_REFERENCE.md` in the repository.

## Atoms

| Kind | Examples |
|---|---|
| Integer (64-bit) | `0`, `42`, `-7` |
| Float (64-bit IEEE) | `3.5`, `1e10` |
| String | `"hi"`, `"a\nb"` |
| Boolean | `true`, `false` |
| Null | `null` |
| Symbol | `int_add`, `lo`, `sys.io` |

## Special Forms

| Form | Shape | Notes |
|---|---|---|
| `if` | `(if c then [else])` | one branch runs; no else → `null` |
| `and` / `or` | `(and a b …)` | short-circuit; **return a value** |
| `do` | `(do a b …)` | sequence; value is the last form |
| `bind` (flat) | `(bind name val rest…)` | defines into current scope, visible after |
| `bind` (paired) | `(bind ((n v)…) body…)` | fresh `letrec` scope; names vanish after |
| `lambda` | `(lambda (p…) body…)` | closure; captures its environment |
| `set!` | `(set! name expr)` | rebinds the nearest binding |
| `while` | `(while c body)` | loops while `c` is truthy |
| `block` / `leave` | `(block [lbl] body…)` / `(leave lbl [v])` | structured early exit |
| `macro` | `(macro (p…) body…)` | args quoted to syntax; returns syntax |
| `throw` / `try` | `(throw v)` / `(try body (catch e h))` | raise / catch |
| `effect_scope` | `(effect_scope effects body…)` | restrict authority to a subset |

**Truthiness:** everything is truthy except `false` and `null`.

## Reference Cells

| Builtin | Meaning |
|---|---|
| `(ref v)` | new mutable cell holding `v` |
| `(deref r)` | read the cell |
| `(set_ref r v)` | write the cell; returns `v` |

## Arithmetic and Comparison

- Integers (checked overflow/÷0): `int_add`, `int_sub`, `int_mul`, `int_div`,
  `int_rem`, `int_to_string`, `string_to_int`.
- Floats: `float_add`, `float_sub`, `float_mul`, `float_div`, `int_to_float`.
- Comparison: `lt`, `gt`, `le`, `ge`, `eq`; logic `not`.

## Collections

| Builtin | Meaning | Example → result |
|---|---|---|
| `list_of` | make a list | `(list_of 1 2 3)` → `[1, 2, 3]` |
| `map_of` / `assoc` | make/set a map | `(assoc (map_of) "a" 1)` → `{a: 1}` |
| `get` | index list/map/tuple/string | `(get xs 1 0)` |
| `get_strict` | like `get` but errors if absent | |
| `size` | length (chars for strings) | `(size "hi")` → `2` |
| `contains` | membership | |
| `append` / `set` / `list_remove_at` | mutate a list in place | |
| `map_keys` / `map_values` / `map_merge` / `map_delete` / `map_update` | map ops | |

## Sequence Toolkit (selection)

`sequence_range`, `sequence_map`, `sequence_filter`, `sequence_fold_left`,
`sequence_each`, `sequence_find`, `sequence_any`, `sequence_all`,
`sequence_sort_by`, `sequence_reverse`, `sequence_zip`, `sequence_join`,
`sequence_unique_by`, `sequence_group_by`, `sequence_take`, `sequence_drop`.

- Descending sort: `(sequence_reverse (sequence_sort_by xs key))`.
- Distinct: `(sequence_unique_by xs)`.

## Strings

`string_concat_many`, `string_slice`, `string_split`, `string_lines`,
`string_chars`, `string_find`, `string_index_of`, `string_replace`,
`string_repeat`, `string_trim`, `string_upcase`, `string_downcase`,
`string_starts_with`, `string_ends_with`, `string_contains`,
`string_byte_length`, `value_to_string`, `stable_hash`.
(Padding helpers `pad_left`/`pad_right` live in the tower's text module.)

## Reflection

`value_type` → one of `int`, `float`, `string`, `bool`, `null`, `list`, `map`,
`tuple`, `ref`, `callable`.

## Value Printing

When a program's final value is printed: strings show their text, lists as
`[a, b, c]`, maps as `{k: v}` (insertion order), and `null` shows nothing.
Launcher-mode tools may use an integer result as the process exit code.
