//! `ctfe_debug_frames` — the live closure-call stack as data, STRICTLY
//! diagnostics-class: pure effect scopes cannot observe it (alpha-renaming
//! must stay unobservable in semantics), and the fold engine never folds it.

use caap_core::{frontend::parse, Evaluator, PhasePolicy, RuntimeValue};

fn eval_compile_time(src: &str) -> Result<RuntimeValue, String> {
    let graph = parse(src).unwrap();
    Evaluator::with_phase(graph, PhasePolicy::CompileTime)
        .run()
        .map_err(|e| e.to_string())
}

#[test]
fn frames_list_named_calls_outermost_first_with_spans() {
    let src = r#"(bind inner (lambda () (ctfe_debug_frames))
                   (bind outer (lambda () (inner))
                     (outer)))"#;
    let RuntimeValue::List(frames) = eval_compile_time(src).expect("frames") else {
        panic!("expected a list of frames");
    };
    let frames = frames.borrow();
    assert_eq!(frames.len(), 2, "outer + inner");
    let name_of = |frame: &RuntimeValue| -> String {
        let RuntimeValue::Map(fields) = frame else {
            panic!("frame must be a map");
        };
        let fields = fields.borrow();
        let Some(RuntimeValue::Str(name)) =
            fields.get(&caap_core::values::MapKey::Str("name".into()))
        else {
            panic!("frame must carry a name");
        };
        name.to_string()
    };
    assert_eq!(name_of(&frames[0]), "outer", "outermost first");
    assert_eq!(name_of(&frames[1]), "inner");
    let RuntimeValue::Map(fields) = &frames[1] else {
        panic!("frame must be a map");
    };
    let fields = fields.borrow();
    assert!(
        matches!(
            fields.get(&caap_core::values::MapKey::Str("span".into())),
            Some(RuntimeValue::Map(_))
        ),
        "the call site of a parsed program carries a span"
    );
}

#[test]
fn top_level_call_has_no_frames() {
    let RuntimeValue::List(frames) = eval_compile_time("(ctfe_debug_frames)").expect("frames")
    else {
        panic!("expected a list");
    };
    assert!(frames.borrow().is_empty(), "no closure frames at top level");
}

#[test]
fn pure_effect_scopes_cannot_observe_frames() {
    // The impure marking is the semantic firewall: code running under a pure
    // effect scope must not be able to branch on frame names.
    let err = eval_compile_time(r#"(effect_scope (list_of) (ctfe_debug_frames))"#)
        .expect_err("a pure scope must reject the diagnostics builtin");
    assert!(
        err.contains("effect") || err.contains("pure"),
        "expected an effect-policy rejection, got: {err}"
    );
}

#[test]
fn a_tco_loop_is_one_collapsed_frame() {
    // The trampoline reuses the frame, so the diagnostic stack must not grow
    // with tail iterations — frames reflect live frames, not call history.
    let src = r#"(bind probe (lambda (n)
                    (if (eq n 0) (size (ctfe_debug_frames)) (probe (int_sub n 1))))
                   (probe 10000))"#;
    assert_eq!(
        eval_compile_time(src).expect("tco frames"),
        RuntimeValue::Int(1),
        "one frame for the whole tail loop"
    );
}
