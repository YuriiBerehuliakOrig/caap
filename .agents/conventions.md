# CAAP Agent Conventions

This is the shared rulebook for every skill and agent under `.agents/`. Skills
link here instead of duplicating these rules.

## 1. Edit Existing `.caap` Files Through The Script

- Existing `.caap` files should be edited through
  [`scripts/caap_refactor.py`](../scripts/caap_refactor.py). It applies
  span-based edits, preserves untouched formatting, and verifies by running
  `tools/ast_json.caap` under `tools/bare.caap`.
- Raw write/edit is acceptable only for brand-new `.caap` files. Run
  `python3 scripts/caap_refactor.py check <file>` afterward.

See [`skills/caap-refactor.md`](skills/caap-refactor.md).

## 2. Ground Claims In Real Code

- Do not invent primitives, APIs, paths, or behavior.
- Verify kernel primitive names and semantics against
  [`KERNEL_REFERENCE.md`](../KERNEL_REFERENCE.md) and the actual registration
  code before relying on them.
- If uncertain, mark uncertainty explicitly instead of presenting a guess as
  fact.

## 3. Preserve Behavior By Default

- Default changes should preserve behavior: refactors, cleanup, documentation,
  and structure.
- Breaking changes must be explicit in the task or skill.
- Do not add silent fallbacks. Contract violations should be diagnostics or
  errors.

## 4. Prefer One Mechanism

- Use one mechanism instead of several parallel ones.
- New semantics should use callee policy, a stdlib kit/pass, or grammar
  extension rather than a new IR node or evaluator string check.
- Types, generics, pattern matching, and similar language features belong in
  stdlib policy unless they truly require kernel substrate.

## 5. Respect Compile-Time And Runtime Phases

- Always distinguish compile-time (CTFE) from runtime.
- Many primitives exist in only one phase.
- Compile-time and runtime are endpoints of one partial-evaluation model. When
  choosing a mechanism, consider whether values are static, dynamic, or mixed,
  and whether residual runtime code is expected.

See [`docs/design-partial-evaluation.md`](../docs/design-partial-evaluation.md).

## 6. Use Golden References For Refactors

- For behavior-preserving compiler or stdlib refactors, capture lowered output
  before and after with the relevant tool program, such as `tools/ast_json.caap`
  under `tools/bare.caap` or `tools/s2_emit.caap` under `stdlib/bootstrap.caap`.
- The golden output should match unless the task intentionally changes the
  contract.
- Intentional behavior changes need tests that pin the new behavior.

## 7. Keep Commits Atomic And Ask On Ambiguity

- One commit should represent one coherent change.
- Commit `.agents/` changes separately from code changes when possible.
- If a decision changes user-visible behavior and the task is ambiguous, ask
  instead of silently choosing.

## 8. Project Components

| Component | Purpose | Path |
| --- | --- | --- |
| `peg` | Standalone PEG parser engine. | [`peg/`](../peg/) |
| `caap` (`caap-core`) | Kernel: IR, evaluator, builtins, CTFE, host contracts. | [`caap/`](../caap/) |
| `caap-cli` | CLI launcher. | [`caap-cli/`](../caap-cli/) |
| `caap-dap` | DAP server for debugging. | [`caap-dap/`](../caap-dap/) |
| `caap-lsp` | LSP analysis, semantic tokens, diagnostics. | [`caap-lsp/`](../caap-lsp/) |
| `caap-sys-runtime` | System services across the FFI boundary. | [`caap-sys-runtime/`](../caap-sys-runtime/) |
| `stdlib` | CAAP policy layer: modules, kits, passes, grammar extensions, backends. | [`stdlib/`](../stdlib/) |

Core is substrate; stdlib is policy. See
[`docs/principles.md`](../docs/principles.md).

## 9. Definition Of Done

- `scripts/strict-gate.sh` should pass for completed work:
  `cargo fmt --check`, workspace tests, and clippy.
- Host or stdlib changes may also need `scripts/test-acceptance.sh`.
- See [`skills/build-and-test.md`](skills/build-and-test.md) and
  [`docs/testing.md`](../docs/testing.md).
