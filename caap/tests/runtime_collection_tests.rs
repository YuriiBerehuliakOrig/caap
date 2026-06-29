/// Integration tests for runtime collection, string, and sequence builtins.
///
/// These scenarios stay separate from evaluator control-flow tests so collection
/// behavior remains independently runnable and fast.
use caap_core::{
    frontend::parse,
    graph::GraphBuilder,
    ir::{IrLiteralData, NodeId},
    values::{Environment, EvalSignal},
    Evaluator, RuntimeValue,
};

fn lit_int(v: i64) -> IrLiteralData {
    IrLiteralData::Int(v)
}

fn lit_null() -> IrLiteralData {
    IrLiteralData::Null
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
// ── mutable collections ───────────────────────────────────────────────────────

#[test]
fn test_list_of_empty() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("list_of");
    let call_id = b.call(fn_node, vec![]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => assert!(l.borrow().is_empty()),
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_list_of_values() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("list_of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let d = b.literal(lit_int(3));
    let call_id = b.call(fn_node, vec![a, c, d]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow.len(), 3);
            assert_eq!(borrow[0], RuntimeValue::Int(1));
            assert_eq!(borrow[2], RuntimeValue::Int(3));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_append() {
    let mut b = GraphBuilder::new();
    // (append (list-of 1 2) 3) → [1, 2, 3]
    let list_fn = b.name("list_of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let list_call = b.call(list_fn, vec![a, c]);
    let append_fn = b.name("append");
    let three = b.literal(lit_int(3));
    let call_id = b.call(append_fn, vec![list_call, three]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow.len(), 3);
            assert_eq!(borrow[2], RuntimeValue::Int(3));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_map_of() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("map_of");
    let k = b.literal(IrLiteralData::Str("x".to_string()));
    let v = b.literal(lit_int(99));
    let call_id = b.call(fn_node, vec![k, v]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::Map(m) => {
            let borrow = m.borrow();
            assert_eq!(borrow.len(), 1);
            use caap_core::MapKey;
            let key = MapKey::Str("x".into());
            assert_eq!(borrow[&key], RuntimeValue::Int(99));
        }
        other => panic!("expected map, got {other}"),
    }
}

#[test]
fn test_map_keys_and_values_use_insertion_order() {
    let graph = parse(
        r#"(bind m (map_of "z" 5 null 1 2 3 false 2 "a" 4)
             (list_of (map_keys m) (map_values m)))"#,
    )
    .expect("parse failed");
    let root_id = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);

    match ev.eval(root_id, &env).expect("evaluation failed") {
        RuntimeValue::List(items) => {
            let items = items.borrow();
            assert_eq!(items.len(), 2);
            let RuntimeValue::List(keys) = &items[0] else {
                panic!("expected map-keys result list");
            };
            assert_eq!(
                keys.borrow().as_slice(),
                // Insertion order: exactly as map_of received them.
                &[
                    RuntimeValue::Str("z".into()),
                    RuntimeValue::Null,
                    RuntimeValue::Int(2),
                    RuntimeValue::Bool(false),
                    RuntimeValue::Str("a".into()),
                ]
            );
            let RuntimeValue::List(values) = &items[1] else {
                panic!("expected map-values result list");
            };
            assert_eq!(
                values.borrow().as_slice(),
                &[
                    RuntimeValue::Int(5),
                    RuntimeValue::Int(1),
                    RuntimeValue::Int(3),
                    RuntimeValue::Int(2),
                    RuntimeValue::Int(4),
                ]
            );
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_assoc() {
    let mut b = GraphBuilder::new();
    // (assoc (map-of) "key" 42)
    let map_fn = b.name("map_of");
    let empty_map = b.call(map_fn, vec![]);
    let assoc_fn = b.name("assoc");
    let k = b.literal(IrLiteralData::Str("key".to_string()));
    let v = b.literal(lit_int(42));
    let call_id = b.call(assoc_fn, vec![empty_map, k, v]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::Map(m) => {
            use caap_core::MapKey;
            assert_eq!(
                m.borrow()[&MapKey::Str("key".into())],
                RuntimeValue::Int(42)
            );
        }
        other => panic!("expected map, got {other}"),
    }
}

#[test]
fn test_set_list() {
    let mut b = GraphBuilder::new();
    // (set (list-of 0 0 0) 1 99) → [0, 99, 0]
    let list_fn = b.name("list_of");
    let z1 = b.literal(lit_int(0));
    let z2 = b.literal(lit_int(0));
    let z3 = b.literal(lit_int(0));
    let list = b.call(list_fn, vec![z1, z2, z3]);
    let set_fn = b.name("set");
    let idx = b.literal(lit_int(1));
    let val = b.literal(lit_int(99));
    let call_id = b.call(set_fn, vec![list, idx, val]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            assert_eq!(l.borrow()[1], RuntimeValue::Int(99));
        }
        other => panic!("expected list, got {other}"),
    }
}

// ── string builtins ───────────────────────────────────────────────────────────

fn lit_str(s: &str) -> IrLiteralData {
    IrLiteralData::Str(s.to_string())
}

#[test]
fn test_string_concat_many() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_concat_many");
    let a = b.literal(lit_str("foo"));
    let c = b.literal(lit_str("bar"));
    let call_id = b.call(fn_node, vec![a, c]);
    assert_eq!(
        eval_one(&mut b, call_id),
        RuntimeValue::Str("foobar".into())
    );
}

#[test]
fn test_string_split() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_split");
    let s = b.literal(lit_str("a,b,c"));
    let sep = b.literal(lit_str(","));
    let call_id = b.call(fn_node, vec![s, sep]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow.len(), 3);
            assert_eq!(borrow[0], RuntimeValue::Str("a".into()));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_value_eq_is_structural() {
    // The kernel `eq` is identity for lists/maps; `value_eq` is the structural
    // twin (stdlib `deep_eq` facade): contents, recursively; maps by key set.
    let graph = parse(
        "(list_of
          (eq (list_of 1 2) (list_of 1 2))
          (value_eq (list_of 1 2) (list_of 1 2))
          (value_eq (list_of 1 (list_of 2 3)) (list_of 1 (list_of 2 3)))
          (value_eq (map_of \"a\" 1 \"b\" (list_of 2)) (map_of \"b\" (list_of 2) \"a\" 1))
          (value_eq (map_of \"a\" 1) (map_of \"a\" 2))
          (value_eq 1 1.0)
          (value_eq \"x\" \"x\" \"x\")
          (value_eq (ref 1) (ref 1)))",
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);
    let RuntimeValue::List(items) = ev.run().unwrap() else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Bool(false), "eq stays identity");
    assert_eq!(items[1], RuntimeValue::Bool(true), "lists by contents");
    assert_eq!(items[2], RuntimeValue::Bool(true), "recursive");
    assert_eq!(items[3], RuntimeValue::Bool(true), "maps by key set");
    assert_eq!(items[4], RuntimeValue::Bool(false), "value mismatch");
    assert_eq!(items[5], RuntimeValue::Bool(false), "no numeric coercion");
    assert_eq!(items[6], RuntimeValue::Bool(true), "variadic chain");
    assert_eq!(
        items[7],
        RuntimeValue::Bool(false),
        "refs stay identity (mutable cells), like eq"
    );
}

#[test]
fn test_string_chars_lists_characters_in_one_call() {
    // O(n) char iteration; unicode-aware (chars, not bytes); empty → empty.
    let graph = parse(
        "(list_of
          (string_chars \"abc\")
          (string_chars \"п\u{456}ч\")
          (string_chars \"\"))",
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);

    let RuntimeValue::List(items) = ev.run().unwrap() else {
        panic!("expected list result");
    };
    let items = items.borrow();
    let RuntimeValue::List(abc) = &items[0] else {
        panic!("expected list");
    };
    let abc = abc.borrow();
    assert_eq!(abc.len(), 3);
    assert_eq!(abc[0], RuntimeValue::Str("a".into()));
    assert_eq!(abc[2], RuntimeValue::Str("c".into()));
    let RuntimeValue::List(cyr) = &items[1] else {
        panic!("expected list");
    };
    let cyr = cyr.borrow();
    assert_eq!(cyr.len(), 3);
    assert_eq!(cyr[1], RuntimeValue::Str("\u{456}".into()));
    let RuntimeValue::List(empty) = &items[2] else {
        panic!("expected list");
    };
    assert!(empty.borrow().is_empty());
}

#[test]
fn test_string_slice_accepts_null_end_and_negative_indexes() {
    let graph = parse(
        "(list_of
          (string_slice \"abcdef\" 2 null)
          (string_slice \"abcdef\" -3 null)
          (string_slice \"abcdef\" 4 2)
          (string_slice \"abcdef\" -9223372036854775808 null))",
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);

    let RuntimeValue::List(items) = ev.run().unwrap() else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("cdef".into()));
    assert_eq!(items[1], RuntimeValue::Str("def".into()));
    assert_eq!(items[2], RuntimeValue::Str("".into()));
    assert_eq!(items[3], RuntimeValue::Str("abcdef".into()));
}

#[test]
fn test_string_padding_via_repeat_and_concat() {
    // Padding is implemented in stdlib.string using string_repeat + string_concat_many.
    // Test the building blocks directly: pad "go" to width 4 using "λ" fill.
    let graph = parse(
        r#"(list_of
             (string_concat_many (string_repeat "λ" 2) "go")
             (string_concat_many "go" (string_repeat "λ" 2)))"#,
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);

    let RuntimeValue::List(items) = ev.run().unwrap() else {
        panic!("expected list result");
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Str("λλgo".into()));
    assert_eq!(items[1], RuntimeValue::Str("goλλ".into()));
}

#[test]
fn test_string_trim() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_trim");
    let s = b.literal(lit_str("  hello  "));
    let call_id = b.call(fn_node, vec![s]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("hello".into()));
}

#[test]
fn test_string_upcase() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_upcase");
    let s = b.literal(lit_str("hello"));
    let call_id = b.call(fn_node, vec![s]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("HELLO".into()));
}

#[test]
fn test_string_replace() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_replace");
    let s = b.literal(lit_str("hello world"));
    let old = b.literal(lit_str("world"));
    let new = b.literal(lit_str("rust"));
    let call_id = b.call(fn_node, vec![s, old, new]);
    assert_eq!(
        eval_one(&mut b, call_id),
        RuntimeValue::Str("hello rust".into())
    );
}

#[test]
fn test_string_contains_true() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_contains");
    let s = b.literal(lit_str("foobar"));
    let sub = b.literal(lit_str("oba"));
    let call_id = b.call(fn_node, vec![s, sub]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_string_find_uses_character_indexes_and_rejects_negative_start() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_find");
    let text = b.literal(lit_str("éaé"));
    let needle = b.literal(lit_str("é"));
    let start = b.literal(lit_int(1));
    let call_id = b.call(fn_node, vec![text, needle, start]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(2));

    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_find");
    let text = b.literal(lit_str("abc"));
    let needle = b.literal(lit_str("a"));
    let start = b.literal(lit_int(-1));
    let call_id = b.call(fn_node, vec![text, needle, start]);
    let error = eval_one_result(&mut b, call_id).expect_err("negative start should fail");
    let EvalSignal::Error(error) = error else {
        panic!("expected evaluation error");
    };
    assert!(error.message().contains("start index must be non-negative"));
}

#[test]
fn test_string_to_float_parses_decimal_strings() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_to_float");
    let text = b.literal(lit_str("1.5"));
    let call_id = b.call(fn_node, vec![text]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Float(1.5));
}

#[test]
fn test_string_to_float_parses_negative_and_scientific() {
    let graph = parse(r#"(string_to_float "-1.5e2")"#).unwrap();
    let mut ev = Evaluator::new(graph);
    assert_eq!(ev.run().unwrap(), RuntimeValue::Float(-150.0));
}

#[test]
fn test_string_find_uses_character_indexes() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_find");
    let text = b.literal(lit_str("éaé"));
    let needle = b.literal(lit_str("é"));
    let start = b.literal(lit_int(1));
    let call_id = b.call(fn_node, vec![text, needle, start]);
    // string-find returns the char index (2) or null; not-found returns null, not -1
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(2));
}

#[test]
fn test_string_starts_with() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_starts_with");
    let s = b.literal(lit_str("hello"));
    let prefix = b.literal(lit_str("hel"));
    let call_id = b.call(fn_node, vec![s, prefix]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_string_ends_with() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_ends_with");
    let s = b.literal(lit_str("hello"));
    let suffix = b.literal(lit_str("llo"));
    let call_id = b.call(fn_node, vec![s, suffix]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_string_to_int() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("string_to_int");
    let s = b.literal(lit_str("42"));
    let call_id = b.call(fn_node, vec![s]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(42));
}

#[test]
fn test_int_to_string() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("int_to_string");
    let v = b.literal(lit_int(123));
    let call_id = b.call(fn_node, vec![v]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Str("123".into()));
}

#[test]
fn test_stable_hash_is_deterministic_for_structured_values() {
    let source = r#"
        (do
          (bind first (stable_hash (map_of "left" 1 "right" (list_of true null)))
            (bind second (stable_hash (map_of "right" (list_of true null) "left" 1))
              (bind ordered (stable_hash (list_of "left" "right"))
                (bind reversed (stable_hash (list_of "right" "left"))
                  (list_of (eq first second) (eq ordered reversed) (size first)))))))"#;
    let graph = parse(source).expect("parse failed");
    let root_id = graph.top_level_form_ids()[0];
    let env = Environment::new(None);
    let mut ev = Evaluator::new(graph);
    match ev.eval(root_id, &env).expect("evaluation failed") {
        RuntimeValue::List(items) => {
            let items = items.borrow();
            assert_eq!(items[0], RuntimeValue::Bool(true));
            assert_eq!(items[1], RuntimeValue::Bool(false));
            assert_eq!(items[2], RuntimeValue::Int(32));
        }
        other => panic!("expected list, got {other}"),
    }
}

// ── sequence builtins ─────────────────────────────────────────────────────────

#[test]
fn test_sequence_range() {
    let mut b = GraphBuilder::new();
    let fn_node = b.name("sequence_range");
    let s = b.literal(lit_int(0));
    let e = b.literal(lit_int(5));
    let call_id = b.call(fn_node, vec![s, e]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow.len(), 5);
            assert_eq!(borrow[0], RuntimeValue::Int(0));
            assert_eq!(borrow[4], RuntimeValue::Int(4));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_map() {
    // (sequence-map (list-of 1 2 3) (lambda (x) (int-add x 10)))  → [11, 12, 13]
    let mut b = GraphBuilder::new();

    // list
    let list_fn = b.name("list_of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let d = b.literal(lit_int(3));
    let list = b.call(list_fn, vec![a, c, d]);

    // lambda: (lambda (x) (int-add x 10))
    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_x]);
    let add_fn = b.name("int_add");
    let body_x = b.name("x");
    let ten = b.literal(lit_int(10));
    let body = b.call(add_fn, vec![body_x, ten]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);

    let map_fn = b.name("sequence_map");
    let call_id = b.call(map_fn, vec![list, lam]);

    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow[0], RuntimeValue::Int(11));
            assert_eq!(borrow[2], RuntimeValue::Int(13));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_map_builtin_callback_uses_eager_dispatch() {
    let graph = parse("(sequence_map (list_of -3 4 -5) int_abs)").unwrap();
    let mut b = GraphBuilder { graph };
    let root_id = b.graph.root_id;

    match eval_one(&mut b, root_id) {
        RuntimeValue::List(items) => {
            let items = items.borrow();
            assert_eq!(
                items.as_slice(),
                &[
                    RuntimeValue::Int(3),
                    RuntimeValue::Int(4),
                    RuntimeValue::Int(5)
                ]
            );
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_filter() {
    // (sequence-filter (list-of 1 2 3 4 5) (lambda (x) (gt x 2))) -> [3, 4, 5]
    let mut b = GraphBuilder::new();

    let list_fn = b.name("list_of");
    let vals: Vec<u32> = (1..=5).map(|i| b.literal(lit_int(i as i64))).collect();
    let list = b.call(list_fn, vals);

    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_x]);
    let gt_fn = b.name("gt");
    let body_x = b.name("x");
    let two = b.literal(lit_int(2));
    let body = b.call(gt_fn, vec![body_x, two]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);

    let filter_fn = b.name("sequence_filter");
    let call_id = b.call(filter_fn, vec![list, lam]);

    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow.len(), 3);
            assert_eq!(borrow[0], RuntimeValue::Int(3));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_fold_left_sum() {
    // (sequence-fold-left (list-of 1 2 3 4 5) 0 (lambda (acc x) (int-add acc x))) → 15
    let mut b = GraphBuilder::new();

    let list_fn = b.name("list_of");
    let vals: Vec<u32> = (1..=5).map(|i| b.literal(lit_int(i as i64))).collect();
    let list = b.call(list_fn, vals);

    let params_callee = b.name("__params__");
    let param_acc = b.name("acc");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_acc, param_x]);
    let add_fn = b.name("int_add");
    let ref_acc = b.name("acc");
    let ref_x = b.name("x");
    let body = b.call(add_fn, vec![ref_acc, ref_x]);
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, body]);

    let fold_fn = b.name("sequence_fold_left");
    let zero = b.literal(lit_int(0));
    let call_id = b.call(fold_fn, vec![list, zero, lam]);

    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(15));
}

#[test]
fn test_sequence_reverse() {
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list_of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let d = b.literal(lit_int(3));
    let list = b.call(list_fn, vec![a, c, d]);
    let rev_fn = b.name("sequence_reverse");
    let call_id = b.call(rev_fn, vec![list]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow[0], RuntimeValue::Int(3));
            assert_eq!(borrow[2], RuntimeValue::Int(1));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_slice_accepts_null_end_and_negative_indexes() {
    let graph = parse(
        "(list_of
          (sequence_slice (list_of 1 2 3 4) 1 null)
          (sequence_slice (list_of 1 2 3 4) -2 null)
          (sequence_slice (list_of 1 2 3 4) 3 1))",
    )
    .unwrap();
    let mut ev = Evaluator::new(graph);

    let RuntimeValue::List(outer) = ev.run().unwrap() else {
        panic!("expected list result");
    };
    let outer = outer.borrow();
    let RuntimeValue::List(first) = &outer[0] else {
        panic!("expected nested list");
    };
    assert_eq!(
        first.borrow().as_slice(),
        &[
            RuntimeValue::Int(2),
            RuntimeValue::Int(3),
            RuntimeValue::Int(4)
        ]
    );
    let RuntimeValue::List(second) = &outer[1] else {
        panic!("expected nested list");
    };
    assert_eq!(
        second.borrow().as_slice(),
        &[RuntimeValue::Int(3), RuntimeValue::Int(4)]
    );
    let RuntimeValue::List(third) = &outer[2] else {
        panic!("expected nested list");
    };
    assert!(third.borrow().is_empty());
}

#[test]
fn test_sequence_join() {
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list_of");
    let a = b.literal(lit_str("a"));
    let c = b.literal(lit_str("b"));
    let d = b.literal(lit_str("c"));
    let list = b.call(list_fn, vec![a, c, d]);
    let join_fn = b.name("sequence_join");
    let sep = b.literal(lit_str(", "));
    let call_id = b.call(join_fn, vec![list, sep]);
    assert_eq!(
        eval_one(&mut b, call_id),
        RuntimeValue::Str("a, b, c".into())
    );
}

#[test]
fn test_size_list() {
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list_of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let list = b.call(list_fn, vec![a, c]);
    let size_fn = b.name("size");
    let call_id = b.call(size_fn, vec![list]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(2));
}

#[test]
fn test_get_list() {
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list_of");
    let a = b.literal(lit_int(10));
    let c = b.literal(lit_int(20));
    let list = b.call(list_fn, vec![a, c]);
    let get_fn = b.name("get");
    let idx = b.literal(lit_int(1));
    let call_id = b.call(get_fn, vec![list, idx]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(20));
}

#[test]
fn test_get_tuple() {
    let mut b = GraphBuilder::new();
    let tuple = b.literal(IrLiteralData::Tuple(vec![
        IrLiteralData::Int(10),
        IrLiteralData::Int(20),
    ]));
    let get_fn = b.name("get");
    let idx = b.literal(lit_int(1));
    let call_id = b.call(get_fn, vec![tuple, idx]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(20));
}

#[test]
fn test_get_indexed_containers_reject_negative_indexes() {
    for source in [
        "(get (list_of 10 20) -1 \"fallback\")",
        "(get \"ab\" -1 \"fallback\")",
    ] {
        let graph = parse(source).expect("parse failed");
        let root_id = graph.top_level_form_ids()[0];
        let env = Environment::new(None);
        let mut ev = Evaluator::new(graph);
        let err = ev
            .eval(root_id, &env)
            .expect_err("negative indexes should be semantic errors");
        assert!(
            err.to_string().contains("index must be non-negative"),
            "unexpected error: {err}"
        );
    }

    let mut b = GraphBuilder::new();
    let tuple = b.literal(IrLiteralData::Tuple(vec![
        IrLiteralData::Int(10),
        IrLiteralData::Int(20),
    ]));
    let idx = b.literal(lit_int(-1));
    let get_strict = b.name("get_strict");
    let call_id = b.call(get_strict, vec![tuple, idx]);
    let err = eval_one_result(&mut b, call_id)
        .expect_err("negative tuple index should be a semantic error");
    assert!(
        err.to_string().contains("index must be non-negative"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_size_tuple() {
    let mut b = GraphBuilder::new();
    let tuple = b.literal(IrLiteralData::Tuple(vec![
        IrLiteralData::Int(1),
        IrLiteralData::Int(2),
        IrLiteralData::Int(3),
    ]));
    let size_fn = b.name("size");
    let call_id = b.call(size_fn, vec![tuple]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Int(3));
}

#[test]
fn test_contains_list() {
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list_of");
    let a = b.literal(lit_int(1));
    let c = b.literal(lit_int(2));
    let d = b.literal(lit_int(3));
    let list = b.call(list_fn, vec![a, c, d]);
    let contains_fn = b.name("contains");
    let needle = b.literal(lit_int(2));
    let call_id = b.call(contains_fn, vec![list, needle]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Bool(true));
}

#[test]
fn test_for_range() {
    // (sequence-each (sequence_range 0 5) (lambda (i) null)) runs 5 iterations — returns null
    let mut b = GraphBuilder::new();
    let param_i = b.name("i");
    let params_callee = b.name("__params__");
    let params = b.call(params_callee, vec![param_i]);
    let null_body = b.literal(lit_null());
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, null_body]);
    let range_fn = b.name("sequence_range");
    let start = b.literal(lit_int(0));
    let end = b.literal(lit_int(5));
    let range_call = b.call(range_fn, vec![start, end]);
    let each_fn = b.name("sequence_each");
    let call_id = b.call(each_fn, vec![range_call, lam]);
    assert_eq!(eval_one(&mut b, call_id), RuntimeValue::Null);
}

// ── sequence-sort-by / group-by / zip / unique-by ─────────────────────────────

#[test]
fn test_sequence_sort_by() {
    // (sequence-sort-by (list-of 3 1 2) (lambda (x) x)) → [1, 2, 3]
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list_of");
    let three = b.literal(lit_int(3));
    let one = b.literal(lit_int(1));
    let two = b.literal(lit_int(2));
    let list = b.call(list_fn, vec![three, one, two]);
    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_x]);
    let identity = b.name("x");
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, identity]);
    let sort_fn = b.name("sequence_sort_by");
    let call_id = b.call(sort_fn, vec![list, lam]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => {
            let borrow = l.borrow();
            assert_eq!(borrow[0], RuntimeValue::Int(1));
            assert_eq!(borrow[1], RuntimeValue::Int(2));
            assert_eq!(borrow[2], RuntimeValue::Int(3));
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_zip() {
    // (sequence-zip (list-of 1 2 3) (list-of 4 5 6)) → [[1,4],[2,5],[3,6]]
    let mut b = GraphBuilder::new();
    let list_fn1 = b.name("list_of");
    let a1 = b.literal(lit_int(1));
    let b1 = b.literal(lit_int(2));
    let c1 = b.literal(lit_int(3));
    let list1 = b.call(list_fn1, vec![a1, b1, c1]);
    let list_fn2 = b.name("list_of");
    let a2 = b.literal(lit_int(4));
    let b2 = b.literal(lit_int(5));
    let c2 = b.literal(lit_int(6));
    let list2 = b.call(list_fn2, vec![a2, b2, c2]);
    let zip_fn = b.name("sequence_zip");
    let call_id = b.call(zip_fn, vec![list1, list2]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(outer) => {
            let borrow = outer.borrow();
            assert_eq!(borrow.len(), 3);
            match &borrow[0] {
                RuntimeValue::List(inner) => {
                    let ib = inner.borrow();
                    assert_eq!(ib[0], RuntimeValue::Int(1));
                    assert_eq!(ib[1], RuntimeValue::Int(4));
                }
                other => panic!("expected inner list, got {other}"),
            }
        }
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_sequence_unique_by() {
    // (sequence-unique-by (list-of 1 2 1 3 2) identity) → [1, 2, 3]
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list_of");
    let vals: Vec<u32> = [1, 2, 1, 3, 2]
        .iter()
        .map(|&i| b.literal(lit_int(i)))
        .collect();
    let list = b.call(list_fn, vals);
    let params_callee = b.name("__params__");
    let param_x = b.name("x");
    let params = b.call(params_callee, vec![param_x]);
    let identity = b.name("x");
    let lambda_fn = b.name("lambda");
    let lam = b.call(lambda_fn, vec![params, identity]);
    let uniq_fn = b.name("sequence_unique_by");
    let call_id = b.call(uniq_fn, vec![list, lam]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::List(l) => assert_eq!(l.borrow().len(), 3),
        other => panic!("expected list, got {other}"),
    }
}

#[test]
fn test_map_of_entries() {
    // (map-of-entries (list-of (list-of "a" 1) (list-of "b" 2))) → {a:1, b:2}
    let mut b = GraphBuilder::new();
    let list_fn = b.name("list_of");
    let inner1_fn = b.name("list_of");
    let k1 = b.literal(lit_str("a"));
    let v1 = b.literal(lit_int(1));
    let pair1 = b.call(inner1_fn, vec![k1, v1]);
    let inner2_fn = b.name("list_of");
    let k2 = b.literal(lit_str("b"));
    let v2 = b.literal(lit_int(2));
    let pair2 = b.call(inner2_fn, vec![k2, v2]);
    let entries = b.call(list_fn, vec![pair1, pair2]);
    let moe_fn = b.name("map_of_entries");
    let call_id = b.call(moe_fn, vec![entries]);
    match eval_one(&mut b, call_id) {
        RuntimeValue::Map(m) => {
            use caap_core::MapKey;
            let borrow = m.borrow();
            assert_eq!(borrow[&MapKey::Str("a".into())], RuntimeValue::Int(1));
            assert_eq!(borrow[&MapKey::Str("b".into())], RuntimeValue::Int(2));
        }
        other => panic!("expected map, got {other}"),
    }
}
