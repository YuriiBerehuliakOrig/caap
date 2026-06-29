# Migration: stdlib (v1) → stdlib

Staged, reversible-until-the-last-step plan to retire the v1 stdlib in favour
of stdlib. **v1 is NOT deleted until Phase 4, which requires explicit
sign-off** — deleting it removes a working, tested 194-file subsystem plus its
compiler-kit/provider/partial-evaluation architecture and the test suites that
cover it.

Status: **Phase 1 ✅ · Phase 2 ✅ · Phase 3 ✅ · Phase 4 ✅ (v1 deleted).**
The `example/` directory has been removed entirely — see "Phase 4 — done" below.

---

## Phase 1 — stdlib-native toolchain ✅ (done)
The user-facing build path no longer needs v1:
- `stdlib/backend/driver.caap` carries the full clang plumbing (IR→object→link,
  caap-sys-runtime staticlib discovery + platform libs) — `compile_file`
  (source→binary) and `compile_ir` (specs→binary) both route through it.
- `tools/s2_build.caap` — a thin CLI build tool on a stdlib-ONLY composed
  bootstrap (no `stdlib/bootstrap.caap`). Pinned: native_loop (exit 45) and
  native_hello (println, links the staticlib, exit 42).

v1's `tools/build.caap` stays for now (the v1 test suites still drive it).

## Phase 2 — test classification ✅ (this document)
The kernel keeps a large INDEPENDENT regression suite — **30 `caap/tests/*.rs`
files reference v1 zero times** (tail_call, try_error, spec_span,
vocabulary_cache, grammar_parse_forms, dual_phase_equivalence, deep_recursion,
runtime_*, ctfe_*, host_*, numeric_semantics, reference_tests, …) plus
`stdlib_tests` (113). So deleting v1 does not leave the kernel bald.

The **17 v1-referencing test files** split into:

### Group A — v1-library / compiler-kit specific → DELETE with v1 (12 files)
They test exactly what is being removed (v1's library + its
provider/normalizer/partial-evaluation pipeline). No port; coverage of those
*v1 features* is intentionally dropped.
- `stdlib_module_tests` (117 v1 refs), `stdlib_library_tests` (68),
  `stdlib_module_source_tests` (22), `stdlib_compiler_kit_tests` (12),
  `stdlib_workflow_tests` (6), `stdlib_smoke_tests` (3),
  `stdlib_caap_tests` (2), `stdlib_dependency_graph` (1)
- `binding_time_tests`, `compile_time_evaluation_tests`,
  `const_propagation_tests`, `pe_annotation_tests` — these exercise v1's
  compiler-kit PE/provider machinery (`toolchain_binding_time.caap` etc.),
  an architecture stdlib deliberately does NOT replicate (expander + gate +
  const-fold instead). stdlib's own partial-evaluation story is ROADMAP A1.

### Group B — kernel-API tests using v1 only as a vehicle → RE-VEHICLE (5 files)
Their assertions are about the KERNEL (Compiler/session/bootstrap API, IR
builders, rewrite passes); v1 is just "a realistic bootstrap". Port = swap
`stdlib/bootstrap.caap` → `stdlib/bootstrap.caap` (and adjust any module
names referenced). Confirm each still asserts kernel behaviour after the swap;
drop any that turn out redundant with the 30 zero-v1 kernel suites.
- `compiler_session_tests` (asserts `compiler.units()`,
  `has_bootstrap_executions()` — pure kernel)
- `compiler_services_tests`, `bootstrap_session_tests`,
  `ctfe_ir_builder_tests`, `user_rewrite_pass_tests`

## Phase 3 — defaults flipped to stdlib ✅ (done)
- `.vscode/launch.json` and `vscode-caap/package.json` default to
  `stdlib/bootstrap.caap` (the composed-session trap is closed).
- LSP/DAP commands are stdlib-aware (`caap.session.commands` capability map;
  `analyze_source`/`run_source` registered by stdlib's bootstrap).
- `stdlib/README.md` carries a DEPRECATED banner pointing here.
- v1 stays BUILDING and TESTED in CI — it cannot be removed from CI until its
  tests are deleted/re-vehicled (Phase 4). Nothing in Phase 3 breaks v1.

## Phase 4 — done (v1 deleted)
Executed:
1. **Group A deleted (12 files)** + `user_rewrite_pass_tests` (it exercised v1's
   pass_kit `register_provider`/`reresolve`/`normalize_after_resolve` — none are
   kernel builtins, and stdlib uses expander+gate+`register_transform!`, not
   providers; kernel provider/resolution coverage stays in `ctfe_provider_tests`).
2. **Group B resolved** — not a blanket bootstrap-swap. `compiler_session_tests`
   / `compiler_services_tests` kept (kernel API); the v1-compiler-kit test fns
   inside `compiler_services`/`ctfe_unit_node_builtins`/`ctfe_provider` were
   surgically removed. `ctfe_ir_builder_tests` trimmed to its 9 kernel-only
   tests (raw `ctfe_ir_*`/`ctfe_eval_node`/`ctfe_spec_with_span` — the sole
   coverage of `ctfe_ir_call`/`ctfe_spec_with_span`/`ctfe_ir_detached`); its 2
   tests on v1's `stdlib.builder`/`surface_builder` DSL were dropped.
3. **`stdlib/` deleted (194 files)** + v1-only `tools/` (build,
   build_freestanding, emit_llvm, check — superseded by `tools/s2_build.caap`
   and the new `tools/s2_emit.caap`).
4. `scripts/test-acceptance.sh` carried no v1 refs (already clean).
5. **`cli_scenarios.rs`**: `composed_bootstrap` composes only its extras (no
   implicit v1 leg); v1-tool tests removed; the dead `bootstrap_path`/
   `run_binary` helpers and the `EXIT_RUNTIME` import dropped. `cli_tests.rs`
   re-pointed to `stdlib/bootstrap.caap` (the v1 `check.caap` test removed; the
   exit-code test runs a bare expr — the CLI launch path evals directly, so a
   `(module …)` file goes through compile/`run_source`, not bare launch).
6. **LSP `analyze_smoke.rs`** re-vehicled: 6 v1-bootstrap/demo tests removed
   (replaced by `stdlib_analyze_*` + the in-memory grammar-extended tests);
   `workspace_index` re-pointed at the stdlib corpus. `compile_bench.rs` now
   benches `s2_emit` over corpus natives on the composed stdlib bootstrap.
7. `cargo test --workspace` + clippy (`-D` clean) + fmt all green.

Gone for good: v1's compiler-kit (providers, binding-time, normalizer, PE
annotations, derive/contract/explain/visualize/workflow kits) and its tests.
The kernel and stdlib carry forward.

### `example/` demos — resolved (directory deleted)
The 15 v1-only demos built on v1's `pass_kit` / `builder` / `surface_builder` /
`module_kit` no longer loaded and were referenced by nothing in the gate, so
they were deleted: `builtin_fallback_lint_demo, c_like_http_demo,
generics_demo, guess_number_game, hsm_demo, interrupt_dsl_demo, memo_pass_demo,
name_first_expression_only_demo, oop_demo, oop_grammar_demo, oop_native_demo,
oop_vtable_demo, purity_pass_demo, sheet_demo, vtable_demo`.

The remaining kernel-level demos (`extend_syntax_demo`, `scoped_grammar_demo`,
`kernel_demo`) and their cli scenarios were subsequently removed as well — the
`example/` directory no longer exists. Demos now live under `examples/`.
