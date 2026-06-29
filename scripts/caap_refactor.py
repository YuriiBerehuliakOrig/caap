#!/usr/bin/env python3
"""
AST-level refactoring tool for CAAP S-expression files.

Primary approach: span-based text edits.
  - Parse file → JSON AST (with character spans)
  - Walk AST to find matching nodes
  - Collect (start, end, new_text) edits
  - Apply edits end-to-start (preserves formatting of unchanged code)
  - Verify with the parse check (tools/ast_json.caap under tools/bare.caap)

Fallback: regenerate text from the AST yourself (loses formatting).

Usage:
    import sys; sys.path.insert(0, 'scripts')
    from caap_refactor import load_ast, load_source, apply_span_edits, check
    from caap_refactor import collect_span_edits, is_list, items, symbol_text, span_of

    source = load_source("bootstrap.caap")
    ast    = load_ast("bootstrap.caap")

    def matcher(node):
        lst = items(node)
        return is_list(node) and lst and symbol_text(lst[0]) == "register-helper" and len(lst) == 6

    def replacer(node, src):
        lst = items(node)
        effect = lst[4].get("String", {}).get("value")
        head = "register-pure-helper" if effect == "pure" else "register-impure-helper"
        a1, a3 = span_of(lst[1])["start"], span_of(lst[3])["end"]
        return f"({head} {src[a1:a3]})"

    edits  = collect_span_edits(ast, source, matcher, replacer)
    result = apply_span_edits(source, edits)
    open("bootstrap.caap", "w").write(result)
    assert check("bootstrap.caap")
"""

import json
import subprocess
import sys
from pathlib import Path

_ROOT = Path(__file__).parent.parent
CAAP = _ROOT / "target" / "debug" / "caap"
BARE_BOOTSTRAP = _ROOT / "tools" / "bare.caap"
AST_JSON_TOOL = _ROOT / "tools" / "ast_json.caap"


# ── caap CLI wrappers ─────────────────────────────────────────────────────────
# The CLI is a flagless launcher (`caap BOOTSTRAP PROGRAM [ARG…]`); the AST
# dump is the tools/ast_json.caap program run under the bare (no-stdlib)
# policy, which is also the parse check — it fails on malformed source.

def _caap_tool(tool: Path, *args, stdin=None):
    result = subprocess.run(
        [str(CAAP), str(BARE_BOOTSTRAP), str(tool)] + list(args),
        input=stdin,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise RuntimeError(f"caap {tool.name} {' '.join(args)} failed:\n{result.stderr}")
    return result.stdout


def load_ast(path: str) -> dict:
    """Parse a .caap file into a JSON AST dict (includes character spans)."""
    return json.loads(_caap_tool(AST_JSON_TOOL, path))


def load_source(path: str) -> str:
    return Path(path).read_text()


def check(path: str) -> bool:
    """Return True if the file parses (the AST dump doubles as the check)."""
    result = subprocess.run(
        [str(CAAP), str(BARE_BOOTSTRAP), str(AST_JSON_TOOL), path],
        capture_output=True,
    )
    return result.returncode == 0


# ── span-based editing (primary approach — preserves formatting) ───────────────

def span_of(node) -> dict | None:
    """Return the span dict {start, end, ...} of any AST node, or None."""
    for kind in ("List", "Symbol", "String", "Integer", "Float", "Boolean", "Null"):
        if kind in node:
            return node[kind].get("span")
    return None


def collect_span_edits(ast: dict, source: str, matcher_fn, new_text_fn) -> list:
    """
    Walk AST and collect (start, end, new_text) for every node where matcher_fn returns True.
    new_text_fn(node, source) -> str produces the replacement text.
    Returns edits sorted end-to-start so applying them doesn't shift earlier offsets.
    """
    edits = []

    def fn(node):
        if matcher_fn(node):
            sp = span_of(node)
            if sp is not None:
                edits.append((sp["start"], sp["end"], new_text_fn(node, source)))
        return node

    walk_forms(ast, fn)
    return sorted(edits, key=lambda e: e[0], reverse=True)


def apply_span_edits(source: str, edits: list) -> str:
    """Apply (start, end, new_text) edits to source, end-to-start."""
    result = source
    for start, end, new_text in edits:
        result = result[:start] + new_text + result[end:]
    return result


def transform_file(path: str, matcher_fn, new_text_fn, *, dry_run=False) -> int:
    """
    Load file, apply span edits, write back, verify with the parse check.
    Returns number of edits applied. Raises on check failure.
    """
    source = load_source(path)
    ast = load_ast(path)
    edits = collect_span_edits(ast, source, matcher_fn, new_text_fn)
    if not edits:
        return 0
    result = apply_span_edits(source, edits)
    if not dry_run:
        Path(path).write_text(result)
        if not check(path):
            Path(path).write_text(source)  # rollback
            raise RuntimeError(f"parse check failed after transform of {path}")
    return len(edits)


# ── node helpers ──────────────────────────────────────────────────────────────

def is_list(node):   return "List" in node
def is_symbol(node): return "Symbol" in node
def is_string(node): return "String" in node

def symbol_text(node):  return node["Symbol"]["text"] if is_symbol(node) else None
def string_value(node): return node["String"]["value"] if is_string(node) else None
def items(node):        return node["List"]["items"] if is_list(node) else []


# helpers for building new nodes (used in full AST round-trip, not span edits)
_DUMMY_SPAN = {"file_id": None, "start": 0, "end": 0, "path": None,
               "start_line": 1, "start_col": 1, "end_line": 1, "end_col": 1}

def mk_symbol(name: str) -> dict:
    return {"Symbol": {"text": name, "span": _DUMMY_SPAN}}

def mk_string(value: str) -> dict:
    return {"String": {"value": value, "raw": json.dumps(value), "span": _DUMMY_SPAN}}

def mk_bool(value: bool) -> dict:
    return {"Boolean": {"value": value, "span": _DUMMY_SPAN}}

def mk_null() -> dict:
    return {"Null": {"span": _DUMMY_SPAN}}

def mk_list(*nodes) -> dict:
    return {"List": {"items": list(nodes), "span": _DUMMY_SPAN}}


# ── tree walker ───────────────────────────────────────────────────────────────

def walk(node, fn):
    """Bottom-up tree walk: children first, then fn applied to each node."""
    if is_list(node):
        new_items = [walk(child, fn) for child in items(node)]
        node = {"List": {"items": new_items, "span": node["List"]["span"]}}
    return fn(node)


def walk_forms(ast: dict, fn) -> dict:
    return {"forms": [walk(form, fn) for form in ast["forms"]]}


# ── ready-made refactors ──────────────────────────────────────────────────────

def convert_register_helper(path: str) -> int:
    """
    Convert all 5-arg (register-helper root name impl "pure"/"impure" bool)
    to 3-arg (register-pure-helper root name impl) or (register-impure-helper ...).
    Also updates the preamble binding from register-helper to pure+impure variants.
    Returns total number of edits applied.
    """
    source = load_source(path)
    ast = load_ast(path)

    # collect call-site edits
    def call_matcher(node):
        lst = items(node)
        return (is_list(node) and lst
                and symbol_text(lst[0]) == "register-helper"
                and len(lst) == 6
                and is_string(lst[4]))

    def call_replacer(node, src):
        lst = items(node)
        effect = string_value(lst[4])
        head = "register-pure-helper" if effect == "pure" else "register-impure-helper"
        a1 = span_of(lst[1])["start"]
        a3 = span_of(lst[3])["end"]
        return f"({head} {src[a1:a3]})"

    edits = collect_span_edits(ast, source, call_matcher, call_replacer)

    # also collect preamble binding edits:
    # (register-helper (get kit-protocol "register-helper" null))
    # → (register-pure-helper (get kit-protocol "register-pure-helper" null))
    #   (register-impure-helper (get kit-protocol "register-impure-helper" null))
    # These are trickier because one binding becomes two — handle separately via
    # a simple string substitution on the binding name and the string literal.

    # For now handle via targeted find of the binding list node and span editing.
    # We look for bindings list items that are (register-helper (get ...)) pairs.
    preamble_edits = _collect_preamble_binding_edits(ast, source)
    all_edits = sorted(edits + preamble_edits, key=lambda e: e[0], reverse=True)

    if not all_edits:
        return 0
    result = apply_span_edits(source, all_edits)
    Path(path).write_text(result)
    if not check(path):
        Path(path).write_text(source)
        raise RuntimeError(f"parse check failed after convert_register_helper on {path}")
    return len(all_edits)


def _collect_preamble_binding_edits(ast: dict, source: str) -> list:
    """
    Find binding-list items of the form (register-helper (get kit-protocol "register-helper" null))
    and replace with two bindings: register-pure-helper + register-impure-helper.
    """
    edits = []

    def fn(node):
        lst = items(node)
        # Match the top-level bind form
        if not (is_list(node) and lst and symbol_text(lst[0]) == "bind"):
            return node
        bindings_node = lst[1]
        for b in items(bindings_node):
            b_lst = items(b)
            if not b_lst or symbol_text(b_lst[0]) != "register-helper":
                continue
            # b = (register-helper (get kit-protocol "register-helper" null))
            sp = span_of(b)
            if sp is None:
                continue
            new_text = (
                '(register-pure-helper (get kit-protocol "register-pure-helper" null))\n'
                '  (register-impure-helper (get kit-protocol "register-impure-helper" null))'
            )
            edits.append((sp["start"], sp["end"], new_text))
        return node

    walk_forms(ast, fn)
    return edits


# ── CLI entrypoint ────────────────────────────────────────────────────────────

if __name__ == "__main__":
    import argparse, glob

    parser = argparse.ArgumentParser(description="CAAP AST refactoring tool")
    sub = parser.add_subparsers(dest="cmd")

    p = sub.add_parser("check", help="Check syntax of a file")
    p.add_argument("files", nargs="+")

    p = sub.add_parser("convert-register-helper",
                       help="Replace 5-arg register-helper with pure/impure variants")
    p.add_argument("files", nargs="+")

    args = parser.parse_args()

    if args.cmd == "check":
        ok = all(check(f) for f in args.files)
        sys.exit(0 if ok else 1)

    elif args.cmd == "convert-register-helper":
        total = 0
        for pattern in args.files:
            for f in sorted(glob.glob(pattern, recursive=True)):
                n = convert_register_helper(f)
                if n:
                    print(f"  {f}: {n} edit(s)")
                    total += n
        print(f"Total: {total} edit(s)")

    else:
        parser.print_help()
        sys.exit(1)
