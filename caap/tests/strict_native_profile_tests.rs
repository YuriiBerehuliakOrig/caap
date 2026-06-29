//! The opt-in STRICT native profile (audit follow-up #5): backend/prep's gate,
//! when pinned via `set_strict!`, additionally rejects a DECLARED type token that
//! resolves to no known native type / program struct-union / pointer — the silent
//! path where the backend would otherwise lower it to a null IR type. The profile
//! is OFF by default, so an ordinary build's behavior is unchanged.
use caap_core::RuntimeValue;

mod common;

use common::{eval_ok, with_stdlib_root};

/// Run the program at a temp path through prep's gate and return the joined
/// diagnostic messages ("" when clean). `strict` pins the strict profile (and
/// always clears it afterward, even on the clean path).
fn prep_diags(name: &str, prog: &str, strict: bool) -> String {
    let path = std::env::temp_dir().join(format!("{name}_{}.caap", std::process::id()));
    std::fs::write(&path, prog).expect("write program");
    let p = path.to_str().unwrap().to_string();
    // pin or explicitly unpin the profile before prep (explicit clear keeps the
    // permissive case deterministic regardless of any prior state).
    let pin = if strict {
        "((get prep \"set_strict!\"))"
    } else {
        "((get prep \"clear_strict!\"))"
    };
    let v = eval_ok(
        name,
        &with_stdlib_root(&format!(
            "(bind ((prep (load_module \"stdlib.backend.prep\"))
                    (unit (ctfe_compiler_load_surface_file_template compiler {p:?})))
               (do
                 {pin}
                 (bind ((res ((get prep \"prep_units\") unit)))
                   (do
                     ((get prep \"clear_strict!\"))
                     (sequence_join (sequence_map (get res \"diagnostics\" (list_of))
                       (lambda (d) (get d \"message\" \"?\"))) \" | \")))))"
        )),
    );
    std::fs::remove_file(&path).ok();
    match v {
        RuntimeValue::Str(s) => s.to_string(),
        other => panic!("expected diagnostics string, got {other:?}"),
    }
}

// An extern whose RESULT type `u33` is not a known native type. The type sits in
// the (inert) declaration, so the gate's bare-name check does not see it.
const UNKNOWN_TYPE_PROG: &str = "(extern foo () u33)\n(bind main (lambda () 0))\n";

/// Strict mode REJECTS the unknown declared type with a located diagnostic that
/// names the type and where it appears.
#[test]
fn strict_profile_rejects_unknown_declared_type() {
    let msg = prep_diags("strict_extern", UNKNOWN_TYPE_PROG, true);
    assert!(
        msg.contains("strict native profile") && msg.contains("u33"),
        "strict mode must reject the unknown extern result type `u33`, got: {msg:?}"
    );
}

/// Default (permissive) mode is UNCHANGED: the same program passes the gate CLEAN
/// — the unknown type would silently lower to a null IR type, exactly the gap the
/// opt-in strict profile closes. This pins the behavior-preserving default.
#[test]
fn permissive_profile_is_clean_on_unknown_declared_type() {
    let msg = prep_diags("perm_extern", UNKNOWN_TYPE_PROG, false);
    assert_eq!(
        msg, "",
        "permissive mode must NOT run the strict type check (default unchanged), got: {msg:?}"
    );
}
