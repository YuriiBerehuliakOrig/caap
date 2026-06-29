//! Backend codegen INVARIANTS that are additive diagnostics, not value
//! changes: a raw program (bypassing the C-like surface, which rejects N<=0 at
//! parse) must NOT be able to smuggle a zero/negative array length into the
//! native backend, and `emit_with_target` must restore the PRIOR pinned target
//! (proper stacking) rather than always clearing to the host default.
//!
//! Audit refresh 2026-06-18, finding #3: the clike lowerer rejects `[N]` with
//! `N<=0` (`stdlib/frontend/clike.caap`), but the RAW backend `array_local` /
//! `global_array` paths only checked literal-int, not `N>0`. A raw `.caap`
//! program reaches the backend codegen site directly, so the `N>0` enforcement
//! must live there too.
use caap_core::RuntimeValue;

mod common;

use common::{eval_ok, with_stdlib_root};

/// Emit a raw `.caap` PROGRAM through stdlib's OWN LLVM backend (no clike
/// surface) and return the joined emit-diagnostic messages ("" when clean).
/// Unlike `emit_own_llvm` (in stdlib_codegen_tests.rs) this does NOT panic on
/// diagnostics — the whole point here is to assert the diagnostic text.
fn emit_diags(name: &str, prog: &str) -> String {
    let path = std::env::temp_dir().join(format!("s2_inv_{name}_{}.caap", std::process::id()));
    std::fs::write(&path, prog).expect("write program");
    let p = path.to_str().unwrap().to_string();
    let v = eval_ok(
        name,
        &with_stdlib_root(&format!(
            "(bind ((res
                ((get (load_module \"stdlib.backend.emit.llvm\")
                      \"emit_program\" null)
                 (ctfe_compiler_load_surface_file_template compiler {p:?}))))
               (sequence_join
                 (sequence_map (get res \"diagnostics\" (list_of))
                   (lambda (d) (get d \"message\" \"?\")))
                 \" | \"))"
        )),
    );
    std::fs::remove_file(&path).ok();
    match v {
        RuntimeValue::Str(s) => s.to_string(),
        other => panic!("expected joined diagnostics string, got {other:?}"),
    }
}

/// A raw program (NOT through the clike surface, which rejects N<=0 at parse)
/// with `(array_local 0 i32)` must surface the additive "must be a positive
/// integer" diagnostic from the native LLVM backend — the only enforcement
/// point a raw program reaches.
#[test]
fn stdlib_llvm_array_local_zero_length_is_rejected() {
    let prog = "(bind main (lambda ()\n\
                  (bind ((cell (array_local 0 i32)))\n\
                    (ptr_to_int cell))))\n";
    let msg = emit_diags("array_local_zero", prog);
    assert!(
        msg.contains("array_local length must be a positive integer"),
        "a zero-length array_local must be a backend diagnostic, got: {msg:?}"
    );
}

/// A NEGATIVE-length global array (raw `(global_array buf -1 u32)`) must surface
/// the located "must be a positive integer" diagnostic naming the array.
#[test]
fn stdlib_llvm_global_array_negative_length_is_rejected() {
    let prog = "(global_array buf -1 u32)\n\n\
                (bind main (lambda () (ptr_read buf)))\n";
    let msg = emit_diags("global_array_neg", prog);
    assert!(
        msg.contains("global_array `buf` length must be a positive integer"),
        "a negative-length global_array must be a backend diagnostic naming `buf`, got: {msg:?}"
    );
}

/// A VALID array program is UNCHANGED: a positive `array_local` and a positive
/// `global_array` both emit with NO diagnostics. The N>0 guard is purely
/// additive — it never fires on a well-formed program.
#[test]
fn stdlib_llvm_positive_arrays_emit_clean() {
    let prog = "(global_array buf 4 u32)\n\n\
                (bind main (lambda ()\n\
                  (bind ((cell (array_local 2 i32)))\n\
                    (do (ptr_read buf) (ptr_to_int cell)))))\n";
    let msg = emit_diags("arrays_clean", prog);
    assert_eq!(
        msg, "",
        "a valid array program must still emit with no diagnostics, got: {msg:?}"
    );
}

/// `emit_with_target` STACKS: when an outer target is already pinned, a nested
/// `emit_with_target` must RESTORE the outer target on exit (success and
/// throw), not clear to the host default. We observe the active target through
/// prep's `target_pointer_bytes` (4 for a 32-bit cross target, 8 for the host).
///
/// outer = 32-bit (4) → inner = 64-bit (8) → after inner the OUTER 32-bit (4)
/// must be back, on BOTH the success and the throwing inner body. With no outer
/// target set the prior is null, so the restore is a clear-to-host (8) — the
/// pre-existing behavior is unchanged (covered by the success path here too).
#[test]
fn stdlib_emit_with_target_restores_outer_not_host() {
    let v = eval_ok(
        "emit_with_target_nested",
        &with_stdlib_root(
            "(bind (
                 (llvm (load_module \"stdlib.backend.emit.llvm\"))
                 (prep (load_module \"stdlib.backend.prep\"))
                 (ewt (get llvm \"emit_with_target\" null))
                 (set_t (get llvm \"set_target!\" null))
                 (clear_t (get llvm \"clear_target!\" null))
                 (tpb (get prep \"target_pointer_bytes\" null))
                 (outer (assoc (map_of)
                           \"triple\" \"thumbv7m-none-eabi\"
                           \"datalayout\" \"e-m:e-p:32:32-Fi8-i64:64-v128:64:128-a:0:32-n32-S64\"))
                 (inner (assoc (map_of)
                           \"triple\" \"x86_64-unknown-linux-gnu\"
                           \"datalayout\" \"e-m:e-p:64:64-i64:64\"))
               )
                 (do
                   (bind ((no_outer_inner (ewt null inner (lambda (u) (tpb)))))
                     (bind ((after_no_outer (tpb)))
                       (do
                         (set_t (get outer \"triple\" \"\") (get outer \"datalayout\" \"\"))
                         (bind ((outer_pinned (tpb)))
                           (bind ((inner_seen (ewt null inner (lambda (u) (tpb)))))
                             (bind ((after_inner (tpb)))
                               (bind ((threw
                                        (try
                                          (do (ewt null inner (lambda (u) (throw \"boom\"))) \"no-throw\")
                                          (catch err (value_to_string err)))))
                                 (bind ((after_throw (tpb)))
                                   (do
                                     (clear_t)
                                     (list_of no_outer_inner after_no_outer
                                              outer_pinned inner_seen after_inner
                                              threw after_throw))))))))))))",
        ),
    );
    let RuntimeValue::List(items) = v else {
        panic!("expected list, got {v:?}")
    };
    let items = items.borrow();
    // [no_outer_inner, after_no_outer, outer_pinned, inner_seen, after_inner, threw, after_throw]
    let int_at = |i: usize| match &items[i] {
        RuntimeValue::Int(n) => *n,
        other => panic!("item {i} not an int: {other:?}"),
    };
    assert_eq!(
        int_at(0),
        8,
        "with no outer target the inner body sees host 64-bit (8)"
    );
    assert_eq!(
        int_at(1),
        8,
        "with no outer target the restore clears to host (8)"
    );
    assert_eq!(
        int_at(2),
        4,
        "the hand-pinned outer 32-bit target is 4 bytes"
    );
    assert_eq!(
        int_at(3),
        8,
        "the inner body observes the pinned 64-bit target (8)"
    );
    assert_eq!(
        int_at(4),
        4,
        "after a successful inner emit_with_target the OUTER 32-bit target (4) is restored, not host (8)"
    );
    assert!(
        matches!(&items[5], RuntimeValue::Str(s) if s.contains("boom")),
        "the inner body's throw is re-raised unchanged, got {:?}",
        items[5]
    );
    assert_eq!(
        int_at(6),
        4,
        "even when the inner body throws, the OUTER 32-bit target (4) is restored, not host (8)"
    );
}
