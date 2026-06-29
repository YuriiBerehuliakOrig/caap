# Strings

Strings in CAAP are **immutable UTF-8 text**. String operations never modify
their argument; they return a new string. Everything here runs on the bare
kernel.

## Literals and Escapes

String literals use double quotes and JSON-style escapes:

```scheme
"hello, world"
"a tab\tand a newline\n"
"a quote \" and a backslash \\"
```

## Building and Slicing

`string_concat_many` joins any number of strings; `string_slice` takes a
substring by **character** index (`[start, end)`), and `get` reads a single
character:

```scheme
; runnable on the bare kernel
(string_concat_many "hello" ", " "world")  ; => hello, world
(string_slice "hello" 1 4)                  ; => ell
(get "hello" 0 "?")                         ; => h
(size "hello")                              ; => 5  (characters)
(string_byte_length "héllo")                ; => 6  (UTF-8 bytes)
```

Note the distinction: `size` counts **characters** (Unicode code points) while
`string_byte_length` counts **UTF-8 bytes**.

## Splitting and Joining

```scheme
; runnable on the bare kernel
(string_split "a,b,c" ",")        ; => [a, b, c]   (a list of strings)
(string_lines "one\ntwo")         ; => [one, two]
(string_chars "hi")               ; => [h, i]      (one string per character)
(sequence_join (list_of "a" "b" "c") "-")  ; => a-b-c
```

`string_split` is the inverse of `sequence_join`. `string_chars` is the
one-call way to iterate a string's characters.

## Searching and Testing

```scheme
; runnable on the bare kernel
(string_contains "hello" "ell")       ; => true
(string_starts_with "hello" "he")     ; => true
(string_ends_with "hello" "lo")       ; => true
(string_index_of "hello" "l")         ; => 2
(string_find "hello" "l" 3)           ; => 3   (search from index 3)
```

`string_find`/`string_index_of` return `-1` when the pattern is absent.

## Transforming

```scheme
; runnable on the bare kernel
(string_upcase "hi")                  ; => HI
(string_downcase "LOUD")              ; => loud
(string_trim "  spaced  ")            ; => spaced
(string_replace "a-b-c" "-" "+")      ; => a+b+c
(string_repeat "ab" 3)                ; => ababab
```

Width-padding helpers (`pad_left`/`pad_right`) live in the standard library's
text module rather than the kernel; you'll reach them once you're on the tower
(Chapter 7).

## Converting To and From Numbers

```scheme
; runnable on the bare kernel
(int_to_string 42)                ; => 42  (as a string)
(int_add (string_to_int "40") 2)  ; => 42  (parsed, then added)
```

`string_to_int` parses a base-10 integer and **errors on invalid input** — which
is a natural lead-in to the next chapter, error handling. A general value can be
rendered with `value_to_string` (used by the derive example in Chapter 10), and
`stable_hash` gives a deterministic integer hash of any value.

## A Small Example

A function that title-cases a single word, built only from the pieces above:

```scheme
; titlecase.caap — runnable on the bare kernel
(bind titlecase
  (lambda (w)
    (if (eq (size w) 0)
      w
      (string_concat_many
        (string_upcase (string_slice w 0 1))
        (string_downcase (string_slice w 1 (size w)))))))
(titlecase "cAAP")     ; => Caap
```

```bash
$ caap titlecase.caap
Caap
```
