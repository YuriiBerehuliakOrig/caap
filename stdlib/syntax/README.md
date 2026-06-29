# `syntax/`: AST As Data

This directory is the tier-2 domain for working with kernel AST values
(`ExprSpec`): reading nodes, building new nodes, rewriting trees, and rendering
them back to source text.

The modules are thin, pure layers over kernel syntax values. They do not keep
hidden state, except for the explicit `gensym` counter used for hygiene.

| Module | Purpose | Important operations |
| --- | --- | --- |
| [`ast`](ast.caap) | Node readers and builders. | `call?`, `name_of`, `head_of`, `args_of`, `bind_pairs`, `walk`, `sym`, `lit`, `calln`, `lam`, `eval_ir`, `span6`, `loc` |
| [`ir`](ir.caap) | Whole-tree operations and rewrites. | `transform`, `subst`, `subst_safe`, `match_node`, `rule`, `rewrite`, `rewrite_fix`, `free_names`, `gensym`, `rename_all` |
| [`render`](render.caap) | `ExprSpec` to kernel source text. | `render`, `render_program` |

Typical flow:

```text
read with ast -> rewrite with ir -> eval with ast.eval_ir or render with render
```

## `ast`

The raw kernel interface is intentionally small, but direct use is noisy.
`ast` gives names to common operations.

Readers are total: when a node has the wrong shape, they return `null` or an
empty list rather than throwing. `bind_pairs` and related helpers are the single
source of truth for canonical `bind` shape. `span6` and `loc` are the shared
span readers used by diagnostics.

Builders create syntax values from ordinary values:

- `sym`, `lit`, `call`, `calln`, `lam`, `seq`, `if3`;
- `eval_ir` evaluates a built tree into a value;
- `eval_with` evaluates a built tree with a sandbox map for free names.

## `ir`

`ir` is the shared pure substrate for compiler-like rewrites.

| Kind | Operations | Contract |
| --- | --- | --- |
| Traversal | `transform` | Bottom-up tree rebuild through a node function. |
| Substitution | `subst`, `subst_safe` | Inline `name -> spec`; safe mode avoids capture. |
| Matching | `match_node`, `rule` | Tree patterns with `?x` variables and `?xs..` segments. |
| Rewriting | `rewrite`, `rewrite_fix` | Rule lists, one pass or to fixed point. |
| Analysis | `free_names`, `names_used`, `names_set`, `node_eq` | Name analysis and structural equality. |
| Hygiene | `gensym`, `rename_all` | Fresh names and alpha-renaming. |

`transform` returns a fresh tree. It does not mutate the input. That makes the
same machinery safe in compile-time forms, load-time passes, and runtime
code-as-data programs.

Patterns are ordinary trees. A name beginning with `?` is a variable. A name
ending with `..` captures an argument slice. Nonlinear variables must match
structurally equal subtrees through `node_eq`.

## Rewrite Layers

`ir` is used by two higher layers but does not import them.

| Layer | File | Granularity | Runs |
| --- | --- | --- | --- |
| `define_form` | `boot/expander.caap` | one head symbol | during expansion |
| `lib.syntax.ir` | this directory | tree operations | inside forms and passes |
| `install_transform` | `semantics/passes/registry.caap` | whole module | after expansion, before gate checks |

Use `define_form` for syntax sugar tied to one head. Use whole-module transforms
for instrumentation, vocabulary lowering, and module-wide optimizations.

## Span Preservation

`ctfe_spec_with_span` can stamp a rebuilt node with a donor span. `transform`
and `subst` preserve the original root span for rebuilt calls, so diagnostics
remain pointed at user source after rewrites.

Boundaries:

- Leaf names and literals are returned as-is and keep their own spans.
- Fresh synthetic nodes usually have no source span.
- `node_eq` ignores spans, so fixed-point rewriting compares structure rather
  than metadata.

## `render`

`render` is the inverse of parsing for kernel-source values. Expanded trees
rendered here should parse back to the same kernel AST shape.

It is total for source-representable kernel values: names, calls, and primitive
literals. It throws for values that cannot appear as source literals, such as
maps, tuples, or callables embedded as literal values.

Rendering drops span metadata. Spans round-trip through parsing, not text.

## Tests

In-language tests live in:

- [`../lib/tests/test_ast.caap`](../lib/tests/test_ast.caap)
- [`../lib/tests/test_ir.caap`](../lib/tests/test_ir.caap)

The Rust loader harness scans `stdlib/lib/` recursively, so new `test_*.caap`
files normally require no Rust registration.
