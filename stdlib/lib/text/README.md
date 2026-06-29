# `lib/text`: Strings, Characters, Paths, And JSON

`stdlib/lib/text/` is the tier-2 text domain. The modules are normal stdlib
modules loaded through the loader and written with forms such as `cond` and
`for`. They wrap kernel `string_*` operations and add pure derived helpers.

Name resolution example:

```text
stdlib.lib.text.json -> lib/text/json.caap
```

| Module | Purpose | Main exports |
| --- | --- | --- |
| [`string.caap`](string.caap) | Clean facades over kernel string builtins plus derived helpers. | `split`, `trim`, `slice`, `find`, `contains?`, `replace`, `chars`, `char_at`, `pad_left`, `pad_right`, `parse_int`, `parse_float` |
| [`char.caap`](char.caap) | Character-class predicates over one-character strings. | `is_digit?`, `is_alpha?`, `is_alnum?`, `is_space?` |
| [`path.caap`](path.caap) | Pure lexical path manipulation, no filesystem access. | `absolute?`, `path_join`, `normalize`, `dirname`, `basename` |
| [`json.caap`](json.caap) | JSON parse/stringify/pretty in CAAP. | `json_parse`, `json_stringify`, `json_pretty` |

## Strings

Kernel builtins are first-class values, so many facades are direct renames with
zero wrapper lambdas. Derived helpers add behavior that the kernel intentionally
does not own:

- `char_at` and `chars` over `string_chars`;
- total `parse_int` and `parse_float` returning options;
- padding helpers;
- null-on-miss search helpers.

Indexes are character indexes. `find` returns `null` when there is no match.

## Characters

CAAP has no separate `char` type. A character is a one-character string.

The predicates test membership in fixed literal classes. They return `false`
for the empty string and multi-character strings.

## Paths

Path helpers are purely lexical. They do not touch the filesystem and do not
resolve symlinks.

`normalize` handles `.` and `..` lexically, preserves a leading `/`, and maps an
empty path to `"."`. These helpers are used by project manifests and loader path
logic.

## JSON

JSON maps directly to CAAP values:

- object -> map;
- array -> list;
- string/int/float/bool/null -> themselves.

Parse failures are data: `{ok:false, error}` with line/column text. Encode
failures are protocol errors under `stdlib.json.encode`, so callers can catch
them with `try`.

The parser walks a pre-split character list using `string_chars`, paying the
Unicode split cost once.

## Escapes

Supported in both directions:

```text
\" \\ \/ \b \f \n \r \t
```

`\uXXXX` is fully implemented:

- BMP code points decode to real UTF-8 strings.
- Surrogate pairs combine into one code point.
- Lone or mismatched surrogates produce located parse-error data.
- Control characters U+0000 through U+001F encode as short escapes or `\u00XX`.

The kernel does not currently expose a direct `codepoint -> string` primitive,
so JSON builds code points through byte construction and UTF-8 validation.

## Tests

In-language tests live under [`../tests/`](../tests/):

- `test_string.caap`
- `test_char.caap`
- `test_path.caap`
- `test_json.caap`

The loader harness scans `stdlib/lib/` recursively.
