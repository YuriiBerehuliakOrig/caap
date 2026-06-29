# `lib/collections`: Stdlib Collections

This directory contains pure tier-2 data helpers over kernel values: lists,
maps, sets, options, results, graphs, and sorted/priority structures.

Shared contract:

- Accessors are total where practical. They return `null`, defaults, `none`, or
  `err` values instead of throwing.
- Strict unwrap helpers are the named exceptions and may throw by contract.
- Derived values are fresh lists/maps; inputs are not mutated.
- Internal accumulators are fresh local state, so their mutation is not an
  external mutation effect.
- Public bindings should carry local doc comments.

## Modules

| Module | Purpose | Main operations |
| --- | --- | --- |
| [`sequence`](sequence.caap) | List/sequence helpers. | `map`, `filter`, `fold`, `take_while`, `partition`, `unique`, `zip`, `min_by` |
| [`map`](map.caap) | Map helpers. | `keys`, `values`, `merge`, `get_in`, `assoc_in`, `pick`, `map_vals`, `keys_where` |
| [`set`](set.caap) | Sets represented as maps from element string to `true`. | `set_union`, `set_intersection`, `set_difference`, `set_symmetric_difference`, `set_subset?`, `set_equal?` |
| [`option`](option.caap) | Optional value container. | `some`, `none`, `option_map`, `option_and_then`, `option_filter`, `option_to_result` |
| [`result`](result.caap) | Success/error value container. | `ok`, `err`, `map_ok`, `and_then`, `map_err`, `or_else`, `to_option` |
| [`graph`](graph.caap) | Graphs as adjacency maps plus DOT rendering. | `nodes`, `successors`, `has_edge?`, `add_edge`, `transpose`, `to_dot` |
| [`sorted`](sorted.caap) | Ordered and priority structures over lists. | `heap_push`, `heap_pop`, `heap_peek`, `enqueue`, `dequeue`, `push`, `pop`, `int_cmp` |

`sequence` and `map` are foundational. `set`, `graph`, and `sorted` build on
`sequence`. `result` depends on `option`; `option` does not depend on `result`.

## Option And Result

`option` models absence. `result` models success or failure.

The module dependency is one-way to avoid a cycle:

```lisp
(to_option (ok 5))            ; -> (some 5)
(to_option (err "e" "msg"))   ; -> (none)

(option_to_result (some 5) "empty" "no value")  ; -> (ok 5)
(option_to_result (none)   "empty" "no value")  ; -> (err "empty" "no value")
```

`option_to_result` builds the result shape inline, so `option` remains
dependency-free.

## Value Shapes

Containers are ordinary maps with fixed shapes. Code may inspect them without
importing the module when needed:

```text
(ok v)            -> { "ok": true,  "value": v }
(err code msg)    -> { "ok": false, "error": { "code": code, "message": msg } }
(some v)          -> { "some": true, "value": v }
(none)            -> { "some": false }
set               -> { <element>: true, ... }
graph             -> { <node>: [<successor>, ...], ... }
```

## Tests

In-language tests live under [`../tests/`](../tests/):

- `test_sequence.caap`
- `test_map.caap`
- `test_set.caap`
- `test_option.caap`
- `test_result.caap`
- `test_graph.caap`
- `test_sorted.caap`

The Rust loader harness scans them recursively; loading the test file runs its
assertions.
