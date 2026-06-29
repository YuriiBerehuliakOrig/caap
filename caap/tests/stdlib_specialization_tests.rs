//! End-to-end proof of the `pe` polyvariant binding-time specialization pass on
//! the REAL loader path. The pass is registered by the opt-in leg
//! `stdlib/boot/pe.caap`; once composed, every module loaded afterwards is run
//! through the load-time `pe` transform (which redirects annotated literal-static
//! calls to specialized variants, partially evaluates them, materializes the
//! variants into the module IR, and DCEs the fully-specialized originals) BEFORE
//! the semantic checker / type pass and BEFORE evaluation.
//!
//! These tests prove the specialize -> run loop closes through the loader:
//!   (a) a `(static_params …)`-annotated module LOADS (no "unknown name", no
//!       checker rejection of the emitted variant binders),
//!   (b) the specialized variants are MATERIALIZED into the loaded module IR
//!       (observed by a spy transform ordered after `pe`), and
//!   (c) calling the loaded function produces the CORRECT runtime result.
use caap_core::{frontend::parse, PhasePolicy, RuntimeValue, Unit};

mod common;

use common::{corpus_path, session, stdlib_bootstrap, stdlib_path};

/// Compose `bootstrap.caap` + `boot/pe.caap` on a fresh session, declare the
/// fixtures/stdlib roots, then evaluate `body` (which has `compiler` and the
/// loader API `load`/`load_module`/`declare_root` in scope).
fn eval_pe(name: &str, body: &str) -> Result<RuntimeValue, String> {
    let mut compiler = session();
    let bootstrap = stdlib_bootstrap();
    let pe = stdlib_path("boot/pe.caap");
    let base = stdlib_path("");
    let fixtures = corpus_path("fixtures");
    let src = format!(
        "(do
           (ctfe_compiler_execute_bootstrap_file compiler {bootstrap:?})
           (ctfe_compiler_execute_bootstrap_file compiler {pe:?})
           (bind (
             (api          (ctfe_compiler_lookup_value compiler \"stdlib.load\"))
             (load         (get api \"load\" null))
             (load_module  (get api \"load_module\" null))
             (declare_root (get api \"declare_root\" null))
           )
             (do
               (declare_root \"stdlib.fixtures\" {fixtures:?})
               (declare_root \"stdlib\" {base:?})
               {body})))"
    );
    let graph = parse(&src).expect("parse");
    let unit = Unit::from_graph(name, graph).expect("unit");
    compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .map_err(|s| s.to_string())
}

fn eval_pe_ok(name: &str, body: &str) -> RuntimeValue {
    eval_pe(name, body).expect("compile-time evaluation failed")
}

/// (a) + (c): the annotated source module LOADS through the full loader path
/// (expand -> pe transform -> check gate -> type pass -> eval), and calling the
/// exported `compute` gives the specialized-but-equivalent result.
///
/// compute(10) = scale(1,10) + scale(3,10) = 10 + 30 = 40. After pe the body is
/// (int_add (scale__i_1 y) (scale__i_3 y)) with scale__i_1 = (lambda (x) x) and
/// scale__i_3 = (lambda (x) (int_mul 3 x)) — the runtime answer is unchanged.
#[test]
fn pe_annotated_module_loads_and_runs_correctly() {
    let v = eval_pe_ok(
        "pe_scale_run",
        "(bind ((m (load_module \"stdlib.fixtures.pe_scale\")))
           ((get m \"compute\" null) 10))",
    );
    assert_eq!(
        v,
        RuntimeValue::Int(40),
        "compute(10) = scale(1,10)+scale(3,10) = 10+30 = 40, specialized"
    );
}

/// A second static value to confirm the result tracks the specialization, not a
/// constant: compute(7) = 7 + 21 = 28.
#[test]
fn pe_specialized_result_tracks_argument() {
    let v = eval_pe_ok(
        "pe_scale_run7",
        "(bind ((m (load_module \"stdlib.fixtures.pe_scale\")))
           ((get m \"compute\" null) 7))",
    );
    assert_eq!(v, RuntimeValue::Int(28), "compute(7) = 7 + 21 = 28");
}

/// (b): the specialized variants are MATERIALIZED into the loaded module's IR.
/// A spy transform is registered to run AFTER `pe` (ordered via a dep on it); it
/// records, for the fixture module, the name bound by every top-level
/// `(bind <name> (lambda …))` form it sees (pe emits each variant as such a
/// NAME-node binder; a surviving `defn` def is the same bind under a
/// `(do <sig-marker> …)` wrapper, which the spy looks through). We then assert
/// scale__i_1 and scale__i_3 are present, the fully-specialized original `scale` was
/// DCE'd out of the module IR, and the caller `compute` survives.
#[test]
fn pe_materializes_variants_into_module_ir() {
    let v = eval_pe_ok(
        "pe_scale_ir",
        "(bind (
           (reg   (ctfe_compiler_lookup_value compiler \"stdlib.semantics.passes.registry\"))
           (ast   (ctfe_compiler_lookup_value compiler \"stdlib.syntax.ast\"))
           (instw (get reg \"install_transform_with!\" null))
           (call?     (get ast \"call?\" null))
           (head_of   (get ast \"head_of\" null))
           (head_is?  (get ast \"head_is?\" null))
           (args_of   (get ast \"args_of\" null))
           (arg       (get ast \"arg\" null))
           (name_of   (get ast \"name_of\" null))
           (seen (list_of))
         )
           (do
             ; spy: ordered AFTER pe, so it sees pe's emitted variant binders.
             ; pe emits each variant as a NAME-node binder `(bind <name> (lambda …))`
             ; (the kernel's evaluable top-level-definition shape); a surviving
             ; `defn` def is `(do <sig-marker> (bind <name> (lambda …)))`. We record
             ; the defined name for BOTH — i.e. every top-level definition name in
             ; the post-pe module IR. NOTE: this body is raw kernel (not expanded),
             ; so it uses kernel primitives only — sequence_each / if, no for/when.
             (instw \"pe_spy\"
               (lambda (located)
                 (sequence_each located
                   (lambda (f)
                     (bind (
                       (n (get f \"node\" null))
                       ; look through a `(do <marker> (bind …))` defn wrapper
                       (b (if (head_is? n \"do\")
                            (arg n (int_sub (size (args_of n)) 1))
                            n))
                     )
                       (if (call? b)
                         (if (eq (head_of b) \"bind\")
                           (append seen (name_of (arg b 0)))
                           null)
                         null)))))
               (assoc (map_of) \"after\" (list_of \"pe\")))
             ; load through the full path; the spy fills `seen`
             (bind ((m (load_module \"stdlib.fixtures.pe_scale\")))
               (do
                 ; sanity: the module still evaluates correctly
                 (bind ((r ((get m \"compute\" null) 10)))
                   (list_of
                     r
                     ; was scale__i_1 materialized? (static int 1 -> key \"i_1\")
                     (sequence_any seen (lambda (s) (eq s \"scale__i_1\")))
                     ; was scale__i_3 materialized? (static int 3 -> key \"i_3\")
                     (sequence_any seen (lambda (s) (eq s \"scale__i_3\")))
                     ; was the fully-specialized original `scale` DCE'd?
                     (not (sequence_any seen (lambda (s) (eq s \"scale\"))))
                     ; the surviving caller `compute` is still defined
                     (sequence_any seen (lambda (s) (eq s \"compute\")))))))))",
    );
    let RuntimeValue::List(items) = v else {
        panic!("expected list, got {v:?}")
    };
    let items = items.borrow();
    assert_eq!(items[0], RuntimeValue::Int(40), "module still computes 40");
    assert_eq!(
        items[1],
        RuntimeValue::Bool(true),
        "scale__i_1 variant materialized into the module IR"
    );
    assert_eq!(
        items[2],
        RuntimeValue::Bool(true),
        "scale__i_3 variant materialized into the module IR"
    );
    assert_eq!(
        items[3],
        RuntimeValue::Bool(true),
        "the fully-specialized original `scale` was DCE'd"
    );
    assert_eq!(
        items[4],
        RuntimeValue::Bool(true),
        "the surviving caller `compute` is still defined"
    );
}

/// Without the `pe` leg the module fails to load: `(static_params …)` is not a
/// registered form, so it reaches the checker as an unknown name — confirming
/// the loop is genuinely opt-in and the form only exists once pe loads.
#[test]
fn pe_form_is_opt_in() {
    let mut compiler = session();
    let bootstrap = stdlib_bootstrap();
    let base = stdlib_path("");
    let fixtures = corpus_path("fixtures");
    let src = format!(
        "(do
           (ctfe_compiler_execute_bootstrap_file compiler {bootstrap:?})
           (bind (
             (api          (ctfe_compiler_lookup_value compiler \"stdlib.load\"))
             (load_module  (get api \"load_module\" null))
             (declare_root (get api \"declare_root\" null))
           )
             (do
               (declare_root \"stdlib.fixtures\" {fixtures:?})
               (declare_root \"stdlib\" {base:?})
               (load_module \"stdlib.fixtures.pe_scale\"))))"
    );
    let graph = parse(&src).expect("parse");
    let unit = Unit::from_graph("pe_optin", graph).expect("unit");
    let err = compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect_err("module must not load without the pe leg");
    let msg = err.to_string();
    assert!(
        msg.contains("static_params") || msg.contains("failed load-time checks"),
        "without pe, (static_params …) is unknown: {msg}"
    );
}
