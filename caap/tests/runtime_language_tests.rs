/// Integration tests for the CAAP core evaluator.
///
/// Each test builds a small IR graph by hand and runs it through the evaluator.
use caap_core::{
    frontend::parse,
    graph::GraphBuilder,
    ir::{IrLiteralData, NodeId},
    values::{Environment, EvalSignal},
    Evaluator, RuntimeValue,
};

// ── helpers ───────────────────────────────────────────────────────────────────

fn lit_int(v: i64) -> IrLiteralData {
    IrLiteralData::Int(v)
}

fn lit_bool(v: bool) -> IrLiteralData {
    IrLiteralData::Bool(v)
}

fn lit_null() -> IrLiteralData {
    IrLiteralData::Null
}

fn lit_str(s: &str) -> IrLiteralData {
    IrLiteralData::Str(s.to_string())
}

trait TestGraphBuilderExt {
    fn name(&mut self, identifier: impl Into<String>) -> NodeId;
    fn literal(&mut self, value: IrLiteralData) -> NodeId;
    fn call(&mut self, callee: NodeId, args: Vec<NodeId>) -> NodeId;
}

impl TestGraphBuilderExt for GraphBuilder {
    fn name(&mut self, identifier: impl Into<String>) -> NodeId {
        self.try_name(identifier)
            .expect("test graph name must be valid")
    }

    fn literal(&mut self, value: IrLiteralData) -> NodeId {
        self.try_literal(value)
            .expect("test graph literal must be valid")
    }

    fn call(&mut self, callee: NodeId, args: Vec<NodeId>) -> NodeId {
        self.try_call(callee, args)
            .expect("test graph call must reference existing nodes")
    }
}

fn eval_one(b: &mut GraphBuilder, root_id: u32) -> RuntimeValue {
    b.graph.root_id = root_id;
    let env = Environment::new(None);
    let mut ev = Evaluator::new(std::mem::take(&mut b.graph));
    ev.eval(root_id, &env).expect("evaluation failed")
}

fn eval_one_result(b: &mut GraphBuilder, root_id: u32) -> Result<RuntimeValue, EvalSignal> {
    b.graph.root_id = root_id;
    let env = Environment::new(None);
    let mut ev = Evaluator::new(std::mem::take(&mut b.graph));
    ev.eval(root_id, &env)
}

fn eval_source(src: &str) -> Result<RuntimeValue, EvalSignal> {
    let graph = parse(src).unwrap();
    let mut ev = Evaluator::new(graph);
    ev.run()
}

#[test]
fn test_top_level_canonical_bind_rejects_non_string_name() {
    let mut b = GraphBuilder::new();
    let bind = b.name("bind");
    let first_name = b.literal(lit_str("x"));
    let first_value = b.literal(lit_int(1));
    let bad_name = b.literal(lit_int(2));
    let bad_value = b.literal(lit_int(3));
    let body = b.name("x");
    let root = b.call(
        bind,
        vec![first_name, first_value, bad_name, bad_value, body],
    );
    b.graph.root_id = root;
    b.graph.add_top_level_form(root).unwrap();

    let mut ev = Evaluator::new(std::mem::take(&mut b.graph));
    let error = ev
        .run()
        .expect_err("malformed top-level canonical bind must fail loudly");
    assert!(
        error
            .to_string()
            .contains("bind canonical names must be non-empty strings"),
        "unexpected diagnostic: {error}"
    );
}

#[test]
fn test_literal_int() {
    let mut b = GraphBuilder::new();
    let id = b.literal(lit_int(42));
    assert_eq!(eval_one(&mut b, id), RuntimeValue::Int(42));
}

#[test]
fn test_literal_bool_true() {
    let mut b = GraphBuilder::new();
    let id = b.literal(lit_bool(true));
    assert_eq!(eval_one(&mut b, id), RuntimeValue::Bool(true));
}

#[test]
fn test_literal_null() {
    let mut b = GraphBuilder::new();
    let id = b.literal(lit_null());
    assert_eq!(eval_one(&mut b, id), RuntimeValue::Null);
}

#[test]
fn test_effect_scope_blocks_effectful_builtin() {
    let error = eval_source("(effect_scope (list_of) (append (list_of 1) 2))")
        .expect_err("pure effect scope must block mutation");
    assert!(
        error
            .to_string()
            .contains("requires effect(s) [mutation] outside active effect scope []"),
        "unexpected diagnostic: {error}"
    );
}

#[test]
fn test_effect_scope_allows_declared_effect() {
    let result = eval_source("(effect_scope (list_of \"mutation\") (append (list_of 1) 2))")
        .expect("mutation scope should allow append");
    let RuntimeValue::List(items) = result else {
        panic!("expected list result, got {result:?}");
    };
    assert_eq!(
        items.borrow().as_slice(),
        [RuntimeValue::Int(1), RuntimeValue::Int(2)]
    );
}

#[test]
fn test_nested_effect_scope_cannot_escalate() {
    let error = eval_source(
        "(effect_scope (list_of)
           (effect_scope (list_of \"mutation\")
             (append (list_of 1) 2)))",
    )
    .expect_err("nested effect scope must not grant effects outside parent scope");
    assert!(
        error
            .to_string()
            .contains("effect-scope cannot grant effects outside the active scope"),
        "unexpected diagnostic: {error}"
    );
}

#[test]
fn test_effect_scope_applies_through_closure_callbacks() {
    let error = eval_source(
        "(bind ((untrusted (lambda (items) (append items 2))))
           (effect_scope (list_of)
             (untrusted (list_of 1))))",
    )
    .expect_err("effect scope must apply inside called closures");
    assert!(
        error
            .to_string()
            .contains("requires effect(s) [mutation] outside active effect scope []"),
        "unexpected diagnostic: {error}"
    );
}

#[test]
fn test_runtime_macro_expands_quoted_syntax_in_caller_env() {
    let result = eval_source(
        "(bind ((unless (macro (cond body)
                  (syntax_call
                    (syntax_name \"if\")
                    (list_of cond (syntax_literal null) body)))))
           (unless false (int_add 1 2)))",
    )
    .expect("macro expansion should evaluate in caller env");
    assert_eq!(result, RuntimeValue::Int(3));
}

#[test]
fn test_runtime_macro_preserves_lazy_arguments() {
    let result = eval_source(
        "(bind ((unless (macro (cond body)
                  (syntax_call
                    (syntax_name \"if\")
                    (list_of cond (syntax_literal null) body)))))
           (unless true (runtime_error \"boom\")))",
    )
    .expect("macro must not evaluate unused argument syntax");
    assert_eq!(result, RuntimeValue::Null);
}

#[test]
fn test_runtime_macro_can_inspect_quoted_tree() {
    let result = eval_source(
        "(bind ((describe (macro (form)
                  (syntax_literal (syntax_kind form)))))
           (describe (int_add 1 2)))",
    )
    .expect("macro should inspect quoted syntax");
    assert_eq!(result, RuntimeValue::Str("call".into()));
}

#[test]
fn test_runtime_macro_requires_syntax_result() {
    let error = eval_source("(bind ((bad (macro (x) 1))) (bad 2))")
        .expect_err("macro returning a value must be rejected");
    assert!(
        error
            .to_string()
            .contains("macro expansion must return syntax"),
        "unexpected diagnostic: {error}"
    );
}

#[test]
fn test_literal_tuple_runtime_value() {
    let mut b = GraphBuilder::new();
    let id = b.literal(IrLiteralData::Tuple(vec![
        IrLiteralData::Int(1),
        IrLiteralData::Str("two".to_string()),
        IrLiteralData::Bool(true),
    ]));
    match eval_one(&mut b, id) {
        RuntimeValue::Tuple(items) => {
            assert_eq!(items.len(), 3);
            assert_eq!(items[0], RuntimeValue::Int(1));
            assert_eq!(items[1], RuntimeValue::Str("two".into()));
            assert_eq!(items[2], RuntimeValue::Bool(true));
        }
        other => panic!("expected tuple, got {other}"),
    }
}

#[test]
fn test_literal_dict_runtime_value() {
    let mut b = GraphBuilder::new();
    let id = b.literal(IrLiteralData::Dict(vec![
        ("a".to_string(), IrLiteralData::Int(1)),
        (
            "nested".to_string(),
            IrLiteralData::Tuple(vec![IrLiteralData::Str("x".to_string())]),
        ),
    ]));
    match eval_one(&mut b, id) {
        RuntimeValue::Map(m) => {
            use caap_core::MapKey;
            let map = m.borrow();
            assert_eq!(
                map.get(&MapKey::Str("a".into())),
                Some(&RuntimeValue::Int(1))
            );
            match map.get(&MapKey::Str("nested".into())) {
                Some(RuntimeValue::Tuple(items)) => {
                    assert_eq!(items.as_ref(), [RuntimeValue::Str("x".into())]);
                }
                other => panic!("expected nested tuple, got {other:?}"),
            }
        }
        other => panic!("expected map, got {other}"),
    }
}

// ── arithmetic ────────────────────────────────────────────────────────────────

fn eval_arith(op: &str, left: i64, right: i64) -> RuntimeValue {
    let mut b = GraphBuilder::new();
    let fn_node = b.name(op);
    let l = b.literal(lit_int(left));
    let r = b.literal(lit_int(right));
    let call_id = b.call(fn_node, vec![l, r]);
    eval_one(&mut b, call_id)
}

fn eval_arith_result(op: &str, left: i64, right: i64) -> Result<RuntimeValue, EvalSignal> {
    let mut b = GraphBuilder::new();
    let fn_node = b.name(op);
    let l = b.literal(lit_int(left));
    let r = b.literal(lit_int(right));
    let call_id = b.call(fn_node, vec![l, r]);
    eval_one_result(&mut b, call_id)
}

fn eval_int_unary(op: &str, value: i64) -> RuntimeValue {
    let mut b = GraphBuilder::new();
    let fn_node = b.name(op);
    let v = b.literal(lit_int(value));
    let call_id = b.call(fn_node, vec![v]);
    eval_one(&mut b, call_id)
}

#[test]
fn test_int_add() {
    assert_eq!(eval_arith("int_add", 3, 4), RuntimeValue::Int(7));
}

#[test]
fn test_int_sub() {
    assert_eq!(eval_arith("int_sub", 10, 3), RuntimeValue::Int(7));
}

#[test]
fn test_int_mul() {
    assert_eq!(eval_arith("int_mul", 6, 7), RuntimeValue::Int(42));
}

#[test]
fn test_int_div() {
    assert_eq!(eval_arith("int_div", 17, 5), RuntimeValue::Int(3));
}

#[test]
fn test_int_div_by_zero() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("int_div");
    let l = b.literal(lit_int(10));
    let r = b.literal(lit_int(0));
    let call_id = b.call(fn_node, vec![l, r]);
    let env = Environment::new(None);
    let graph = std::mem::take(&mut b.graph);
    let mut ev = Evaluator::new(graph);
    assert!(ev.eval(call_id, &env).is_err());
}

#[test]
fn test_int_rem() {
    assert_eq!(eval_arith("int_rem", 17, 5), RuntimeValue::Int(2));
}

#[test]
fn test_int_mod() {
    assert_eq!(eval_arith("int_mod", -1, 5), RuntimeValue::Int(4));
}

#[test]
fn test_int_abs_positive() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("int_abs");
    let v = b.literal(lit_int(-7));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(7));
}

#[test]
fn test_int_and() {
    assert_eq!(
        eval_arith("int_and", 0b1010, 0b1100),
        RuntimeValue::Int(0b1000)
    );
}

#[test]
fn test_int_xor() {
    assert_eq!(
        eval_arith("int_xor", 0b1010, 0b1100),
        RuntimeValue::Int(0b0110)
    );
}

#[test]
fn test_int_or() {
    assert_eq!(
        eval_arith("int_or", 0b1010, 0b1100),
        RuntimeValue::Int(0b1110)
    );
}

#[test]
fn test_int_not() {
    assert_eq!(eval_int_unary("int_not", 0), RuntimeValue::Int(!0));
}

#[test]
fn test_int_shl() {
    assert_eq!(eval_arith("int_shl", 2, 3), RuntimeValue::Int(16));
}

#[test]
fn test_int_shr() {
    assert_eq!(eval_arith("int_shr", 16, 2), RuntimeValue::Int(4));
}

#[test]
fn test_integer_shifts_reject_out_of_range_amounts() {
    let shr_error = eval_arith_result("int_shr", 1, 64)
        .expect_err("int-shr must reject oversized shifts")
        .to_string();
    assert!(shr_error.contains("shift amount must be in 0..63"));

    let shl_error = eval_arith_result("int_shl", 1, -1)
        .expect_err("int-shl must reject negative shifts")
        .to_string();
    assert!(shl_error.contains("shift amount must be in 0..63"));
}

#[test]
fn test_int_to_float() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("int_to_float");
    let v = b.literal(lit_int(3));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Float(3.0));
}

#[test]
fn test_float_to_int() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("float_to_int");
    let v = b.literal(IrLiteralData::Float(3.7));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(3));
}

#[test]
fn test_float_to_int_rejects_non_finite_and_out_of_range_values() {
    for (value, expected) in [
        (f64::NAN, "value must be finite"),
        (f64::INFINITY, "value must be finite"),
        (9_223_372_036_854_775_808.0, "outside int range"),
    ] {
        let mut b = GraphBuilder::new();
        let fn_node = b.name("float_to_int");
        let v = b.literal(IrLiteralData::Float(value));
        let call_id = b.call(fn_node, vec![v]);
        let error = eval_one_result(&mut b, call_id).expect_err("float-to-int should reject value");
        let EvalSignal::Error(error) = error else {
            panic!("expected evaluation error");
        };
        assert!(
            error.message().contains(expected),
            "expected {expected:?}, got {:?}",
            error.message()
        );
    }
}

// ── comparison ────────────────────────────────────────────────────────────────

#[test]
fn test_eq_true() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("eq");
    let a = b.literal(lit_int(5));
    let c = b.literal(lit_int(5));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_eq_false() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("eq");
    let a = b.literal(lit_int(5));
    let c = b.literal(lit_int(6));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(false));
}

#[test]
fn test_eq_is_type_strict_for_numeric_types() {
    // eq uses structural equality: Int(0) != Float(0.0)
    // Use lt-based zero check: (and (not (lt v 0)) (not (lt 0 v))) for cross-type zero test
    let mut b = GraphBuilder::new();
    let fn_node = b.name("eq");
    let a = b.literal(lit_int(0));
    let c = b.literal(IrLiteralData::Float(0.0));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(false));
}

#[test]
fn test_lt_true() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("lt");
    let a = b.literal(lit_int(3));
    let c = b.literal(lit_int(7));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_lt_supports_mixed_numeric_values() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("lt");
    let a = b.literal(lit_int(3));
    let c = b.literal(IrLiteralData::Float(3.5));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_gt_false() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("gt");
    let a = b.literal(lit_int(3));
    let c = b.literal(lit_int(7));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(false));
}

#[test]
fn test_not_false() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("not");
    let v = b.literal(lit_bool(true));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(false));
}

// ── control flow ─────────────────────────────────────────────────────────────

#[test]
fn test_if_then() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("if");
    let cond = b.literal(lit_bool(true));
    let then_br = b.literal(lit_int(1));
    let else_br = b.literal(lit_int(2));
    let call_id = b.call(fn_node, vec![cond, then_br, else_br]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(1));
}

#[test]
fn test_if_else() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("if");
    let cond = b.literal(lit_bool(false));
    let then_br = b.literal(lit_int(1));
    let else_br = b.literal(lit_int(2));
    let call_id = b.call(fn_node, vec![cond, then_br, else_br]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(2));
}

#[test]
fn test_if_no_else_returns_null() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("if");
    let cond = b.literal(lit_bool(false));
    let then_br = b.literal(lit_int(99));
    let call_id = b.call(fn_node, vec![cond, then_br]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Null);
}

#[test]
fn test_do_returns_last() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("do");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let d = b.literal(lit_int(3));
    let call_id = b.call(fn_node, vec![a, c, d]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(3));
}

#[test]
fn test_or_short_circuits() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("or");
    let f = b.literal(lit_bool(false));
    let t = b.literal(lit_int(42));
    let call_id = b.call(fn_node, vec![f, t]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(42));
}

#[test]
fn test_and_short_circuits_falsey() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("and");
    let t = b.literal(lit_bool(true));
    let f = b.literal(lit_bool(false));
    let call_id = b.call(fn_node, vec![t, f]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(false));
}

// ── lambda + call ─────────────────────────────────────────────────────────────

/// Build (lambda (x) (int-add x 1)) then call it with 5 → 6.
///
/// Params list node convention: CallNode { callee: dummy, args: [NameNode param, ...] }
#[test]
fn test_lambda_and_call() {
    let mut b = GraphBuilder::new();

    // params list: (__ x) — callee is ignored, args are the param names
    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params_call = b.call(params_callee, vec![param_x]);

    // body: (int-add x 1)
    let add_fn = b.name("int_add");
    let body_x = b.name("x");
    let one = b.literal(lit_int(1));
    let body = b.call(add_fn, vec![body_x, one]);

    // lambda: (lambda params_call body)
    let lambda_fn = b.name("lambda");
    let lambda_call = b.call(lambda_fn, vec![params_call, body]);

    // outer call: apply the lambda to 5
    let five = b.literal(lit_int(5));
    let apply_call = b.call(lambda_call, vec![five]);

    assert_eq!(eval_one(&mut b, apply_call), RuntimeValue::Int(6));
}

#[test]
fn test_lambda_rest_param_collects_extra_args() {
    let graph = parse(
        "(list_of
          ((lambda (first &rest) rest) 1 2 3)
          ((lambda (&args) args) 4 5)
          ((lambda (first &empty) empty) 6))",
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);

    let RuntimeValue::List(items) = ev.run().unwrap() else {
        panic!("expected list result");
    };
    let items = items.borrow();
    let RuntimeValue::List(first_rest) = &items[0] else {
        panic!("expected first rest list");
    };
    assert_eq!(
        first_rest.borrow().as_slice(),
        &[RuntimeValue::Int(2), RuntimeValue::Int(3)]
    );
    let RuntimeValue::List(all_args) = &items[1] else {
        panic!("expected all args list");
    };
    assert_eq!(
        all_args.borrow().as_slice(),
        &[RuntimeValue::Int(4), RuntimeValue::Int(5)]
    );
    let RuntimeValue::List(empty_rest) = &items[2] else {
        panic!("expected empty rest list");
    };
    assert!(empty_rest.borrow().is_empty());
}

#[test]
fn test_lambda_rest_param_requires_minimum_arity() {
    let graph = parse("((lambda (first second &rest) first) 1)").unwrap();
    let mut ev = Evaluator::new(graph);

    let err = ev.run().expect_err("expected arity error");
    assert!(format!("{err:?}").contains("lambda expected at least 2 args"));
}

// ── bind ──────────────────────────────────────────────────────────────────────

/// (bind ((x 10) (y 20)) (int-add x y)) → 30
///
/// Binding pair convention: CallNode { callee: dummy, args: [NameNode name, value_expr] }
/// Bindings list convention: CallNode { callee: dummy, args: [pair1, pair2, ...] }
#[test]
fn test_bind() {
    let mut b = GraphBuilder::new();

    // binding pair (x 10): args=[NameNode("x"), LiteralNode(10)]
    let pair_callee_x = b.name("__pair__");
    let name_x = b.name("x");
    let val_10 = b.literal(lit_int(10));
    let pair_x = b.call(pair_callee_x, vec![name_x, val_10]);

    let pair_callee_y = b.name("__pair__");
    let name_y = b.name("y");
    let val_20 = b.literal(lit_int(20));
    let pair_y = b.call(pair_callee_y, vec![name_y, val_20]);

    // bindings list
    let dummy_callee = b.name("__bindings__");
    let bindings = b.call(dummy_callee, vec![pair_x, pair_y]);

    // body: (int-add x y)
    let add_fn = b.name("int_add");
    let ref_x = b.name("x");
    let ref_y = b.name("y");
    let body = b.call(add_fn, vec![ref_x, ref_y]);

    let bind_fn = b.name("bind");
    let bind_call = b.call(bind_fn, vec![bindings, body]);

    assert_eq!(eval_one(&mut b, bind_call), RuntimeValue::Int(30));
}

#[test]
fn test_surface_multi_bind_evaluates_all_pairs() {
    let graph = parse("(bind ((x 10) (y 20)) (int_add x y))").unwrap();
    let mut ev = Evaluator::new(graph);

    assert_eq!(ev.run().unwrap(), RuntimeValue::Int(30));
}

#[test]
fn test_builtin_name_can_be_captured_as_value() {
    let graph = parse(
        "(bind get_ref get
          (apply get_ref (list_of (map_of \"answer\" 42) \"answer\")))",
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);

    assert_eq!(ev.run().unwrap(), RuntimeValue::Int(42));
}

#[test]
fn test_lexical_binding_shadows_builtin_in_call_position() {
    let graph = parse(
        "(bind int_add
          (lambda (left right) (int_mul left right))
          (int_add 6 7))",
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);

    assert_eq!(ev.run().unwrap(), RuntimeValue::Int(42));
}

// ── block + leave ─────────────────────────────────────────────────────────────

/// (block (leave <block-id> 99) (int-add 1 1)) → 99
/// The leave target is the block call's NodeId.
#[test]
fn test_block_leave() {
    let mut b = GraphBuilder::new();

    // We need to know the block's NodeId before creating the leave node.
    // Pre-allocate the block id.
    let block_id = b.graph.allocate_id().unwrap();

    // leave: (leave block_id 99)
    let leave_fn = b.name("leave");
    let target_lit = b.literal(IrLiteralData::Int(block_id as i64));
    let value_99 = b.literal(lit_int(99));
    let leave_call = b.call(leave_fn, vec![target_lit, value_99]);

    // unreachable body after leave
    let unreachable = b.literal(lit_int(0));

    // Manually insert the block node with the pre-allocated id.
    let block_fn = b.name("block");
    let block_fn_call_node = caap_core::ir::CallNode {
        id: block_id,
        callee: block_fn,
        args: vec![leave_call, unreachable].into(),
    };
    b.graph
        .set_node(caap_core::ir::Node::Call(block_fn_call_node), None)
        .unwrap();

    assert_eq!(eval_one(&mut b, block_id), RuntimeValue::Int(99));
}

// ── while ─────────────────────────────────────────────────────────────────────

#[test]
fn test_while_with_assignment_updates_lexical_bindings() {
    let value = eval_source(
        "(do
          (bind ((sum 0) (i 0))
            (while (lt i 5)
              (do
                (set! sum (int_add sum i))
                (set! i (int_add i 1))))
            sum))",
    )
    .expect("while with set! should evaluate");

    assert_eq!(value, RuntimeValue::Int(10));
}

#[test]
fn test_while_false_returns_null() {
    let mut b = GraphBuilder::new();
    let while_fn = b.name("while");
    let cond = b.literal(lit_bool(false));
    let body = b.literal(lit_int(99));
    let call_id = b.call(while_fn, vec![cond, body]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Null);
}

// ── environment lookup ────────────────────────────────────────────────────────

#[test]
fn test_env_lookup_bound_name() {
    let mut b = GraphBuilder::new();
    let name_id = b.name("answer");
    let graph = std::mem::take(&mut b.graph);
    let env = Environment::new(None);
    Environment::define(&env, "answer", RuntimeValue::Int(42));
    let mut ev = Evaluator::new(graph);
    assert_eq!(ev.eval(name_id, &env).unwrap(), RuntimeValue::Int(42));
}

#[test]
fn test_env_lookup_unknown_name_errors() {
    let mut b = GraphBuilder::new();
    let name_id = b.name("unknown");
    let graph = std::mem::take(&mut b.graph);
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);
    assert!(ev.eval(name_id, &env).is_err());
}

// ── reflect / type inspection ─────────────────────────────────────────────────

#[test]
fn test_value_type_int() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value_type");
    let v = b.literal(lit_int(42));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("int".into()));
}

#[test]
fn test_value_type_bool_is_distinct_from_int() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value_type");
    let v = b.literal(lit_bool(true));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("bool".into()));
}

#[test]
fn test_value_type_null() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value_type");
    let v = b.literal(lit_null());
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("null".into()));
}

#[test]
fn test_value_type_string() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value_type");
    let v = b.literal(lit_str("hi"));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(
        eval_one(&mut b, call_id),
        RuntimeValue::Str("string".into())
    );
}

#[test]
fn test_value_type_tuple() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value_type");
    let tuple = b.literal(IrLiteralData::Tuple(vec![IrLiteralData::Int(1)]));
    let call_id = b.call(fn_node, vec![tuple]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("tuple".into()));
}

#[test]
fn test_value_type_has_no_runtime_error_value() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("value_type");
    let v = b.literal(lit_null());
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("null".into()));
}

#[test]
fn test_host_value_kind_int() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("host_value_kind");
    let v = b.literal(lit_int(5));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("int".into()));
}

#[test]
fn test_host_value_kind_tuple() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("host_value_kind");
    let tuple = b.literal(IrLiteralData::Tuple(vec![]));
    let call_id = b.call(fn_node, vec![tuple]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("tuple".into()));
}

#[test]
fn test_host_value_kind_null() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("host_value_kind");
    let v = b.literal(lit_null());
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("null".into()));
}

#[test]
fn test_runtime_error() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("runtime_error");
    let msg = b.literal(lit_str("oops"));
    let call_id = b.call(fn_node, vec![msg]);
    let graph = std::mem::take(&mut b.graph);
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);
    assert!(ev.eval(call_id, &env).is_err());
}

#[test]
fn test_evaluator_rejects_excessive_recursion_depth() {
    // Non-tail (the int_add keeps a frame live): tail self-calls run at
    // constant depth and would never hit the limit.
    let graph = parse("(bind ((loop (lambda (x) (int_add 1 (loop x))))) (loop 0))")
        .expect("recursive source should parse");
    let mut ev = Evaluator::new(graph);
    ev.set_max_eval_depth(64);

    let err = ev
        .run()
        .expect_err("recursive source should hit depth limit");

    assert!(err
        .to_string()
        .contains("maximum evaluation depth 64 exceeded"));
}

#[test]
fn test_evaluator_enforces_runtime_collection_limit() {
    let mut ev = Evaluator::new(parse("(list_of 1 2)").unwrap());
    ev.set_runtime_collection_limit(1);
    let err = ev.run().expect_err("list-of should enforce quota");
    assert!(err.to_string().contains("list size limit 1 exceeded"));

    let mut ev = Evaluator::new(parse("(map_of \"a\" 1 \"b\" 2)").unwrap());
    ev.set_runtime_collection_limit(1);
    let err = ev.run().expect_err("map-of should enforce quota");
    assert!(err.to_string().contains("map size limit 1 exceeded"));

    let mut ev = Evaluator::new(parse("(bind ((xs (list_of 1))) (append xs 2))").unwrap());
    ev.set_runtime_collection_limit(1);
    let err = ev.run().expect_err("append should enforce quota");
    assert!(err.to_string().contains("list size limit 1 exceeded"));

    let mut ev = Evaluator::new(parse("(sequence_range 0 2)").unwrap());
    ev.set_runtime_collection_limit(1);
    let err = ev.run().expect_err("sequence_range should enforce quota");
    assert!(err
        .to_string()
        .contains("sequence_range: list size limit 1 exceeded"));

    let mut ev =
        Evaluator::new(parse("(sequence_flatten (list_of (list_of 1 2) (list_of 3 4)))").unwrap());
    ev.set_runtime_collection_limit(3);
    let err = ev.run().expect_err("sequence_flatten should enforce quota");
    assert!(err
        .to_string()
        .contains("sequence_flatten: list size limit 3 exceeded"));

    let mut ev = Evaluator::new(parse(r#"(map_merge (map_of "a" 1) (map_of "b" 2))"#).unwrap());
    ev.set_runtime_collection_limit(1);
    let err = ev.run().expect_err("map-merge should enforce quota");
    assert!(err
        .to_string()
        .contains("map_merge: map size limit 1 exceeded"));
}

#[test]
fn test_evaluator_enforces_runtime_string_limit_for_amplifying_builtins() {
    let mut ev = Evaluator::new(parse(r#"(string_repeat "ab" 3)"#).unwrap());
    ev.set_runtime_collection_limit(5);
    let err = ev.run().expect_err("string_repeat should enforce quota");
    assert!(err
        .to_string()
        .contains("string_repeat: string size limit 5 exceeded"));

    let mut ev = Evaluator::new(parse(r#"(string_concat_many "ab" "cd")"#).unwrap());
    ev.set_runtime_collection_limit(3);
    let err = ev
        .run()
        .expect_err("string_concat_many should enforce quota");
    assert!(err
        .to_string()
        .contains("string_concat_many: string size limit 3 exceeded"));

    let mut ev = Evaluator::new(parse(r#"(string_replace "aaaa" "a" "bb")"#).unwrap());
    ev.set_runtime_collection_limit(7);
    let err = ev.run().expect_err("string-replace should enforce quota");
    assert!(err
        .to_string()
        .contains("string_replace: string size limit 7 exceeded"));

    let mut ev = Evaluator::new(parse(r#"(sequence_join (list_of "ab" "cd") "-")"#).unwrap());
    ev.set_runtime_collection_limit(4);
    let err = ev.run().expect_err("sequence-join should enforce quota");
    assert!(err
        .to_string()
        .contains("sequence_join: string size limit 4 exceeded"));
}

#[test]
fn test_cross_graph_closure_preserves_runtime_collection_limit() {
    let mut closure_ev = Evaluator::new(parse("(lambda (x) (list_of x x))").unwrap());
    let closure = closure_ev.run().expect("closure source should evaluate");

    let graph = parse("(foreign 1)").expect("call source should parse");
    let root_id = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    Environment::define(&env, "foreign", closure);
    let mut ev = Evaluator::new(graph);
    ev.set_runtime_collection_limit(1);

    let err = ev
        .eval(root_id, &env)
        .expect_err("cross-graph closure should inherit caller collection limit");
    assert!(
        err.to_string().contains("list size limit 1 exceeded"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_runtime_error_carries_call_frame() {
    let source = "(runtime_error \"oops\")";
    let graph = parse(source).expect("parse failed");
    let call_id = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);

    let err = ev.eval(call_id, &env).expect_err("expected runtime error");
    match err {
        caap_core::EvalSignal::Error(error) => {
            assert_eq!(error.message(), "oops");
            let frames = error.frames();
            assert_eq!(frames.len(), 1);
            assert_eq!(frames[0].node_id, call_id);
            assert_eq!(frames[0].name.as_deref(), Some("runtime_error"));
            assert!(frames[0].span.is_some());
            let displayed = error.to_string();
            assert!(displayed.contains("Runtime frames:"));
            assert!(displayed.contains("runtime_error"));
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn test_eval_signal_exposes_inner_error_source() {
    let signal = caap_core::EvalSignal::Error(caap_core::EvaluationError::new("boom"));
    let source = std::error::Error::source(&signal).expect("inner error source");
    assert_eq!(source.to_string(), "EvaluationError: boom");
}

#[test]
fn test_runtime_error_accumulates_nested_frames() {
    let source = "(int_add missing 1)";
    let graph = parse(source).expect("parse failed");
    let call_id = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);

    let err = ev
        .eval(call_id, &env)
        .expect_err("expected unknown-name error");
    match err {
        caap_core::EvalSignal::Error(error) => {
            let names: Vec<Option<&str>> = error
                .frames()
                .iter()
                .map(|frame| frame.name.as_deref())
                .collect();
            assert_eq!(names, vec![Some("missing"), Some("int_add")]);
        }
        other => panic!("expected error signal, got {other:?}"),
    }
}

#[test]
fn test_diagnostic_from_runtime_error_renders_source_and_stack() {
    let source = "(runtime_error \"oops\")";
    let graph = parse(source).expect("parse failed");
    let call_id = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);
    let err = ev.eval(call_id, &env).expect_err("expected runtime error");
    let caap_core::EvalSignal::Error(error) = err else {
        panic!("expected error signal");
    };

    let diagnostic = caap_core::Diagnostic::from_evaluation_error(&error);
    assert_eq!(diagnostic.code.as_deref(), Some("CAAP-RUNTIME-001"));
    assert_eq!(diagnostic.message, "oops");
    assert_eq!(diagnostic.stack_trace.len(), 1);

    let rendered = caap_core::render_diagnostic(&diagnostic, Some(source));
    assert!(rendered.contains("error[CAAP-RUNTIME-001]: oops"));
    assert!(rendered.contains("--> <input>:1:1"));
    assert!(rendered.contains("stack trace:"));
    assert!(rendered.contains("runtime_error"));
}
#[test]
fn test_apply() {
    // (apply (lambda (x y) (int-add x y)) (list-of 3 4)) → 7
    let mut b = GraphBuilder::new();
    let params_callee = b.name("__params__");
    let px = b.name("x");
    let py = b.name("y");
    let params = b.call(params_callee, vec![px, py]);
    let add_fn = b.name("int_add");
    let rx = b.name("x");
    let ry = b.name("y");
    let body = b.call(add_fn, vec![rx, ry]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);
    let list_fn = b.name("list_of");
    let three = b.literal(lit_int(3));
    let four = b.literal(lit_int(4));
    let lst = b.call(list_fn, vec![three, four]);
    let apply_fn = b.name("apply");
    let call_id = b.call(apply_fn, vec![lam, lst]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(7));
}

#[test]
fn test_apply_accepts_tuple_rest_arg() {
    let mut b = GraphBuilder::new();

    let params_callee = b.name("__params__");
    let px = b.name("x");
    let py = b.name("y");
    let params = b.call(params_callee, vec![px, py]);
    let add_fn = b.name("int_add");
    let rx = b.name("x");
    let ry = b.name("y");
    let body = b.call(add_fn, vec![rx, ry]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);

    let tuple = b.literal(IrLiteralData::Tuple(vec![
        IrLiteralData::Int(2),
        IrLiteralData::Int(3),
    ]));
    let apply_fn = b.name("apply");
    let call_id = b.call(apply_fn, vec![lam, tuple]);

    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(5));
}

#[test]
fn test_lt() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("lt");
    let a = b.literal(lit_int(3));
    let c = b.literal(lit_int(7));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_lt_uses_canonical_mixed_numeric_comparison() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("lt");
    let a = b.literal(lit_int(3));
    let c = b.literal(IrLiteralData::Float(3.5));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_le_equal() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("le");
    let a = b.literal(lit_int(5));
    let c = b.literal(lit_int(5));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_gensym_unique() {
    let mut b = GraphBuilder::new();
    let fn1 = b.name("gensym");
    let call1 = b.call(fn1, vec![]);
    let fn2 = b.name("gensym");
    let call2 = b.call(fn2, vec![]);
    let graph = std::mem::take(&mut b.graph);
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);
    let v1 = ev.eval(call1, &env).unwrap();
    let v2 = ev.eval(call2, &env).unwrap();
    assert_ne!(v1, v2);
}

/// ctfe_kernel_vocabulary exposes the kernel's callable vocabulary as data
/// (names, arity, purity/effects) for in-language compile-time tooling.
#[test]
fn test_ctfe_kernel_vocabulary_describes_builtins() {
    let graph = parse(
        "(bind ((v (ctfe_kernel_vocabulary)))
           (list_of
             (get (get v \"int_add\" null) \"min_arity\" null)
             (get (get v \"int_add\" null) \"pure\" null)
             (get (get v \"append\" null) \"pure\" null)
             (get (get (get v \"append\" null) \"effects\" null) 0 null)
             (get (get v \"if\" null) \"kind\" null)
             (get (get v \"lambda\" null) \"kind\" null)))",
    )
    .unwrap();
    let mut ev = caap_core::Evaluator::with_phase(graph, caap_core::PhasePolicy::CompileTime);
    let v = ev.run().expect("vocabulary evaluates");
    let RuntimeValue::List(items) = v else {
        panic!("expected list, got {v:?}")
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(2), "int_add min arity");
    assert_eq!(items[1], RuntimeValue::Bool(true), "int_add pure");
    assert_eq!(items[2], RuntimeValue::Bool(false), "append impure");
    assert_eq!(
        items[3],
        RuntimeValue::Str("mutation".into()),
        "append effect"
    );
    assert_eq!(
        items[4],
        RuntimeValue::Str("special".into()),
        "if is special"
    );
    assert_eq!(
        items[5],
        RuntimeValue::Str("special".into()),
        "lambda listed"
    );
}

/// `()` IS null in CAAP — so a null params node means "zero parameters" on
/// BOTH kernel paths. The eval-level contract (extract_param_names) always
/// accepted LiteralNode(Null); the frontend lowering used to reject the same
/// shape, breaking post-parse producers (the stdlib expander emits it for
/// zero-param defn/struct).
#[test]
fn null_lambda_params_mean_zero_parameters() {
    assert_eq!(
        eval_source("((lambda null 42))").expect("null params"),
        RuntimeValue::Int(42)
    );
    assert_eq!(
        eval_source("((lambda () 42))").expect("empty params"),
        RuntimeValue::Int(42)
    );
    // Arity is still enforced: a null-params lambda takes no arguments.
    assert!(eval_source("((lambda null 1) 2)").is_err());
}

/// Float literal patterns use exact IEEE equality (regression for U4).
///
/// The previous absolute-epsilon tolerance (`< f64::EPSILON`) was too tight at
/// large magnitudes — `1e10` would fail to match the literal `1e10`. Exact
/// equality matches it. It is also too loose near zero — `0.3` must NOT match
/// `0.1 + 0.2` (= 0.30000000000000004), which exact equality correctly rejects.
#[test]
fn float_literal_pattern_uses_exact_equality() {
    // Large magnitude: exact match succeeds where an absolute epsilon failed.
    assert_eq!(
        eval_source("(match 1e10 (1e10 \"hit\") (else \"miss\"))").expect("large float match"),
        RuntimeValue::Str("hit".into())
    );

    // Floating-point error near a literal must NOT match: 0.1 + 0.2 != 0.3.
    assert_eq!(
        eval_source("(match (float_add 0.1 0.2) (0.3 \"hit\") (else \"miss\"))")
            .expect("near-zero float match"),
        RuntimeValue::Str("miss".into())
    );

    // Exact float still matches itself.
    assert_eq!(
        eval_source("(match 0.5 (0.5 \"hit\") (else \"miss\"))").expect("exact float match"),
        RuntimeValue::Str("hit".into())
    );
}
