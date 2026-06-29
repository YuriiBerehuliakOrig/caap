# Incremental AST reuse & typed AST — status and remaining work

Two feature lines that are now **largely shipped**. This is the residual-work
note; the design rationale that's already realised has been pruned.

## Shipped

- **Typed extraction.** The `typed` runtime layer (`FromParseValue`,
  `ParseValue::{field,node,text,parse_as,parse_field}`, `FromParseValueError`
  with a breadcrumb path) plus the `#[derive(FromParseValue)]` proc-macro in the
  `caap-peg-derive` crate, behind the `derive` feature — including
  `#[peg(rename/text/default/tag)]` field/variant attributes.
- **Incremental diff.** `ast_diff::changed_ranges` (the byte ranges that actually
  changed across an edit) with **LCS child alignment** over shift-invariant
  subtree hashes, so a mid-list insert/delete is localised to the affected item.
- **Allocation sharing.** `AstNode.children` is `Arc<[AstNode]>`, and
  `ast_diff::reparse_ast_incremental` reconciles a freshly-parsed tree against the
  previous one, physically reusing (`Arc::ptr_eq`-observable) every subtree that
  is unchanged — decided by the same text- and shift-aware `subtree_equal` the
  diff uses, so a same-span leaf whose text changed is rebuilt, not shared.
- **Sound value reuse underneath.** `parse_incremental_many` + `PositionCache`
  already reuse `ParseValue` subtrees across edits, gated on the **examined**
  read-extent (`read_lo/read_hi`), not just the matched span.

## Remaining follow-ups (optimisation, not correctness)

1. **Share shifted-but-equal suffixes.** `reparse_ast_incremental` shares only
   subtrees that did *not* move, because `AstNode` spans are absolute: a
   suffix the parser reproduced identically after the edit has stale spans and is
   rebuilt. Switching to **relative spans** (offset from parent start) would let
   the reconcile pass adopt equal-but-shifted suffixes too. Invasive: touches
   every span consumer; do behind a typed span accessor first.

2. **O(changed) diff instead of O(n·m).** `align_children` runs an LCS DP over
   child hashes per node — fine for typical fan-out, quadratic on very large
   sibling lists. A persistent **subtree-hash side table** keyed by
   shift-invariant hash would make "did this subtree change?" O(1) and the diff
   O(changed). Pairs naturally with (1).

3. **Tie reuse to the examined extent, not just `==`.** `reparse_ast_incremental`
   currently re-decides equality structurally. If it instead consulted the
   position cache's examined interval per `(rule, pos)`, it could adopt unchanged
   subtrees without re-walking them — the same soundness datum `parse_incremental`
   already tracks. This is the bridge between the value-cache and the AST layer.

Each is independent and shippable on its own; none is required for current
correctness.
