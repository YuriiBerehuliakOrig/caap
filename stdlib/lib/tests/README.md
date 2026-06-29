# `stdlib/lib/tests/` — in-language test corpus

This directory holds the flat `test_*.caap` corpus of in-language tests written
in CAAP and exercised by the Rust loader (`caap/tests/stdlib_loader_tests.rs`, which
discovers every `test_*.caap` here). Each file is an ordinary CAAP program: loading it
runs its assertions, and a thrown `"FAIL: …"` (or any error) is the failure signal.
By default the loader treats every file identically — it loads and runs them all in one
corpus — but it also parses each file's metadata header to support subset selection (see
"Selecting a subset" below).

## Metadata header convention

Every `test_*.caap` carries a uniform **metadata comment header** placed immediately
after the existing top-of-file description comment and before the first `(use …)` /
executable form:

```
;; @area: <area>
;; @kind: <unit|regression|golden|acceptance>
;; @slow: <true|false>
```

These are **CAAP comments** (every line starts with `;`), so they have **zero effect on
executable behavior**. They are pure metadata: a machine- and human-readable label for
each test that the loader reads to drive subset selection (see "Selecting a subset"
below). `@area` is **required on every file** and enforced by the loader.

### `@area` — the subject under test

The library/subsystem the test primarily exercises (derived from the file's intent and
its non-`stdlib.lib.test` `(use …)` imports):

| area          | covers                                                                 |
|---------------|------------------------------------------------------------------------|
| `collections` | `lib/collections/*` (option, result, sequence, set, map, graph, …)     |
| `text`        | `lib/text/*` (string, json, toml, csv, regex, url, path, buffer, …)    |
| `numeric`     | `lib/numeric/*` + math/bits/float/random/algebra                       |
| `crypto`      | `lib/crypto/*` (digest, checksum, kdf, hash) + uuid                     |
| `types`       | `semantics/types/*` (generics, derive, let-generalization, …)          |
| `passes`      | `semantics/passes/*` + dataflow/ssa analysis & optimization passes     |
| `semantics`   | broader semantics substrate (load-time check, namespace, prelude)      |
| `sys`         | `sys/*` typed facades (path, wrap, verify)                             |
| `bare`        | `bare/*` native-only bare-metal wrappers (none in this corpus yet)      |
| `diag`        | `lib/diag/*` (error, diag bag, log) + diagnostics conversion           |
| `syntax`      | `syntax/*` (ast, ir, forms — the code-as-data substrate)               |
| `core`        | `lib/core/*` general utilities (equal, functional) + harness/cli/misc  |
| `backend`     | `backend/*` codegen (codegen_common, emit, …)                          |
| `storage`     | `storage/*` binary-format DSL                                          |
| `frontend`    | `frontend/*` surface languages (clike, …)                              |

The vocabulary mirrors the stdlib tier layout in `stdlib/CONVENTIONS.md`. When a test
spans several areas, the header names the **primary** subject under test.

### `@kind` — the test's character

- `unit` — focused API tests over a module's exports at their edges (the default).
- `regression` — larger behavior-preservation / round-trip suites that guard against
  miscompiles. The optimization and dataflow passes use the pattern "evaluate the IR
  before AND after a rewrite, assert the two results are equal, THEN assert the
  structural change" (e.g. `test_dead_store`, `test_licm`, `test_ssa*`, `test_ccp`,
  `test_cse`, `test_inline`, `test_pe`). A wrong rewrite is a miscompile, so these
  files exist to catch drift, not to document an API.
- `golden` — tests whose oracle is a stored golden artifact (rendered output / lowered
  IR compared byte-for-byte). None in this flat corpus yet; the golden gates live in
  the Rust split-by-scenario suites (`stdlib_{forms,types,codegen,…}_tests.rs`).
- `acceptance` — end-to-end / slow acceptance scenarios. None here; acceptance tests
  are the Rust tests marked `#[ignore = "acceptance: …"]` (see `docs/testing.md`).

When unsure, a test is `unit`.

### `@slow` — rough runtime cost

`true` for the heaviest optimization / dataflow suites (large files that build and
`eval_ir` many hand-written IR bodies through round-trips — `test_dead_store`,
`test_licm`, `test_ssa`, `test_ssa_ccp`, `test_ssa_dce`, `test_ccp`, `test_pe`).
`false` for everything else. This is a coarse hint for a future "fast subset" filter,
not a measured budget.

## How a future harness could group / filter

Because the metadata is a uniform leading comment block, a harness (Rust loader, a
CAAP test driver, or a shell script) can `grep` it without parsing CAAP. For example:

```bash
# all "passes" regression tests
grep -lE '^;; @area: passes'   stdlib/lib/tests/test_*.caap \
  | xargs grep -lE '^;; @kind: regression'

# the fast subset (skip the slow optimization suites)
grep -LE '^;; @slow: true'     stdlib/lib/tests/test_*.caap

# everything touching the type system
grep -lE '^;; @area: types'    stdlib/lib/tests/test_*.caap
```

The Rust harness reads the three header lines per file during discovery and exposes
them as an env-var filter (see "Selecting a subset" below). No file content beyond the
header lines needs parsing.

## Selecting a subset

The Rust loader (`stdlib_loader_tests.rs`, the `stdlib_run_all_in_language_tests`
acceptance test) parses each file's `@area` / `@kind` / `@slow` header in pure Rust
(no CAAP eval — it scans the leading comment block) and supports three env-var filters:

| env var          | selects files whose header has |
|------------------|--------------------------------|
| `CAAP_TEST_AREA` | `@area:` equal to the value    |
| `CAAP_TEST_KIND` | `@kind:` equal to the value    |
| `CAAP_TEST_SLOW` | `@slow:` equal to the value    |

Matching is **exact and case-insensitive**. When more than one is set they **AND**
together (a file must match all set filters). When **no** filter env var is set the
**full corpus** runs exactly as before — same files, same order. A filtered run logs
`selected N of M test files` to stderr (no silent truncation), and matching zero files
is an error (so a typo'd filter fails loudly instead of running nothing).

```bash
# the fast subset (skip the heavy optimization/dataflow suites)
CAAP_TEST_SLOW=false cargo nextest run -p caap-core \
  --test stdlib_loader_tests --run-ignored all

# only the type-system tests
CAAP_TEST_AREA=types cargo nextest run -p caap-core \
  --test stdlib_loader_tests --run-ignored all

# passes regression tests only (filters AND together)
CAAP_TEST_AREA=passes CAAP_TEST_KIND=regression cargo nextest run -p caap-core \
  --test stdlib_loader_tests --run-ignored all
```

## Header enforcement

The headers are not optional decoration: the loader **asserts every discovered
`test_*.caap` declares a `;; @area:` line**. A new test file without one fails the
acceptance test (naming the offending file), so the convention cannot silently rot.
`@kind` / `@slow` are documented and present on the whole corpus too, but only `@area`
is hard-required by the gate.

The metadata stays **inert at load time**: the lines are CAAP comments, so each test's
executable forms are unchanged — annotate freely, the corpus stays green.
