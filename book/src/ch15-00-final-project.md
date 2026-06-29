# A Final Project: A JSON Report

Let's finish by building a real program that exercises the tower: it parses a
JSON array of transactions, totals them per account, sorts by amount, and renders
a report — with parse failures handled as data, never as a crash. This is the
`json_report.caap` program from the corpus; we'll assemble it piece by piece.

## The Imports

```scheme
(use stdlib.lib.text.json json_parse)
(use stdlib.lib.collections.sequence fold sort_by map reverse join)
(use stdlib.lib.collections.map update! entries)
```

We pull in exactly the names we need from three library modules: the JSON parser,
the sequence toolkit (the friendly wrappers over the kernel `sequence_*`
builtins from Chapter 4), and two map helpers.

## Aggregating with `fold` and `update!`

The heart of the program turns a list of `{acct, amt}` records into a map from
account to summed amount:

```scheme
; account_totals — fold {acct, amt} records into acct -> summed amount.
(defn account_totals ((records list)) map
  (fold records (map_of)
    (lambda (acc rec)
      (update! acc (get rec "acct" "?")
        (lambda (old) (int_add old (get rec "amt" 0)))
        0))))
```

`fold` reduces the list into an accumulator that starts as an empty map. For each
record, `update!` adds the record's amount to that account's running total —
`update!`'s last argument, `0`, is the default for an account seen for the first
time. This is the `map_update` idiom from Chapter 4, under its tower name. Notice
the typed signature: `(records list) -> map`.

## Formatting a Row

```scheme
; format_line — one "<acct>: <total>" row.
(defn format_line ((pair list)) string
  (string_concat_many (get pair 0 "?") ": " (int_to_string (get pair 1 0))))
```

`entries` (used below) turns the totals map into a list of `[key, value]` pairs;
`format_line` renders one pair into a string with the string builtins from
Chapter 5.

## The Report, with Errors as Data

```scheme
; report — totals, highest first, one line each. A parse failure is reported
; as a single located line (the json error rides through as data).
(defn report ((source string)) string
  (bind ((parsed (json_parse source)))
    (if (get parsed "ok" false)
      (bind ((totals (account_totals (get parsed "value" (list_of))))
             (rows (reverse (sort_by (entries totals) (lambda (e) (get e 1 0))))))
        (join (map rows format_line) "\n"))
      (string_concat_many "report error: " (get parsed "error" "?")))))
```

This ties the whole book together:

- `json_parse` returns a **result map** (Chapter 6): `{ok, value}` on success,
  `{ok: false, error}` on failure. The `if` branches on `ok`, so a malformed
  input becomes a one-line `"report error: …"` string — the program never throws.
- On success we compute `totals`, take its `entries`, `sort_by` amount, and
  `reverse` for descending order — the exact "descending sort" idiom from
  Chapter 4 (`reverse` ∘ `sort_by`), here under the tower's short names.
- `map rows format_line` renders each row and `join` glues them with newlines.

## Running It

The file ends with a call on sample input — the program's value is the rendered
report:

```scheme
(report
  (string_concat_many
    "[{\"acct\":\"alice\",\"amt\":30},"
    " {\"acct\":\"bob\",\"amt\":12},"
    " {\"acct\":\"alice\",\"amt\":12}]"))
```

Alice's two transactions (30 + 12 = 42) outrank Bob's 12, so the report reads:

```text
alice: 42
bob: 12
```

Because the program uses tower modules (`use`, `defn`), it runs on the bootstrap
tower rather than the bare kernel — loaded as a module, or fed through the tools.
You can also compile a typed program like this toward the native backend
(Chapter 13) once its types are nailed down.

## What You've Learned

Look back at how much of the book this one program touches:

- **Kernel** (Chapters 3–6): `bind`, `lambda`, `if`, `get`, `int_add`,
  `string_concat_many`, result maps.
- **Collections** (Chapter 4): folding, sorting, the `reverse ∘ sort_by` idiom,
  `entries`.
- **The tower** (Chapter 7): `use`, the sequence and map libraries, the JSON
  parser.
- **Types** (Chapter 8): `defn` signatures that document and check each function.
- **Error handling** (Chapter 6): errors-as-data, so a bad input degrades
  gracefully.

From here, the natural next steps are to write your own modules (Chapter 7),
add a compile-time check or derive for your data (Chapters 10–11), give your
program a friendlier surface (Chapter 12), or compile a typed core to a native
binary (Chapter 13). You now have the whole language in view — kernel, tower,
compiler, and backends.

Welcome to CAAP. Go build something.
