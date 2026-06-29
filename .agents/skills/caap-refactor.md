---
name: caap-refactor
description: Use for any programmatic edit to existing .caap files, especially stdlib bootstrap code. Applies span-based scripts/caap_refactor.py edits to preserve formatting and verify by parsing with tools/ast_json.caap. Raw writes are only for new .caap files.
---

# Skill: caap-refactor

Use this skill for any programmatic edit to existing `.caap` files, especially
bootstrap or stdlib files.

Shared rules:

- [`conventions.md`](../conventions.md) section 1: edit existing `.caap` files
  through the script.
- Section 3: preserve behavior by default.
- Section 6: use golden lowered output for behavior-preserving refactors.

## Golden Rule

Existing `.caap` files should be edited through
[`scripts/caap_refactor.py`](../../scripts/caap_refactor.py). The script applies
span-based edits, preserves untouched formatting, and verifies by parsing with
`tools/ast_json.caap` under `tools/bare.caap`.

Raw edit/write is only for brand-new `.caap` files. Run
`python3 scripts/caap_refactor.py check <file>` afterward.

Prerequisite: a debug `caap` binary should exist, usually from `cargo build`.

## Script Commands

Run:

```bash
python3 scripts/caap_refactor.py --help
```

Use built-in subcommands where possible, such as `check <file>` or migration
helpers. For custom edits, use the span API.

## Span-Based Mini Recipe

```python
import sys
sys.path.insert(0, "scripts")

from caap_refactor import (
    load_ast, load_source, collect_span_edits, apply_span_edits, check,
    is_list, items, symbol_text, span_of,
)

path = "stdlib/.../file.caap"
source = load_source(path)
ast = load_ast(path)

def matcher(node):
    lst = items(node)
    return is_list(node) and lst and symbol_text(lst[0]) == "register-helper"

def replacer(node, src):
    start = span_of(node)["start"]
    end = span_of(node)["end"]
    return src[start:end].replace("old", "new")

edits = collect_span_edits(ast, source, matcher, replacer)
result = apply_span_edits(source, edits)
open(path, "w").write(result)
assert check(path)
```

## Fallback

Full AST round-tripping loses formatting. Use it only when span edits are not
practical.

## Verification

The script should run its parse check. For behavior-preserving refactors,
compare lowered output before and after. For full gates, use
[`build-and-test`](build-and-test.md).
