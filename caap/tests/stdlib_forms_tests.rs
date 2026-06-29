/// Foundation tests for the from-scratch stdlib (`stdlib`).
///
/// stdlib treats a "macro" as a plain compile-time `spec -> spec` transform.
/// One expansion pass (`stdlib.expand`) rewrites sugar into pure kernel AST;
/// that AST is then consumed identically by eval (`ctfe_eval_node`) and, later,
/// by lowering. These tests are the proof gate for that design:
///   1. expanded sugar evaluates to the right value (eval path), and
///   2. the expanded tree is pure kernel forms (`if`/`do`), i.e. compile-ready —
///      no `cond`/`when` head survives for a downstream consumer to choke on.
///
/// stdlib v1 split these: sugar lived as compile-pipeline normalizers that never
/// reached module source loaded via raw eval. Here one transform serves both.
use caap_core::RuntimeValue;

mod common;

use common::{eval_with_expander, eval_with_expander_err};

/// The engine enforces each form's declared arity centrally (define_form min/max):
/// a malformed form fails with a clear "wrong arg count" diagnostic rather than a
/// cryptic deep failure.
#[test]
fn stdlib_form_arity_is_enforced() {
    // (if_let (x 5)) — if_let expects 3 args (binding, then, else), got 1.
    let msg = eval_with_expander_err(
        "(expand (scall (snm \"if_let\")
           (list_of (scall (snm \"x\") (list_of (slit 5))))))",
    );
    assert!(msg.contains("wrong arg count"), "msg: {msg}");
    assert!(msg.contains("if_let"), "names the form: {msg}");
}

/// `cond` expands and evaluates correctly, AND the expanded tree is kernel `if`
/// (not `cond`) — proving one transform yields AST both paths can consume.
#[test]
fn stdlib_cond_expands_to_kernel_if_and_evaluates() {
    // (cond ((lt 5 2) "a") ((lt 1 0) "b") (else "fallback"))
    let result = eval_with_expander(
        "(bind (
           (sugar
             (scall (snm \"cond\")
               (list_of
                 (scall (scall (snm \"lt\") (list_of (slit 5) (slit 2))) (list_of (slit \"a\")))
                 (scall (scall (snm \"lt\") (list_of (slit 1) (slit 0))) (list_of (slit \"b\")))
                 (scall (snm \"else\") (list_of (slit \"fallback\"))))))
           (expanded (expand sugar))
         )
           (list_of
             (ctfe_eval_node expanded)
             (syntax_kind expanded)
             (syntax_name_identifier (syntax_call_callee expanded))))",
    );
    let RuntimeValue::List(items) = result else {
        panic!("expected list, got {result:?}");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("fallback".into()), "cond value");
    assert_eq!(
        items[1],
        RuntimeValue::Str("call".into()),
        "expanded is a call"
    );
    assert_eq!(
        items[2],
        RuntimeValue::Str("if".into()),
        "expanded head is kernel `if`, not `cond` — compile-ready"
    );
}

/// Visible demonstration: render the INPUT form, the EXPANDED IR, and the
/// evaluated RESULT for `cond` and `unless`. Run with `--nocapture` to see them.
/// Asserts the expanded IR is built from kernel `if`/`do` and evaluates right.
#[test]
fn stdlib_demo_cond_and_unless_render_expanded_ir() {
    // A tiny spec->text renderer (defined in-program) plus three lines per form:
    // input, expanded IR, result.
    let result = eval_with_expander(
        "(bind (
           (render
             (lambda (self spec)
               (bind ((k (syntax_kind spec)))
                 (if (eq k \"name\") (syntax_name_identifier spec)
                   (if (eq k \"literal\")
                     (bind ((v (syntax_literal_value spec)) (t (value_type v)))
                       (if (eq t \"string\") (string_concat_many \"\\\"\" v \"\\\"\")
                         (if (eq t \"int\") (int_to_string v)
                           (if (eq t \"bool\") (if v \"true\" \"false\")
                             (if (eq t \"null\") \"null\" \"?\")))))
                     (string_concat_many \"(\" (render self (syntax_call_callee spec))
                       (sequence_fold_left (syntax_call_args spec) \"\"
                         (lambda (acc a) (string_concat_many acc \" \" (render self a))))
                       \")\"))))))
           (show (lambda (spec) (render render spec)))
           ; (cond ((lt 5 2) \"a\") ((lt 1 2) \"b\") (else \"c\"))  -> \"b\"
           (cond_in
             (scall (snm \"cond\")
               (list_of
                 (scall (scall (snm \"lt\") (list_of (slit 5) (slit 2))) (list_of (slit \"a\")))
                 (scall (scall (snm \"lt\") (list_of (slit 1) (slit 2))) (list_of (slit \"b\")))
                 (scall (snm \"else\") (list_of (slit \"c\"))))))
           ; (unless (lt 5 2) \"ran\")  -> \"ran\"
           (unless_in
             (scall (snm \"unless\")
               (list_of (scall (snm \"lt\") (list_of (slit 5) (slit 2))) (slit \"ran\"))))
           (cond_out (expand cond_in))
           (unless_out (expand unless_in))
         )
           (list_of
             (show cond_in)   (show cond_out)   (ctfe_eval_node cond_out)
             (show unless_in) (show unless_out) (ctfe_eval_node unless_out)))",
    );
    let RuntimeValue::List(items) = result else {
        panic!("expected list, got {result:?}");
    };
    let items = items.borrow();
    let s = |v: &RuntimeValue| match v {
        RuntimeValue::Str(s) => s.to_string(),
        other => format!("{other:?}"),
    };
    println!("\n── cond ──");
    println!("  input    : {}", s(&items[0]));
    println!("  expanded : {}", s(&items[1]));
    println!("  result   : {}", s(&items[2]));
    println!("── unless ──");
    println!("  input    : {}", s(&items[3]));
    println!("  expanded : {}", s(&items[4]));
    println!("  result   : {}", s(&items[5]));

    // cond expands to nested kernel `if` and picks the second clause.
    assert!(
        s(&items[1]).starts_with("(if "),
        "cond -> if: {}",
        s(&items[1])
    );
    assert_eq!(items[2], RuntimeValue::Str("b".into()));
    // unless expands to `(if test null (do …))` and runs the body (5<2 is false).
    assert!(
        s(&items[4]).starts_with("(if "),
        "unless -> if: {}",
        s(&items[4])
    );
    assert_eq!(items[5], RuntimeValue::Str("ran".into()));
}

/// `const` is the proof that a form is a compile-time FUNCTION, not a textual
/// substitution: it evaluates its argument at compile time and forms IR (a
/// literal) from the result. `(const (int_mul (int_add 1 2) 4))` folds to the
/// literal 12 — the expanded node is a `literal`, not a `call`.
#[test]
fn stdlib_const_evaluates_at_compile_time_and_forms_a_literal() {
    let result = eval_with_expander(
        "(bind (
           (sugar
             (scall (snm \"const\")
               (list_of
                 (scall (snm \"int_mul\")
                   (list_of
                     (scall (snm \"int_add\") (list_of (slit 1) (slit 2)))
                     (slit 4))))))
           (expanded (expand sugar))
         )
           (list_of
             (ctfe_eval_node expanded)
             (syntax_kind expanded)))",
    );
    let RuntimeValue::List(items) = result else {
        panic!("expected list, got {result:?}");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(12), "const folds to 12");
    assert_eq!(
        items[1],
        RuntimeValue::Str("literal".into()),
        "const produced a folded literal node, not a runtime call"
    );
}

/// `when` lowers to `(if test (do body…) null)` and yields the body's value.
#[test]
fn stdlib_when_expands_and_evaluates() {
    // (when (lt 1 2) 7 8 9)  -> 9 ;  (when (lt 5 2) 1) -> null
    let result = eval_with_expander(
        "(list_of
           (ctfe_eval_node (expand
             (scall (snm \"when\")
               (list_of (scall (snm \"lt\") (list_of (slit 1) (slit 2)))
                        (slit 7) (slit 8) (slit 9)))))
           (ctfe_eval_node (expand
             (scall (snm \"when\")
               (list_of (scall (snm \"lt\") (list_of (slit 5) (slit 2)))
                        (slit 1))))))",
    );
    let RuntimeValue::List(items) = result else {
        panic!("expected list, got {result:?}");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(9), "when body sequence value");
    assert_eq!(items[1], RuntimeValue::Null, "when on false -> null");
}

/// Lazy: an unmatched `cond` clause must never evaluate its body. If expansion
/// (or evaluation) were eager, the `throw` would fire.
#[test]
fn stdlib_cond_does_not_evaluate_unmatched_clauses() {
    // (cond ((eq 1 1) "safe") (else (throw "boom")))
    let result = eval_with_expander(
        "(ctfe_eval_node (expand
           (scall (snm \"cond\")
             (list_of
               (scall (scall (snm \"eq\") (list_of (slit 1) (slit 1))) (list_of (slit \"safe\")))
               (scall (snm \"else\") (list_of (scall (snm \"throw\") (list_of (slit \"boom\")))))))))",
    );
    assert_eq!(result, RuntimeValue::Str("safe".into()));
}

/// A form can VALIDATE and REPORT — checking a textual macro cannot do. A `cond`
/// with `else` not last yields an error diagnostic (via `expand_with_diagnostics`),
/// and the strict `expand` raises on it.
#[test]
fn stdlib_cond_reports_misplaced_else_diagnostic() {
    // (cond (else "x") ((lt 1 2) "y"))  — else is not last.
    let diags = eval_with_expander(
        "(bind (
           (sugar
             (scall (snm \"cond\")
               (list_of
                 (scall (snm \"else\") (list_of (slit \"x\")))
                 (scall (scall (snm \"lt\") (list_of (slit 1) (slit 2))) (list_of (slit \"y\"))))))
           (result (expand_wd sugar))
           (ds (get result \"diagnostics\" (list_of)))
         )
           (list_of
             (size ds)
             (get (get ds 0 null) \"severity\" null)
             (get (get ds 0 null) \"message\" null)))",
    );
    let RuntimeValue::List(items) = diags else {
        panic!("expected list, got {diags:?}");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(1), "exactly one diagnostic");
    assert_eq!(
        items[1],
        RuntimeValue::Str("error".into()),
        "severity error"
    );
    let RuntimeValue::Str(msg) = &items[2] else {
        panic!("message not a string");
    };
    assert!(msg.contains("else"), "diagnostic mentions else: {msg}");
}

/// Threading `->` inserts the accumulator FIRST, `->>` inserts it LAST.
#[test]
fn stdlib_thread_first_and_last() {
    let result = eval_with_expander(
        "(list_of
           ; (-> 5 (int_add 3) (int_mul 2)) -> (int_mul (int_add 5 3) 2) = 16
           (ctfe_eval_node (expand
             (scall (snm \"->\")
               (list_of (slit 5)
                 (scall (snm \"int_add\") (list_of (slit 3)))
                 (scall (snm \"int_mul\") (list_of (slit 2)))))))
           ; (->> 5 (int_sub 3)) -> (int_sub 3 5) = -2
           (ctfe_eval_node (expand
             (scall (snm \"->>\")
               (list_of (slit 5)
                 (scall (snm \"int_sub\") (list_of (slit 3))))))))",
    );
    let RuntimeValue::List(items) = result else {
        panic!("expected list, got {result:?}");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(16), "-> threads first arg");
    assert_eq!(items[1], RuntimeValue::Int(-2), "->> threads last arg");
}

/// `for` iterates a sequence, binding each element. Sugar over `sequence_each`.
#[test]
fn stdlib_for_iterates_a_sequence() {
    // (bind ((acc (list_of))) (do (for x (list_of 10 20 30) (append acc x)) acc)) -> [10,20,30]
    let result = eval_with_expander(
        "(ctfe_eval_node (expand
           (scall (snm \"bind\")
             (list_of
               (scall (scall (snm \"acc\") (list_of (scall (snm \"list_of\") (list_of)))) (list_of))
               (scall (snm \"do\")
                 (list_of
                   (scall (snm \"for\")
                     (list_of (snm \"x\") (scall (snm \"list_of\") (list_of (slit 10) (slit 20) (slit 30)))
                       (scall (snm \"append\") (list_of (snm \"acc\") (snm \"x\")))))
                   (snm \"acc\")))))))",
    );
    let RuntimeValue::List(items) = result else {
        panic!("expected list result, got {result:?}");
    };
    let items = items.borrow();
    assert_eq!(
        items.as_slice(),
        [
            RuntimeValue::Int(10),
            RuntimeValue::Int(20),
            RuntimeValue::Int(30)
        ]
    );
}

/// `with_map` destructures a map: each local binds to (get m "key" default), with
/// the map evaluated once. Kills the (get x "k" null) ladder.
#[test]
fn stdlib_with_map_destructures_with_defaults() {
    // (with_map (assoc (map_of) "a" 10 "b" 20) ((a "a") (b "b") (c "c" 99))
    //   (int_add (int_add a b) c))  -> 10 + 20 + 99 = 129
    let result = eval_with_expander(
        "(ctfe_eval_node (expand
           (scall (snm \"with_map\")
             (list_of
               (scall (snm \"assoc\")
                 (list_of (scall (snm \"map_of\") (list_of))
                          (slit \"a\") (slit 10) (slit \"b\") (slit 20)))
               (scall (scall (snm \"a\") (list_of (slit \"a\")))
                 (list_of
                   (scall (snm \"b\") (list_of (slit \"b\")))
                   (scall (snm \"c\") (list_of (slit \"c\") (slit 99)))))
               (scall (snm \"int_add\")
                 (list_of (scall (snm \"int_add\") (list_of (snm \"a\") (snm \"b\")))
                          (snm \"c\")))))))",
    );
    assert_eq!(result, RuntimeValue::Int(129), "a+b + default c(99)");
}

/// `if_let` binds and branches on non-null; `when_let` runs a body when present.
#[test]
fn stdlib_if_let_and_when_let() {
    let result = eval_with_expander(
        "(list_of
           ; (if_let (x 5) (int_add x 1) -1) -> 6
           (ctfe_eval_node (expand
             (scall (snm \"if_let\")
               (list_of (scall (snm \"x\") (list_of (slit 5)))
                 (scall (snm \"int_add\") (list_of (snm \"x\") (slit 1)))
                 (slit -1)))))
           ; (if_let (x null) x -1) -> -1
           (ctfe_eval_node (expand
             (scall (snm \"if_let\")
               (list_of (scall (snm \"x\") (list_of (slit null)))
                 (snm \"x\") (slit -1)))))
           ; (when_let (x 5) (int_add x 1)) -> 6
           (ctfe_eval_node (expand
             (scall (snm \"when_let\")
               (list_of (scall (snm \"x\") (list_of (slit 5)))
                 (scall (snm \"int_add\") (list_of (snm \"x\") (slit 1)))))))
           ; (when_let (x null) 1) -> null
           (ctfe_eval_node (expand
             (scall (snm \"when_let\")
               (list_of (scall (snm \"x\") (list_of (slit null))) (slit 1))))))",
    );
    let RuntimeValue::List(items) = result else {
        panic!("expected list, got {result:?}");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(6), "if_let present");
    assert_eq!(items[1], RuntimeValue::Int(-1), "if_let null -> else");
    assert_eq!(items[2], RuntimeValue::Int(6), "when_let present");
    assert_eq!(items[3], RuntimeValue::Null, "when_let null -> null");
}

/// `case` evaluates the scrutinee once and dispatches by equality (lowers through
/// `cond`). Matches the second clause; falls through to `else`.
#[test]
fn stdlib_case_dispatches_by_equality() {
    let result = eval_with_expander(
        "(list_of
           ; (case 2 (1 \"a\") (2 \"b\") (else \"z\")) -> \"b\"
           (ctfe_eval_node (expand
             (scall (snm \"case\")
               (list_of (slit 2)
                 (scall (slit 1) (list_of (slit \"a\")))
                 (scall (slit 2) (list_of (slit \"b\")))
                 (scall (snm \"else\") (list_of (slit \"z\")))))))
           ; (case 9 (1 \"a\") (else \"z\")) -> \"z\"
           (ctfe_eval_node (expand
             (scall (snm \"case\")
               (list_of (slit 9)
                 (scall (slit 1) (list_of (slit \"a\")))
                 (scall (snm \"else\") (list_of (slit \"z\"))))))))",
    );
    let RuntimeValue::List(items) = result else {
        panic!("expected list, got {result:?}");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("b".into()), "case matches 2");
    assert_eq!(
        items[1],
        RuntimeValue::Str("z".into()),
        "case falls to else"
    );
}

/// Nested sugar expands fully: a `when` inside a `cond` else-branch must also be
/// reduced to kernel forms by the single fixpoint pass.
#[test]
fn stdlib_nested_sugar_expands_to_fixpoint() {
    // (cond ((lt 9 0) "no") (else (when (lt 1 2) "deep")))  -> "deep"
    let result = eval_with_expander(
        "(ctfe_eval_node (expand
           (scall (snm \"cond\")
             (list_of
               (scall (scall (snm \"lt\") (list_of (slit 9) (slit 0))) (list_of (slit \"no\")))
               (scall (snm \"else\")
                 (list_of
                   (scall (snm \"when\")
                     (list_of (scall (snm \"lt\") (list_of (slit 1) (slit 2)))
                              (slit \"deep\")))))))))",
    );
    assert_eq!(result, RuntimeValue::Str("deep".into()));
}
