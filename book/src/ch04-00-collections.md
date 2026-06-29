# Collections and References

Most programs spend their time building up and tearing down collections. CAAP's
kernel gives you two collection types — **lists** and **maps** — plus
first-class **reference cells**, and a rich family of `sequence_*` operations
that work over any sequence. Everything in this chapter runs on the bare kernel.

## Lists

A list is an ordered, mutable sequence built with `list_of`:

```scheme
; runnable on the bare kernel
(bind xs (list_of 10 20 30))
xs                          ; => [10, 20, 30]
```

Read elements with `get` (with an optional default for out-of-range indices) and
ask for the length with `size`:

```scheme
(get xs 1 0)    ; => 20
(get xs 99 -1)  ; => -1   (default, since index 99 is absent)
(size xs)       ; => 3
```

Lists are *mutable*. `append` adds to the end, `set` overwrites an index, and
`list_remove_at` removes one — all in place, returning the list (or, for `set`,
the value):

```scheme
; runnable on the bare kernel
(bind ((xs (list_of 1 2)))
  (do
    (append xs 3 4)   ; xs is now [1, 2, 3, 4]
    (set xs 0 99)     ; xs is now [99, 2, 3, 4]
    xs))              ; => [99, 2, 3, 4]
```

## Maps

A map is a mutable key/value table that **keeps insertion order** — iterating a
map, or printing it, always reflects the order keys were added. Build one with
`map_of` and populate it with `assoc`:

```scheme
; runnable on the bare kernel
(bind m (assoc (map_of) "name" "ada" "age" 36))
m                         ; => {name: ada, age: 36}
(get m "name" "?")        ; => ada
(map_keys m)              ; => [name, age]
```

`assoc` sets one or more `key value` pairs and returns the map. Other staples:

| Builtin | Meaning |
|---------|---------|
| `map_keys` / `map_values` | lists of keys / values, in insertion order |
| `map_merge a b` | merge two maps (`b` wins on conflict) |
| `map_delete m k` | remove a key in place |
| `map_update m k fn` | replace `m[k]` with `fn(old)` |
| `map_of_entries pairs` | build a map from `[key, value]` pairs |

```scheme
(map_update (assoc (map_of) "n" 5) "n" (lambda (old) (int_add old 1)))
; => {n: 6}
```

`map_update` is the clean way to accumulate — increment a counter, push onto a
bucket — without a manual get/modify/assoc dance.

## Universal Access

`get`, `size`, and `contains` are *universal*: they work on lists, maps, tuples,
**and strings**.

```scheme
(get "hello" 0 "?")           ; => h     (a string indexes by character)
(size "hello")                ; => 5
(contains (list_of 1 2 3) 2)  ; => true
(contains "hello" "ell")      ; => true
```

If you'd rather an absent key be an error than a default, use `get_strict`.

## Tuples

Tuples are immutable, fixed-arity sequences. They arise naturally from the
language — a `lambda`'s parameter list is a tuple, and `sequence_zip` produces
`[a, b]` pairs — and you read them with `get` like any sequence. Reach for a
*list* when you need to grow or mutate, and let tuples be the small fixed
groupings the language hands you.

## Reference Cells

A `ref` is a shared mutable box (introduced in Chapter 3). Its superpower is
*aliasing*: hand the same cell to two places and both see each other's writes.

```scheme
; runnable on the bare kernel
(bind ((cell (ref 0)))
  (bind ((bump (lambda () (set_ref cell (int_add (deref cell) 1)))))
    (do (bump) (bump) (bump) (deref cell))))   ; => 3
```

`bump` captured `cell` and mutates it on each call. Because reference equality is
*cell identity*, two `(ref 0)` values are distinct boxes even though their
contents are equal. The native backend lowers a `ref` to a pointer, so this is
also the kernel's notion of "a thing you point at."

## The `sequence_*` Toolkit

Rather than write loops by hand, most list processing uses the higher-order
`sequence_*` builtins. A representative slice:

```scheme
; runnable on the bare kernel
(sequence_range 0 5)                                       ; [0, 1, 2, 3, 4]
(sequence_map (list_of 1 2 3) (lambda (x) (int_mul x x)))  ; [1, 4, 9]
(sequence_filter (sequence_range 0 10)
  (lambda (x) (eq (int_rem x 2) 0)))                       ; [0, 2, 4, 6, 8]
(sequence_fold_left (list_of 1 2 3 4) 0
  (lambda (acc x) (int_add acc x)))                        ; 10
(sequence_zip (list_of 1 2) (list_of "a" "b"))             ; [[1, a], [2, b]]
(sequence_join (list_of "a" "b" "c") "-")                  ; a-b-c
```

There are many more — `sequence_any`, `sequence_all`, `sequence_find`,
`sequence_take`, `sequence_drop`, `sequence_group_by`, `sequence_unique_by`,
`sequence_sort_by`, and so on. Two idioms worth memorising because the kernel
deliberately omits the "obvious" variants:

- **Descending sort:** `(sequence_reverse (sequence_sort_by xs key_fn))`.
- **Distinct:** `(sequence_unique_by xs)` (one argument) — there is no
  `sequence_distinct`.

```scheme
(sequence_reverse (sequence_sort_by (list_of 3 1 2) (lambda (x) x)))  ; [3, 2, 1]
```

The standard-library tower wraps these under shorter names — `map`, `filter`,
`fold`, `sort_by`, `range`, `each` in `stdlib.lib.collections.sequence` — which
you'll use from Chapter 7 on. The kernel names are what's underneath.
