# Name-First Expression-Only Surface Grammar

This document records a historical proposed CAAP surface style. It predates the
stdlib surface protocol; use [caap-spec.md](caap-spec.md) and
[mechanisms/surface-grammar-and-lowering.md](mechanisms/surface-grammar-and-lowering.md)
for the active surface contract.

## Core rules

Everything is an expression. There are no statements. A source file is a tree of
expressions, and a block is also an expression.

Every named thing is a binding, and every binding starts with its name:

```caap
name type = expression
```

The right side after `=` is always an expression. For named functions, the
function header is still name-first and does not use `fn`:

```caap
name (params...) returnType = {
  expression...
}
```

Types and modules are also bindings:

```caap
Name type = {
  fieldName fieldType
  fieldName fieldType = defaultValue
}

name module = {
  ...
}
```

Imports are bindings too:

```caap
alias import = "module.path"
```

## Blocks

A block is an expression:

```caap
{
  value i32 = 31
  add value 11
}
```

The value of the block is the value of its last expression. In the example above,
the block evaluates to the result of `add value 11`.

## Prefix expressions

Ordinary calls use prefix notation:

```caap
add 1 2
gt x 0
io.println classify value
```

Conditional expressions are still expressions:

```caap
if gt x 0 {
  "positive"
} else {
  "non_positive"
}
```

Because `if` is an expression, it can be the final expression in a block and
therefore become that block's return value.

## Desugaring

The named function form is surface sugar for a binding whose value is a function
expression. This surface form:

```caap
isPositive (x i32) bool = {
  gt x 0
}
```

is internally equivalent to:

```caap
isPositive = fn (x i32) bool {
  gt x 0
}
```

The important surface rule is that named functions do not start with `fn`.
`fn` is only the anonymous function expression used by the desugared form.

Similarly:

```caap
value i32 = 31
```

desugars to a typed binding of `value` to the expression `31`.

The old demo kept module imports as normal top-level v1 CAAP forms:

```caap
(import_symbols "sys.io" "println")
```

This is deliberate. Import declarations are module-level forms, so a grammar
hook for `app module = { ... }` must not hide them inside a generated expression
body.

The syntax hook uses `nfeo-*` constructors only as private lowering markers
inside the hook implementation. The final result is canonical CAAP that the
stdlib pipeline already understands: type declarations become
`caap.codegen.struct-type`, function entries become
`caap.codegen.root-signature`, and executable code becomes ordinary `bind`,
`lambda`, `do`, `if`, and call forms.

## Rule-Based Grammar Authoring

The syntax file describes the surface grammar through source-level rule forms,
not through a text blob:

```caap
(syntax_rule "nfeo_function"
  (transform
    "nfeo.function"
    (seq
      (named "name" (ref "nfeo_ident"))
      (literal "(")
      (named "params" (ref "nfeo_params"))
      (literal ")")
      (named "return_type" (ref "nfeo_ident"))
      (literal "=")
      (named "body" (ref "nfeo_block")))
    (lower
      (lambda (value span)
        ...))))
```

The rule expression vocabulary is intentionally small and structural:

- `(literal "...")`
- `(regex "...")`
- `(ref "rule_name")`
- `(seq ...)`
- `(choice ...)`
- `(optional expr)`, `(many expr)`, `(plus expr)`
- `(named "field" expr)`
- `(transform "hook.name" expr)`
- `(transform "hook.name" expr (lower (lambda (value span) ...)))`

Inline `lower` keeps the parser pattern and lowering policy in one place. The
lowering lambda is registered for the transform tag automatically, so public
grammar files do not need a separate `syntax_hook` declaration or standalone
`*-lower` function.

Replacing the base CAAP `form` rule is also explicit:

```caap
(syntax_replace_rule "form"
  (choice
    (ref "nfeo_module")
    (ref "nfeo_list")
    (ref "nfeo_string")
    (ref "nfeo_integer")
    (ref "nfeo_boolean")
    (ref "nfeo_null")
    (ref "nfeo_symbol")))
```

At the core IR level, this still fits CAAP's minimal mechanism: names, literals,
and calls. Module, import, type, function, and variable behavior remain surface
or stdlib policy layered over that mechanism rather than new kernel node kinds.
