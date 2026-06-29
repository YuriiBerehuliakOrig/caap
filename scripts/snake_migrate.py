#!/usr/bin/env python3
"""
Identifier-aware kebab-case -> snake_case migration for CAAP .caap files.

Raw-text lexer (does NOT depend on `caap ast-json`, which cannot parse float
literals). Lexes the source into comments / strings / atoms and rewrites only
identifier tokens:

  - Atoms (bare symbols): snake when the atom is an identifier/dotted-identifier
    path containing '-'. Numbers (`3.14`, `-1.0`), operators (`-`, `<=`, `==`,
    `+`), and `()` punctuation never match the identifier shape and are left
    untouched. `?`/`!` suffixes and `.` separators are preserved (only '-' is
    rewritten).
  - String contents: snaked only when the WHOLE content is such an identifier
    (module names, import/export/declare paths, effect tags, kit-protocol helper
    keys). Prose strings (spaces/punctuation) are left untouched.
  - Comments (`;` ... EOL): left untouched.

Idempotent and formatting-preserving. Confirmed safe: no kebab identifiers are
used as grammar terminals in the codebase, so string renaming cannot alter
grammar.

Usage:  python3 scripts/snake_migrate.py [--dry-run] <file-or-glob> ...
"""
import sys, re
from pathlib import Path

# Lowercase-leading only: CAAP identifiers are lowercase kebab. This deliberately
# excludes SCREAMING-KEBAB diagnostic codes (CAAP-PARSE-001) and SCREAMING
# constants, which are a separate vocabulary and stay as-is. An optional leading
# `&` is the rest-parameter sigil (`&entry-lists`); snake the identifier after it.
IDENT_RE = re.compile(r'^&?[a-z_][a-z0-9_]*([.\-][a-z0-9_]+)*[?!]?$')

def is_ident_with_dash(tok: str) -> bool:
    return '-' in tok and IDENT_RE.match(tok) is not None

def snake(tok: str) -> str:
    return tok.replace('-', '_')

# Delimiters that end an atom. Includes name-first surface punctuation
# (`,{}[]`) which is never part of a CAAP symbol, so identifiers adjacent to it
# (e.g. `pair-a,` in `compare(pair-a, b)`) tokenize correctly. `=`/`:`/`.` are
# valid symbol chars and are NOT delimiters.
ATOM_DELIMS = set(' \t\r\n()";,{}[]')

def migrate_text(src: str):
    out = []
    i, n = 0, len(src)
    edits = 0
    while i < n:
        c = src[i]
        if c == ';':                      # comment to EOL
            j = i
            while j < n and src[j] != '\n':
                j += 1
            out.append(src[i:j]); i = j
        elif c == '"':                    # string literal
            j = i + 1
            while j < n:
                if src[j] == '\\':
                    j += 2; continue
                if src[j] == '"':
                    break
                j += 1
            content = src[i+1:j]
            if is_ident_with_dash(content):
                out.append('"' + snake(content) + '"'); edits += 1
            else:
                out.append(src[i:j+1])
            i = j + 1 if j < n else j
        elif c in '() \t\r\n':            # punctuation / whitespace
            out.append(c); i += 1
        else:                              # atom
            j = i
            while j < n and src[j] not in ATOM_DELIMS:
                j += 1
            atom = src[i:j]
            if is_ident_with_dash(atom):
                out.append(snake(atom)); edits += 1
            else:
                out.append(atom)
            i = j
    return ''.join(out), edits

def migrate_file(path: str, dry_run=False) -> int:
    src = Path(path).read_text()
    result, edits = migrate_text(src)
    if edits and not dry_run:
        Path(path).write_text(result)
    return edits

if __name__ == "__main__":
    import glob
    args = sys.argv[1:]
    dry = "--dry-run" in args
    args = [a for a in args if a != "--dry-run"]
    files = []
    for pat in args:
        files.extend(sorted(glob.glob(pat, recursive=True)))
    tf = te = 0
    for f in files:
        n = migrate_file(f, dry_run=dry)
        if n:
            tf += 1; te += n
            print(f"  {f}: {n} edit(s)")
    print(f"{'[dry-run] ' if dry else ''}Total: {te} edit(s) across {tf} file(s)")
