//! Best-effort wasm debug info: an EXTERNAL Source Map v3 (.wasm.map).
//!
//! WAT — the WebAssembly *text* format — has no debug-metadata syntax (no
//! `!DI…`-style channel a textual emitter can write the way the LLVM backend
//! does), so source-level wasm debugging cannot ride inside the WAT. The only
//! mechanism real wasm debuggers consume is an external source map. The wasm
//! backend therefore emits a FUNCTION-LEVEL Source Map v3 JSON alongside the WAT
//! when a debug level is requested, keyed on the generated WAT lines (which the
//! emitter controls exactly) back to `.caap` source lines. The granularity is
//! function-level by design: the textual layer does not know the final binary
//! byte offsets a wasm debugger keys on (those are assigned by `wat2wasm`), and
//! the wasm `lower` does not thread per-node spans the way the LLVM backend does.
//!
//! The proof here (always run, no external toolchain):
//!   1. a no-debug emit's WAT is BYTE-IDENTICAL to a "line-tables" emit's WAT and
//!      carries no source map (debug must never change codegen);
//!   2. a debug emit attaches a real Source Map v3 (version 3, the source path,
//!      a non-empty `mappings`).

use caap_core::RuntimeValue;

mod common;

use common::{eval_ok, with_stdlib_root};

/// A tiny real program with two typed functions and a `main` that calls one;
/// a REAL file (not synthesized specs) so the function values carry spans.
const PROGRAM_SRC: &str =
    "(defn add ((a i32)(b i32)) i32 (int_add a b))\n(bind main (lambda () (add 40 2)))\n(main)\n";

/// Emit `src` through stdlib's OWN wasm backend at the given `debug_level`
/// ("none" | "line-tables" | "full") and return (wat_text, source_map_or_empty,
/// diagnostic_count). A "none" level uses `emit_program`; any other uses
/// `emit_program_debug`. The source map is "" (the sentinel for null/no map).
fn emit_wasm(name: &str, src: &str, debug_level: &str) -> (String, String, i64) {
    let path = std::env::temp_dir().join(format!("{name}_{}.caap", std::process::id()));
    std::fs::write(&path, src).expect("write source");
    let p = path.to_str().unwrap().to_string();
    // for "none" call emit_program (the default, no debug arg); else _debug.
    let emit_call = if debug_level == "none" {
        format!(
            "((get (load_module \"stdlib.backend.emit.wasm\") \"emit_program\" null)
               (ctfe_compiler_load_surface_file_template compiler {p:?}))"
        )
    } else {
        format!(
            "((get (load_module \"stdlib.backend.emit.wasm\") \"emit_program_debug\" null)
               (ctfe_compiler_load_surface_file_template compiler {p:?})
               {debug_level:?})"
        )
    };
    let v = eval_ok(
        name,
        &with_stdlib_root(&format!(
            "(bind ((res {emit_call}))
               (list_of
                 (get res \"text\" \"\")
                 (bind ((m (get res \"source_map\" null))) (if (eq m null) \"\" m))
                 (size (get res \"diagnostics\" (list_of)))))"
        )),
    );
    std::fs::remove_file(&path).ok();
    let RuntimeValue::List(items) = v else {
        panic!("expected list, got {v:?}");
    };
    let items = items.borrow();
    let (RuntimeValue::Str(text), RuntimeValue::Str(map), RuntimeValue::Int(n)) =
        (&items[0], &items[1], &items[2])
    else {
        panic!("unexpected emit result shape: {items:?}");
    };
    (text.to_string(), map.to_string(), *n)
}

/// The no-debug emit and a debug emit must produce BYTE-IDENTICAL WAT, and only
/// the debug emit carries a source map. Debug must never change codegen.
#[test]
fn wasm_debug_wat_is_byte_identical_and_no_debug_has_no_map() {
    let (wat_none, map_none, dn) = emit_wasm("wasm_dbg_none", PROGRAM_SRC, "none");
    let (wat_dbg, map_dbg, dd) = emit_wasm("wasm_dbg_on", PROGRAM_SRC, "line-tables");
    assert_eq!(dn, 0, "no-debug emit diagnostics");
    assert_eq!(dd, 0, "debug emit diagnostics");

    assert_eq!(
        wat_none, wat_dbg,
        "the WAT must be byte-identical with and without debug:\n--none--\n{wat_none}\n--dbg--\n{wat_dbg}"
    );
    assert!(
        map_none.is_empty(),
        "a no-debug emit must carry NO source map, got: {map_none}"
    );
    assert!(!map_dbg.is_empty(), "a debug emit must carry a source map");
}

/// A debug emit produces a REAL Source Map v3: version 3, the source path in
/// `sources`, and a non-empty `mappings`. (We don't decode the VLQ here; the
/// presence + shape is the contract — `mappings` is non-trivial when functions
/// carry spans.)
#[test]
fn wasm_debug_emits_source_map_v3() {
    let (_wat, map, d) = emit_wasm("wasm_dbg_v3", PROGRAM_SRC, "full");
    assert_eq!(d, 0, "debug emit diagnostics");

    assert!(
        map.contains("\"version\":3"),
        "source map must be v3:\n{map}"
    );
    assert!(
        map.contains("\"sources\":[") && map.contains("wasm_dbg_v3"),
        "source map must list the .caap source path:\n{map}"
    );
    assert!(
        map.contains("\"mappings\":\""),
        "source map must have a mappings field:\n{map}"
    );
    // the mappings must be non-empty (two spanned functions -> >=1 segment).
    let mappings = map
        .split("\"mappings\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap_or("");
    assert!(
        !mappings.is_empty(),
        "mappings should encode at least one segment:\n{map}"
    );
    // a Source Map v3 mappings string is Base64-VLQ + ';' / ',' separators only.
    assert!(
        mappings.chars().all(|c| c.is_ascii_alphanumeric()
            || c == '+'
            || c == '/'
            || c == ';'
            || c == ','),
        "mappings must be Base64-VLQ:\n{mappings}"
    );
}

/// The debug path never changes the FUNCTIONS that land in the WAT: a debug emit
/// still contains every program function. (The map is an addition, not a
/// replacement — the WAT itself is unaffected, which the byte-identical test
/// above already proves; this is the per-function spot-check.)
#[test]
fn wasm_debug_keeps_all_functions_in_wat() {
    let (wat, _map, d) = emit_wasm("wasm_dbg_funcs", PROGRAM_SRC, "line-tables");
    assert_eq!(d, 0, "debug emit diagnostics");
    assert!(
        wat.contains("(func $add") && wat.contains("(func $main"),
        "the WAT must still contain the functions under debug:\n{wat}"
    );
}
