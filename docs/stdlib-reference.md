# CAAP stdlib Reference

This page documents the active standard library: `stdlib/`. The removed v1
stdlib used `stdlib.*` namespaces, `register_module`, `pass_kit`,
`compiler_kit`, and string-based module directives; those names are historical
and must not be treated as the current API.

For the language-level contract, read [caap-spec.md](caap-spec.md). For the
architecture and tower-of-tiers overview, read
[../stdlib/README.md](../stdlib/README.md); for authoring rules, read
[../stdlib/CONVENTIONS.md](../stdlib/CONVENTIONS.md). This page is the flat
surface table; per-domain detail lives in the sub-tree READMEs:
[collections](../stdlib/lib/collections/README.md) ·
[text](../stdlib/lib/text/README.md) ·
[syntax](../stdlib/syntax/README.md) ·
[passes](../stdlib/semantics/passes/README.md) ·
[backend](../stdlib/backend/README.md) ·
[sys](../stdlib/sys/README.md) ·
[storage](../stdlib/storage/README.md).

## Bootstrap And Loader

| Module/file | Public surface | Purpose |
|---|---|---|
| [stdlib/bootstrap.caap](../stdlib/bootstrap.caap) | registers `stdlib.load`, `cli.main`, `caap.session.commands` | active bootstrap; loads forms, loader, type/effect passes, command capabilities |
| [boot/expander.caap](../stdlib/boot/expander.caap) | `expand`, `expand_with`, `expand_collect`, `expand_with_diagnostics`, `define_form`, `forms` | compile-time form engine |
| [boot/forms.caap](../stdlib/boot/forms.caap) | form registrations | `const`, `cond`, `when`, `unless`, `case`, `defn`, `struct`, `alias`, `enum`, `union`, threading forms |
| [boot/check.caap](../stdlib/boot/check.caap) | checker entrypoints used by loader | unknown-name and arity checks over expanded kernel AST |
| [boot/loader.caap](../stdlib/boot/loader.caap) | `load`, `load_module`, `declare`, `declare_root`, `discover`, `module_path`, `backfill_types` | module resolver and `read -> expand -> check -> typecheck -> eval` pipeline |
| [boot/run.caap](../stdlib/boot/run.caap) | `run_source`, `run_from_root`, `run_source_checked` | run files through stdlib loader |
| [boot/analyze.caap](../stdlib/boot/analyze.caap) | `analyze_source`, `analyze_source_with_root` | LSP-friendly definitions/diagnostics, including surface files |
| [boot/commands.caap](../stdlib/boot/commands.caap) | `commands`, `analyze_source`, `analyze_source_with_root`, `run_source`, `run_from_root`, `run_source_checked` | `caap.session.commands` capability map |
| [boot/sys_grants.caap](../stdlib/boot/sys_grants.caap) | capability setup | opt-in system-service handles |
| [boot/native_emit.caap](../stdlib/boot/native_emit.caap) | registers `stdlib.native.emit`, `stdlib.llvm.emit`, `stdlib.llvm.emit_freestanding` | lazily loads native/codegen modules |

## Module Directives

Directives take names, not strings:

| Directive | Contract |
|---|---|
| `(module name)` | module identity |
| `(import mod alias)` | bind dependency export map under `alias` |
| `(use mod a b)` | bind selected exports directly |
| `(re_export mod a b)` | bind and re-export selected names |
| `(export a b)` | explicit public surface; omitted export returns the last body value |

## Core Libraries

| Module | Source | Public exports |
|---|---|---|
| `stdlib.lib.core.prelude` | [lib/core/prelude.caap](../stdlib/lib/core/prelude.caap) | re-exports common sequence/map/string/option/result/equality/math/error helpers |
| `stdlib.lib.core.math` | [lib/core/math.caap](../stdlib/lib/core/math.caap) | `abs`, `sign`, `min`, `max`, `clamp`, `pow`, `gcd`, `lcm`, `factorial`, `even?`, `odd?` |
| `stdlib.lib.core.equal` | [lib/core/equal.caap](../stdlib/lib/core/equal.caap) | `deep_eq`, `deep_ne` |
| `stdlib.lib.core.functional` | [lib/core/functional.caap](../stdlib/lib/core/functional.caap) | `compose`, `compose_all`, `pipe`, `pipe_all`, `partial1`, `partial2`, `identity`, `constantly`, `flip`, `complement`, `memoize` |

## Collections

| Module | Source | Public exports |
|---|---|---|
| `stdlib.lib.collections.sequence` | [lib/collections/sequence.caap](../stdlib/lib/collections/sequence.caap) | `map`, `filter`, `fold`, `each`, `range`, `reverse`, `find`, `any?`, `all?`, `take`, `drop`, `count`, `join`, `zip`, `sort_by`, `length`, `empty?`, `first`, `last`, `sum`, `contains?`, `prepend`, `concat`, `pairs`, `map_indexed`, `flat_map`, `flatten`, `index_of`, `min_by`, `max_by`, `min`, `max`, `unique`, `partition`, `take_while`, `drop_while` |
| `stdlib.lib.collections.map` | [lib/collections/map.caap](../stdlib/lib/collections/map.caap) | `keys`, `values`, `merge`, `clone`, `delete!`, `update!`, `of_entries`, `has?`, `map_size`, `empty?`, `entries`, `pick`, `map_vals`, `get_in`, `assoc_in`, `keys_where` |
| `stdlib.lib.collections.option` | [lib/collections/option.caap](../stdlib/lib/collections/option.caap) | `some`, `none`, `option_of`, `some?`, `none?`, `option_unwrap`, `option_unwrap_or`, `option_map`, `option_map_or`, `option_filter`, `option_and_then`, `option_or`, `option_or_else`, `option_to_result` |
| `stdlib.lib.collections.result` | [lib/collections/result.caap](../stdlib/lib/collections/result.caap) | `ok`, `err`, `ok?`, `err?`, `error_of`, `error_code`, `error_message`, `unwrap`, `unwrap_err`, `unwrap_or`, `map_ok`, `map_or`, `and_then`, `map_err`, `or_else`, `result_or`, `to_option` |
| `stdlib.lib.collections.set` | [lib/collections/set.caap](../stdlib/lib/collections/set.caap) | `set_of`, `set_has?`, `set_size`, `set_empty?`, `set_items`, `set_add`, `set_remove`, `set_union`, `set_intersection`, `set_difference`, `set_symmetric_difference`, `set_filter`, `set_subset?`, `set_superset?`, `set_equal?`, `set_disjoint?` |
| `stdlib.lib.collections.graph` | [lib/collections/graph.caap](../stdlib/lib/collections/graph.caap) | graph helpers used by passes and tests |

## Text And Diagnostics

| Module | Source | Public exports |
|---|---|---|
| `stdlib.lib.text.string` | [lib/text/string.caap](../stdlib/lib/text/string.caap) | `split`, `trim`, `upcase`, `downcase`, `replace`, `repeat`, `lines`, `slice`, `find`, `contains?`, `starts_with?`, `ends_with?`, `concat`, `to_string`, `length`, `empty?`, `char_at`, `chars`, `pad_left`, `pad_right`, `parse_int`, `parse_float` |
| `stdlib.lib.text.char` | [lib/text/char.caap](../stdlib/lib/text/char.caap) | `is_digit?`, `is_alpha?`, `is_alnum?`, `is_space?` |
| `stdlib.lib.text.path` | [lib/text/path.caap](../stdlib/lib/text/path.caap) | lexical path helpers |
| `stdlib.lib.text.json` | [lib/text/json.caap](../stdlib/lib/text/json.caap) | `json_parse`, `json_stringify`, `json_pretty` |
| `stdlib.lib.diag.error` | [lib/diag/error.caap](../stdlib/lib/diag/error.caap) | `make_error`, `error?`, `error_code`, `error_message`, `error_data`, `raise!`, `as_error` |
| `stdlib.lib.diag.registry` | [lib/diag/registry.caap](../stdlib/lib/diag/registry.caap) | `register_code!`, `register_owned!`, `describe`, `codes` |

## Syntax And IR Helpers

| Module | Source | Public exports |
|---|---|---|
| `stdlib.syntax.ast` | [syntax/ast.caap](../stdlib/syntax/ast.caap) | `call?`, `name?`, `literal?`, `string_lit?`, `bool_lit?`, `name_of`, `literal_of`, `head_of`, `callee`, `head_is?`, `args_of`, `arg`, `items_of`, `bind_pairs`, `def_of`, `walk`, `span6`, `loc_or`, `loc`, `sym`, `lit`, `call`, `calln`, `lam`, `seq`, `if3`, `eval_ir`, `eval_with` |
| `stdlib.syntax.ir` | [syntax/ir.caap](../stdlib/syntax/ir.caap) | `transform`, `subst`, `subst_safe`, `replace_heads`, `node_eq`, `names_used`, `names_set`, `free_names`, `gensym`, `rename_all`, `pattern_var?`, `segment_var?`, `match_node`, `rule`, `rewrite`, `rewrite_fix`, `rewrite_traced` |
| `stdlib.syntax.render` | [syntax/render.caap](../stdlib/syntax/render.caap) | `render`, `render_program` |

## Passes

Passes are ordinary stdlib modules. `stdlib.semantics.passes.registry` is the shared
registration and fact channel. Full design + inventory:
[semantics/passes/README.md](../stdlib/semantics/passes/README.md).

| Module | Public exports |
|---|---|
| `stdlib.semantics.passes.registry` | `register_pass!`, `register_pass_with!`, `unregister_pass!`, `unregister_transform!`, `install_pass!`, `install_pass_with!`, `install_transform!`, `install_transform_with!`, `clear_passes!`, `run_passes`, `register_transform!`, `register_transform_with!`, `run_transforms`, `deps_of`, `no_deps`, `entry_of`, `ordered`, `loc_of`, `finding`, `finding_at`, `note!`, `ignored?`, `fact!`, `fact_of`, `facts_of`, `fact_schema!`, `schema_of`, `fact_typed!`, `fact_typed_of`, `fact_typed?` |
| `stdlib.semantics.passes.alias` | `check`, `check_module`, `class_map`, `register!` |
| `stdlib.semantics.passes.borrow` | `move`, `borrow`, `borrow_mut`, `check`, `check_module`, `register!` |
| `stdlib.semantics.passes.callgraph` | `analyze`, `check_module`, `register!` |
| `stdlib.semantics.passes.constfold` | `fold_node`, `run`, `register!` |
| `stdlib.semantics.passes.dce` | `droppable?`, `rewrite_node`, `run`, `register!` |
| `stdlib.semantics.passes.derive` | `register_derive!`, `derive_for`, `derived?`, `derives`, `reset_derives!` |
| `stdlib.semantics.passes.escape` | `escaping`, `check`, `check_module`, `register!` |
| `stdlib.semantics.passes.imports` | `check_forms`, `check_file` |
| `stdlib.semantics.passes.lint` | `check`, `check_module`, `register!`, `check_unused`, `check_shadow`, `check_unreachable`, `check_const_cond` |
| `stdlib.semantics.passes.match_check` | `check`, `check_module`, `register!` |
| `stdlib.semantics.passes.naming` | `bool_result?`, `check_module`, `register!` |
| `stdlib.semantics.passes.pe` | `static!`, `static_of`, `clear!`, `static_in!`, `clear_in!`, `static_marker`, `run`, `register!` |
| `stdlib.semantics.passes.peval` | `constprop`, `peval_node`, `run`, `register!` |
| `stdlib.semantics.passes.simplify` | `simplify_node`, `run`, `register!` |
| `stdlib.semantics.passes.tiers` | `rank_of`, `check_forms`, `check_file` |

## Types

| Module | Public exports |
|---|---|
| `stdlib.semantics.types.registry` | `define_struct!`, `define_alias!`, `define_enum!`, `enum_variants`, `enum?`, `define_type_fn!`, `elem_type`, `field`, `resolve`, `known_type?`, `field_type`, `struct?`, `sized_int?`, `int_family?`, `float_family?`, `bounds`, `literal_fits?`, `assignable?`, `pointer?`, `ptr_elem` |
| `stdlib.semantics.types.records` | `marker?`, `sig_marker`, `alias_marker`, `enum_marker`, `union_marker`, `struct_marker`, `ctor_record`, `fn_record?`, `lambda_param_names` |
| `stdlib.semantics.types.effects` | `vocab`, `known_tags`, `effect_state`, `effect_scan`, `inferred_tags`, `derive_effect`, `settle_effect!` |
| `stdlib.semantics.types.infer` | `result_types`, `param_types`, `check_module_types`, `register_sigs!`, `declare_sigs!`, `sig_of`, `sig_marker` |

## Codegen, Surface, Projects, Storage

See [backend/README.md](../stdlib/backend/README.md) for how `prep` (the shared
front-end) feeds the `llvm`/`wasm` backends, with `driver` as the toolchain build
wrapper.

| Module | Public exports | Purpose |
|---|---|---|
| `stdlib.lib.project` | `load_project`, `load_entry`, `run`, `projects`, `clear_projects!` | project manifests with roots/deps/entry |
| `stdlib.backend.native_meta` | `native_heads`, `native_types`, `head_names`, `type_names`, `backend_supports?`, `wasm_gap_of`, `native_type?` | the single declarative source of the native head/type vocabulary (consumed by `prep` + both emitters) |
| `stdlib.backend.prep` | `prep_program`, `prep_units`, `set_strict!`, `clear_strict!`, `strict?` | shared front-end → codegen tables; the pre-codegen gate + the opt-in strict native profile |
| `stdlib.backend.emit.llvm` | `emit_program`, `emit_freestanding`, `emit_program_debug`, `emit_freestanding_debug`, `set_target!`, `clear_target!`, `emit_with_target` | stdlib LLVM backend (incl. debug-info + per-emit target overrides) |
| `stdlib.backend.emit.wasm` | `emit_program`, `emit_module` | stdlib WASM (WAT) backend, sibling of `llvm` |
| `stdlib.backend.driver` | `compile_ir`, `compile_file`, `compile_freestanding`, `compile_surface_freestanding`, `link_ir!`, `link_bare!`, `targets` | clang/runtime build driver (incl. the one-call surface→freestanding ELF helper) |
| `stdlib.frontend.surface` | `apply_grammar!`, `parse_forms`, `parse_forms_at`, `form_to_spec`, `parse_to_specs`, `parse_text`, `template`, authoring helpers | custom grammar/lowering kit |
| `stdlib.frontend.clike` | `lower_program`, `lower_program_at`, `analyze_program`, `register_decl_kind!`, `register_attribute!` | C-like opt-in surface (extensible decl-kinds/attributes) |
| `stdlib.storage.binary` | `parse_storage`, `validate`, `generate`, `render_native`, `le_bytes`, `read_le`, `read_le_signed`, `pow256`, `crc32`, `emit!`, `pad_bytes`, `slice_bytes` | declarative binary-layout compiler used by TinyLogFS ([storage/README.md](../stdlib/storage/README.md)) |

## System Facades

`stdlib/sys/*.caap` are typed facades over `caap-sys-runtime`. `verify_sys`
checks the declarations against the live host-service catalog.

| Module | Purpose |
|---|---|
| `sys.io` | standard input/output |
| `sys.fs` | filesystem |
| `sys.os` | OS/environment |
| `sys.time` | clock/sleep |
| `sys.net` | networking |
| `sys.process` | process spawning |
| `stdlib.sys.verify` | `p`, `op`, `verify_facade`, `verify_sys` |
| `stdlib.sys.wrap` | `make_fn`, `make_facade`, `declare_ops!` |
