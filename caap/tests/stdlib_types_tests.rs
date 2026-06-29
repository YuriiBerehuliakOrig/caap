//! Type/effect system scenarios: typed containers, user type constructors,
//! enums, structs, defn signature/body checks, the kernel-driven type pass,
//! declared effect tags, inferred lambda results, and const purity.
use caap_core::RuntimeValue;

mod common;

use common::{corpus_path, eval_err_msg, eval_ok, with_stdlib_root};

/// The const purity guard: folding an effectful expression is refused at
/// expansion time (it would otherwise RUN the side effect), naming the
/// impure call.
#[test]
fn stdlib_const_refuses_impure_expressions() {
    let bad = corpus_path("fixtures/impure_const.caap");
    let msg = eval_err_msg(
        "stdlib_impure_const",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("const requires a pure expression"),
        "msg: {msg}"
    );
    assert!(msg.contains("`append`"), "names the impure call: {msg}");
    assert!(
        msg.contains("impure_const.caap:"),
        "expansion errors are located too: {msg}"
    );
}

/// TYPE CONSTRUCTORS ARE COMPILE-TIME FUNCTIONS over descriptors — generics
/// without new machinery. `(list int)` in a signature spells list<int>; the
/// registry runs the constructor lazily on first resolve and memoizes the
/// descriptor. Element types flow: list_of over ints INFERS list<int>; a
/// list<string> at a list<int> call site, an element flowing into int_add,
/// and an append of the wrong element are all CERTAIN errors with
/// sub-expression locations.
#[test]
fn stdlib_typed_containers_check_at_load() {
    let good = corpus_path("fixtures/generics.caap");
    let v = eval_ok(
        "generics_good",
        &with_stdlib_root(&format!("(get (load {good:?}) \"ok\" null)")),
    );
    assert_eq!(
        v,
        RuntimeValue::Int(3),
        "typed container module loads + runs"
    );

    for (name, fixture, needle) in [
        (
            "generics_bad_call",
            "fixtures/generics_bad_call.caap",
            "`total` arg 1: expected list<int>, got list<string>",
        ),
        (
            "generics_bad_elem",
            "fixtures/generics_bad_elem.caap",
            "`int_add` arg 1: expected int, got string",
        ),
        (
            "generics_bad_append",
            "fixtures/generics_bad_append.caap",
            "`append` onto list<int>: expected element int, got string",
        ),
    ] {
        let bad = corpus_path(fixture);
        let good = corpus_path("fixtures/generics.caap");
        let msg = eval_err_msg(
            name,
            &with_stdlib_root(&format!("(do (load {good:?}) (load {bad:?}))")),
        );
        assert!(msg.contains(needle), "{name}: {msg}");
    }
}

/// A USER-DEFINED type constructor: fixtures/user_typefn registers `pair` (a
/// compile-time function producing a struct-shaped descriptor); a later
/// module's `(pair int string)` signature instantiates it — field `a` types
/// as int (fst evaluates), and declaring field `b` (a string) as an int
/// result is a certain body mismatch.
#[test]
fn stdlib_user_type_constructor() {
    let ctor = corpus_path("fixtures/user_typefn.caap");
    let user = corpus_path("fixtures/uses_pair.caap");
    let bad = corpus_path("fixtures/uses_pair_bad.caap");
    let v = eval_ok(
        "user_typefn",
        &with_stdlib_root(&format!(
            "(do (load {ctor:?})
                 (bind ((m (load {user:?})))
                   ((get m \"fst\" null) (assoc (map_of) \"a\" 7 \"b\" \"x\"))))"
        )),
    );
    assert_eq!(
        v,
        RuntimeValue::Int(7),
        "user-constructed type works at eval"
    );

    let msg = eval_err_msg(
        "user_typefn_bad",
        &with_stdlib_root(&format!("(do (load {ctor:?}) (load {bad:?}))")),
    );
    assert!(
        msg.contains("`snd_as_int`: body returns string, declared result int"),
        "field types ride the user constructor: {msg}"
    );
}

/// The type pass reads EVERY builtin's signature from the kernel vocabulary
/// (not a hand-kept ~40-entry table): `string_upcase` was never in the old
/// param table, yet passing an int to it is now a certain error.
#[test]
fn stdlib_type_pass_is_exhaustive_over_kernel_builtins() {
    let bad = corpus_path("fixtures/exhaustive_table.caap");
    let msg = eval_err_msg(
        "exhaustive_table",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("`string_upcase` arg 1: expected string, got i32"),
        "msg: {msg}"
    );
}

/// Kernel-table mismatch: (int_add 1 "x") dies at load with the arg position.
#[test]
fn stdlib_type_pass_rejects_kernel_table_mismatch() {
    let bad = corpus_path("fixtures/type_mismatch.caap");
    let msg = eval_err_msg(
        "type_mismatch",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("`int_add` arg 2: expected int, got string"),
        "msg: {msg}"
    );
}

/// Sized-range check: a literal 300 cannot fit a declared u8 parameter.
#[test]
fn stdlib_type_pass_rejects_out_of_range_literal() {
    let bad = corpus_path("fixtures/bad_range.caap");
    let msg = eval_err_msg("bad_range", &with_stdlib_root(&format!("(load {bad:?})")));
    assert!(
        msg.contains("literal 300 out of range for u8"),
        "msg: {msg}"
    );
}

/// defn signature check, same module: a string into a u8 parameter.
#[test]
fn stdlib_type_pass_rejects_sig_mismatch() {
    let bad = corpus_path("fixtures/bad_sig_call.caap");
    let msg = eval_err_msg(
        "bad_sig_call",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("`add8` arg 2: expected u8, got string"),
        "msg: {msg}"
    );
    assert!(
        msg.contains("bad_sig_call.caap:"),
        "type findings are located: {msg}"
    );
}

/// Field access to a field the struct does not declare is a load-time error.
#[test]
fn stdlib_struct_rejects_unknown_field() {
    let bad = corpus_path("fixtures/bad_field.caap");
    let msg = eval_err_msg("bad_field", &with_stdlib_root(&format!("(load {bad:?})")));
    assert!(msg.contains("struct `Pt` has no field `z`"), "msg: {msg}");
}

/// The generated constructor carries the field types as its signature.
#[test]
fn stdlib_struct_rejects_bad_constructor_arg() {
    let bad = corpus_path("fixtures/bad_ctor_arg.caap");
    let msg = eval_err_msg(
        "bad_ctor_arg",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("`make_Pt2` arg 1: expected i32, got string"),
        "msg: {msg}"
    );
}

/// A defn whose body type contradicts its declared result is a load-time error.
#[test]
fn stdlib_defn_rejects_body_result_mismatch() {
    let bad = corpus_path("fixtures/bad_result.caap");
    let msg = eval_err_msg("bad_result", &with_stdlib_root(&format!("(load {bad:?})")));
    assert!(
        msg.contains("`wrong`: body returns int, declared result string"),
        "msg: {msg}"
    );
}

/// A defn body is walked exactly ONCE (the typed pairing walk): a body error
/// that doesn't involve the params must appear exactly once in the report.
#[test]
fn stdlib_defn_body_findings_are_not_duplicated() {
    let bad = corpus_path("fixtures/bad_body_arg.caap");
    let msg = eval_err_msg(
        "bad_body_arg",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert_eq!(
        msg.matches("`int_add` arg 2: expected int, got string")
            .count(),
        1,
        "finding reported exactly once: {msg}"
    );
}

/// Type phase 4 — branch JOIN: `(if c a 2)` with an i32 param and an int
/// literal joins to the int family (previously unknown), so the declared
/// string result is a certain contradiction.
#[test]
fn stdlib_type_pass_joins_if_branches() {
    let bad = corpus_path("fixtures/bad_if_result.caap");
    let msg = eval_err_msg(
        "bad_if_result",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("`pick`: body returns int, declared result string"),
        "msg: {msg}"
    );
}

/// Type phase 4 — match guards TYPE their bindings: (str? s) proves s is a
/// string inside the clause, so (int_add s 1) is a certain error.
#[test]
fn stdlib_type_pass_types_match_guard_bindings() {
    let bad = corpus_path("fixtures/bad_match_arm.caap");
    let msg = eval_err_msg(
        "bad_match_arm",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("`int_add` arg 1: expected int, got string"),
        "msg: {msg}"
    );
}

/// Type phase 4 — a PLAIN bind lambda gets an inferred signature: its int
/// result flows into the defn body check.
#[test]
fn stdlib_plain_lambda_gets_inferred_result() {
    let bad = corpus_path("fixtures/bad_plain_result.caap");
    let msg = eval_err_msg(
        "bad_plain_result",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("`shout5`: body returns int, declared result string"),
        "msg: {msg}"
    );
}

/// Type phase 4 — a builtin-alias facade (bind len size) inherits the
/// kernel's declared result type.
#[test]
fn stdlib_builtin_alias_carries_result_type() {
    let bad = corpus_path("fixtures/bad_facade_result.caap");
    let msg = eval_err_msg(
        "bad_facade_result",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("`label`: body returns int, declared result string"),
        "msg: {msg}"
    );
}

/// A declared tag LIST is verified: every certain effect of the body must be
/// among the declared tags.
#[test]
fn stdlib_declared_tags_are_verified() {
    let bad = corpus_path("fixtures/bad_declared_tags.caap");
    let msg = eval_err_msg(
        "bad_declared_tags",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains(
            "`push_bad` declared effects (emit_diagnostics) but its body has effect `mutation` (via `append`)"
        ),
        "msg: {msg}"
    );
}

/// enum negative: an explicit value that is not an integer literal is a
/// certain error, never a silent fall-through to auto-numbering.
#[test]
fn stdlib_enum_rejects_non_integer_value() {
    let bad = corpus_path("fixtures/enum_bad_value.caap");
    let msg = eval_err_msg(
        "enum_bad_value",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("enum value must be an integer literal"),
        "msg: {msg}"
    );
}

/// enum negative: re-declaring a registered type name fails at load.
#[test]
fn stdlib_enum_rejects_duplicate_type() {
    let bad = corpus_path("fixtures/enum_dup_type.caap");
    let msg = eval_err_msg(
        "enum_dup_type",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(msg.contains("type `Dup` is already defined"), "msg: {msg}");
}

/// Effects, negative: under the loader, a head whose effect cannot be PROVEN
/// pure (a plain local lambda) does not fold.
#[test]
fn stdlib_const_rejects_unprovable_head() {
    let bad = corpus_path("fixtures/const_unknown.caap");
    let msg = eval_err_msg(
        "const_unknown",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(msg.contains("cannot prove `twice` pure"), "msg: {msg}");
}

/// A declared effect at the name is VERIFIED against the body's inference:
/// `pure` over a certainly-mutating body is a load-time error.
#[test]
fn stdlib_declared_pure_is_verified_against_body() {
    let bad = corpus_path("fixtures/declared_pure_violation.caap");
    let msg = eval_err_msg(
        "declared_pure_violation",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("`bad` declared pure but calls impure `append`"),
        "msg: {msg}"
    );
}

/// LET-GENERALIZATION (B5/P1+P2) still catches genuine errors. `id` is a
/// single-param generic local; `(id "hi")` instantiates its fresh variable to
/// string, so its inferred result is string (sharper than the old `null`).
/// Feeding that to `want_int` (declared `(n int)`) is a CERTAIN mismatch —
/// generalization must REPORT it, not silently accept it.
#[test]
fn stdlib_letgen_still_catches_genuine_mismatch() {
    let bad = corpus_path("fixtures/letgen_bad.caap");
    let msg = eval_err_msg("letgen_bad", &with_stdlib_root(&format!("(load {bad:?})")));
    assert!(
        msg.contains("`want_int` arg 1: expected int, got string"),
        "generalized result must still flag a real mismatch: {msg}"
    );
}

/// REACHABILITY (HM-sound P-I): a diverging control form (`throw`) still has its
/// ARGUMENT walked, so a type error inside it is reported. The reachability
/// analysis annotates divergence WITHOUT cutting the argument walk — proving the
/// new control-form cases preserve the old fall-through's full arg traversal.
#[test]
fn stdlib_diverge_form_still_walks_its_argument() {
    let bad = corpus_path("fixtures/diverge_arg_walked.caap");
    let msg = eval_err_msg(
        "diverge_arg_walked",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("`int_add` arg 1: expected int, got string"),
        "the throw's argument must still be type-walked: {msg}"
    );
}
