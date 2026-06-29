# Stdlib Frontends

`frontend/` contains opt-in source surfaces. A frontend parses or lowers a
source language into stdlib/kernel forms; it does not own type policy or backend
emission.

Public users should import facade modules such as `stdlib.frontend.clike`.
Implementation leaves under a facade namespace are internal unless documented
otherwise.

Current frontends:

- `surface.caap`: grammar-combinator framework for custom lowering surfaces.
- `clike.caap`: public facade for the C-like/NMV surface.
- `clike/*`: internal leaves for AST/collection helper wiring, lexer grammar,
  token helpers, semantic tokens, type/expression/statement/declaration
  lowering, program framing, analysis, and per-file state.
