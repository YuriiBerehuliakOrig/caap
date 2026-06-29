//! Consistency guards for the single native-vocabulary source
//! (`stdlib/backend/native_meta.caap`, audit follow-up #4).
//!
//! Before #4 the native head/type vocabulary lived in three hand-kept places
//! (backend/native_meta's `native_vocab`/`type_tokens`, the LLVM dispatch, the WAT
//! dispatch) that had to agree by convention — and had drifted: the `atomic_*`
//! heads were in the gate vocabulary but the WAT emitter had no arm for them, so
//! a wasm program using one fell through to the generic "unsupported form" error.
//! These two guards pin the canonical set and prove the gap is closed.
use caap_core::RuntimeValue;

mod common;

use common::{eval_ok, with_stdlib_root};

fn eval_str(name: &str, body: &str) -> String {
    match eval_ok(name, &with_stdlib_root(body)) {
        RuntimeValue::Str(s) => s.to_string(),
        other => panic!("expected string, got {other:?}"),
    }
}

/// The native head + type vocabulary is the single source feeding prep's gate,
/// the WAT rejects, and the strict profile. Pin the exact sets so adding or
/// removing a head/type is a DELIBERATE edit to `native_meta.caap`, not a silent
/// drift across the backends. (A symmetric set diff, so order is irrelevant.)
#[test]
fn native_meta_head_and_type_sets_are_pinned() {
    let out = eval_str(
        "native_meta_pin",
        "(bind (
            (nm (load_module \"stdlib.backend.native_meta\"))
            (heads (get nm \"head_names\"))
            (types (get nm \"type_names\"))
            (exp_h (list_of \"ptr_read\" \"ptr_write\" \"ptr_add\" \"int_to_ptr\" \"ptr_to_int\"
                            \"union_field\" \"field_ptr\" \"array_local\" \"cast\" \"fn_ptr\"
                            \"call_ptr\" \"println\" \"asm\" \"volatile_read\" \"volatile_write\"
                            \"atomic_load\" \"atomic_store\" \"atomic_add\" \"atomic_cas\"))
            (exp_t (list_of \"int\" \"i8\" \"u8\" \"i16\" \"u16\" \"i32\" \"u32\" \"i64\" \"u64\"
                            \"uptr\" \"f32\" \"f64\" \"bool\" \"string\" \"void\" \"ptr\"))
            (hset (map_of)) (eset (map_of)) (tset (map_of)) (etset (map_of))
            (miss (list_of)) (extra (list_of))
          )
            (do
              (sequence_each heads (lambda (h) (assoc hset h true)))
              (sequence_each exp_h (lambda (e) (assoc eset e true)))
              (sequence_each types (lambda (t) (assoc tset t true)))
              (sequence_each exp_t (lambda (e) (assoc etset e true)))
              (sequence_each exp_h (lambda (e) (if (contains hset e) null (append miss e))))
              (sequence_each heads (lambda (h) (if (contains eset h) null (append extra h))))
              (sequence_each exp_t (lambda (e) (if (contains tset e) null (append miss e))))
              (sequence_each types (lambda (t) (if (contains etset t) null (append extra t))))
              (string_concat_many \"missing=[\" (sequence_join miss \",\")
                                  \"] extra=[\" (sequence_join extra \",\") \"]\")))",
    );
    assert_eq!(
        out, "missing=[] extra=[]",
        "native_meta head/type vocabulary drifted from the pinned canonical set: {out}"
    );
}

/// A wasm program using `atomic_load` — a valid native head the gate accepts, but
/// one the WAT emitter cannot realize — must be rejected with a PRECISE diagnostic
/// naming the head, NOT the generic "unsupported form" fallthrough it hit before
/// the central table closed the gap.
#[test]
fn wasm_rejects_atomic_load_with_precise_diagnostic() {
    let prog = "(bind main (lambda ()\n\
                  (bind ((cell (array_local 1 i32)))\n\
                    (atomic_load cell))))\n";
    let path = std::env::temp_dir().join(format!("nm_wasm_atomic_{}.caap", std::process::id()));
    std::fs::write(&path, prog).expect("write program");
    let p = path.to_str().unwrap().to_string();
    let out = eval_str(
        "wasm_atomic_reject",
        &format!(
            "(try
               (bind ((res ((get (load_module \"stdlib.backend.emit.wasm\") \"emit_program\" null)
                             (ctfe_compiler_load_surface_file_template compiler {p:?}))))
                 (sequence_join (sequence_map (get res \"diagnostics\" (list_of))
                   (lambda (d) (get d \"message\" \"?\"))) \" | \"))
               (catch e (value_to_string e)))"
        ),
    );
    std::fs::remove_file(&path).ok();
    assert!(
        out.contains("atomic_load") && out.contains("WebAssembly realization"),
        "wasm must reject atomic_load with a precise diagnostic naming the head, got: {out:?}"
    );
    assert!(
        !out.contains("unsupported form"),
        "wasm must NOT fall through to the generic 'unsupported form' error for atomic_load, got: {out:?}"
    );
}
