//! Native (LLVM) storage codegen: float fields. The eval byte runtime already
//! round-trips `float`/`f64`/`f32` via the kernel's float_to_bits/bits_to_float
//! inverse bit-casts (pinned in stdlib/lib/tests/test_storage.caap); these tests
//! pin the NATIVE leg of that parity — the SAME spec lowered through stdlib's own
//! LLVM backend, where a float field rides an LLVM `bitcast`.
//!
//!   1. `..._float_emits_bitcast` (always runs): render a float-field storage
//!      record to native-subset CAAP, emit it through the own LLVM backend, and
//!      assert the encode_F/decode_F BODIES contain the four bit-reinterpretations
//!      as `bitcast` instructions (double<->i64, float<->i32) — the novel lowering
//!      this work adds. `main` is bitcast-free, so the attribution is exact.
//!   2. `..._float_round_trip_matches_eval` (acceptance; self-skips without
//!      clang): build + run the native encode, and assert it produces the SAME
//!      on-disk bytes as the eval backend — byte-level eval = native parity.
use std::path::{Path, PathBuf};
use std::process::Command;

use caap_core::RuntimeValue;

mod common;

use common::{stdlib_bootstrap, stdlib_path, with_stdlib_root};

/// A storage spec map (built directly, like test_storage.caap's `mk_spec`) with
/// one record `F { d: float (f64, 8B), s: f32 (4B) }` — a native-lowerable float
/// record. The CAAP expression evaluates to the native-subset source for it.
const FLOAT_RECORD_NATIVE_SRC: &str = r#"
(bind (
  (sb (load_module "stdlib.storage.binary"))
  (render_native (get sb "render_native"))
  (mk_field (lambda (nm ty) (assoc (map_of) "name" nm "type" ty "crc" false "where" null)))
  (ft64 (assoc (map_of) "kind" "float" "size" 8 "spelling" "float"))
  (ft32 (assoc (map_of) "kind" "float" "size" 4 "spelling" "f32"))
  (rec (assoc (map_of) "name" "F" "version" 1
         "fields" (list_of (mk_field "d" ft64) (mk_field "s" ft32))))
  (spec (assoc (map_of) "name" "Floats" "records" (list_of rec) "migrations" (list_of)))
)
  (render_native spec))
"#;

/// Render the float record to native-subset CAAP source (the program a native
/// build/emit consumes).
fn float_record_native_src() -> String {
    let v = common::eval_ok(
        "float_native_src",
        &with_stdlib_root(FLOAT_RECORD_NATIVE_SRC),
    );
    let RuntimeValue::Str(s) = v else {
        panic!("expected native source string, got {v:?}");
    };
    s.to_string()
}

/// Emit `src` (native-subset CAAP, with a `main`) through stdlib's OWN LLVM
/// backend and return the IR text. Diagnostics fail the run.
fn emit_native_llvm(name: &str, src: &str) -> String {
    let path = std::env::temp_dir().join(format!("{name}_{}.caap", std::process::id()));
    std::fs::write(&path, src).expect("write native source");
    let p = path.to_str().unwrap().to_string();
    let v = common::eval_ok(
        name,
        &with_stdlib_root(&format!(
            "(bind ((res
                ((get (load_module \"stdlib.backend.emit.llvm\") \"emit_program\" null)
                 (ctfe_compiler_load_surface_file_template compiler {p:?}))))
               (list_of
                 (get res \"text\" null)
                 (size (get res \"diagnostics\" (list_of)))
                 (sequence_join
                   (sequence_map (get res \"diagnostics\" (list_of))
                     (lambda (d) (get d \"message\" \"?\")))
                   \"; \")))"
        )),
    );
    std::fs::remove_file(&path).ok();
    let RuntimeValue::List(items) = v else {
        panic!("expected list, got {v:?}");
    };
    let items = items.borrow();
    let (RuntimeValue::Str(text), RuntimeValue::Int(n)) = (&items[0], &items[1]) else {
        panic!("unexpected emit result shape: {items:?}");
    };
    assert_eq!(*n, 0, "emit diagnostics: {}", items[2]);
    text.to_string()
}

/// The `{ … }` body of `define … @<name>(…)` in LLVM IR text (the instructions
/// between the function's opening and closing brace). Panics if absent.
fn ir_function_body<'a>(ir: &'a str, name: &str) -> &'a str {
    let at = format!("@{name}(");
    let head = ir
        .find(&at)
        .unwrap_or_else(|| panic!("no `@{name}(` in IR:\n{ir}"));
    let open = ir[head..]
        .find('{')
        .map(|o| head + o)
        .unwrap_or_else(|| panic!("no body brace after @{name}:\n{ir}"));
    let close = ir[open..]
        .find("\n}")
        .map(|c| open + c)
        .unwrap_or_else(|| panic!("no closing brace for @{name}:\n{ir}"));
    &ir[open..close]
}

/// A float record lowers to native LLVM: each float field is written/read as an
/// integer via the kernel's float_to_bits/bits_to_float inverse bit-casts, which
/// the own LLVM backend turns into `bitcast` instructions. `main` is a trivial
/// `0` with NO bitcasts, so every bitcast in the IR must come from the storage
/// codegen ITSELF — proving the encode/decode BODIES emit them, not a synthetic
/// caller. Encode (value -> bits) and decode (bits -> value) cover all four
/// directions across the f64 and f32 fields.
#[test]
fn stdlib_storage_native_float_emits_bitcast() {
    // a no-op main (the emitter requires a `main`); it contains no float ops, so
    // it contributes zero bitcasts — every bitcast below is from encode_F/decode_F.
    let src = format!("{}\n(defn main () i32 0)\n", float_record_native_src());
    let ir = emit_native_llvm("storage_float_bitcast", &src);

    // the record DID lower natively (not skipped the way an unsupported field is)
    assert!(
        ir.contains("@encode_F(") && ir.contains("@decode_F("),
        "float record encode/decode lowered to native functions:\n{ir}"
    );
    // main itself must be bitcast-free, so the assertions truly attribute the
    // bitcasts to the storage bodies
    assert!(
        !ir_function_body(&ir, "main").contains("bitcast"),
        "main was supposed to be bitcast-free:\n{ir}"
    );

    // ENCODE bit-casts the float fields TO their integer bit patterns: f64->i64
    // (the d field) and f32->i32 (the s field). Both must appear in encode_F.
    let enc = ir_function_body(&ir, "encode_F");
    assert!(
        enc.contains("bitcast double ") && enc.contains(" to i64"),
        "encode_F should bitcast the f64 field to i64:\n{enc}"
    );
    assert!(
        enc.contains("bitcast float ") && enc.contains(" to i32"),
        "encode_F should bitcast the f32 field to i32:\n{enc}"
    );

    // DECODE bit-casts the read integers BACK to floats: i64->double and i32->float.
    let dec = ir_function_body(&ir, "decode_F");
    assert!(
        dec.contains("bitcast i64 ") && dec.contains(" to double"),
        "decode_F should bitcast the i64 back to an f64:\n{dec}"
    );
    assert!(
        dec.contains("bitcast i32 ") && dec.contains(" to float"),
        "decode_F should bitcast the i32 back to an f32:\n{dec}"
    );
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

fn caap_binary() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    ["../target/debug/caap", "../target/release/caap"]
        .iter()
        .map(|r| manifest.join(r))
        .find(|p| p.is_file())
        .expect("caap binary (build the workspace first)")
}

/// The EVAL backend's encoding of `F { d: 3.14159, s: 2.5 }`, as the 12 on-disk
/// bytes. Loaded through the loader (the `(storage …)` form needs the grammar
/// extension active, which the module's `(use stdlib.storage.binary …)` installs).
fn eval_float_bytes() -> Vec<u8> {
    // The `(storage …)` form needs its grammar extension active, which only the
    // module loader installs — so the eval encode is obtained by loading a real
    // storage module (built under a temp root) and calling its encode_F_v1.
    let tmp = std::env::temp_dir().join(format!("storage-eval-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("mktmp");
    let module = tmp.join("evalbytes.caap");
    std::fs::write(
        &module,
        "(module test.evalbytes)\n\
         (use stdlib.storage.binary\n\
         \x20\x20le_bytes read_le read_le_signed validate crc32 emit! pad_bytes slice_bytes)\n\
         (bind store (storage Floats (endian little) (record F 1 (field d float) (field s f32))))\n\
         (bind enc (get store \"encode_F_v1\"))\n\
         (bind out_bytes (enc (assoc (map_of) \"d\" 3.14159 \"s\" 2.5)))\n\
         (export out_bytes)\n",
    )
    .expect("write eval module");

    let root = stdlib_path("");
    let modroot = tmp.to_str().unwrap().to_string();
    let v = common::eval_ok(
        "storage_eval_bytes",
        &format!(
            "(bind (
                 (api          (ctfe_compiler_lookup_value compiler \"stdlib.load\"))
                 (load_module  (get api \"load_module\" null))
                 (declare_root (get api \"declare_root\" null))
               )
                 (do
                   (declare_root \"stdlib\" {root:?})
                   (declare_root \"test\" {modroot:?})
                   (get (load_module \"test.evalbytes\") \"out_bytes\" (list_of))))"
        ),
    );
    std::fs::remove_dir_all(&tmp).ok();
    let RuntimeValue::List(items) = v else {
        panic!("expected byte list, got {v:?}");
    };
    let bytes: Vec<u8> = items
        .borrow()
        .iter()
        .map(|b| match b {
            RuntimeValue::Int(n) => *n as u8,
            other => panic!("non-int byte: {other:?}"),
        })
        .collect();
    bytes
}

/// Byte-level eval = native parity: the native encode of `F { 3.14159, 2.5 }`
/// produces the SAME on-disk bytes as the eval backend. The native program
/// encodes into a stack buffer and returns 0 iff every byte equals the
/// eval-computed expectation (the float fields ride LLVM `bitcast`). Needs clang.
#[test]
#[ignore = "acceptance: builds + runs a native float-storage encode (clang) and checks byte parity vs the eval backend"]
fn stdlib_storage_native_float_round_trip_matches_eval() {
    if !on_path("clang") {
        eprintln!("skipping: clang unavailable");
        return;
    }
    let expected = eval_float_bytes();
    assert_eq!(expected.len(), 12, "F encodes to 8 (f64) + 4 (f32) bytes");

    let tmp = std::env::temp_dir().join(format!("storage-native-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("mktmp");
    let composed = tmp.join("native_composed.caap");
    std::fs::write(
        &composed,
        format!(
            "(do\n  (ctfe_compiler_execute_bootstrap_file compiler {:?})\n  (ctfe_compiler_execute_bootstrap_file compiler {:?}))\n",
            stdlib_bootstrap(),
            stdlib_path("boot/native_emit.caap"),
        ),
    )
    .expect("write composed");

    // native source: the float record + a main that encodes and folds the 12
    // per-byte equality checks (each vs the eval-computed expectation) into a
    // count, returning 0 iff all match — byte-level eval = native parity.
    let sum = {
        let mut it = expected
            .iter()
            .enumerate()
            .map(|(i, b)| format!("(byte_eq buf {i} {b})"));
        let first = it.next().expect("at least one byte");
        it.fold(first, |acc, term| format!("(int_add {acc} {term})"))
    };
    let program = format!(
        "{src}\n\
         (defn byte_eq ((buf ptr_u8) (i i32) (want i32)) i32\n\
         \x20\x20(if (eq (cast (ptr_read (ptr_add buf i)) i32) want) 1 0))\n\
         (defn main () i32\n\
         \x20\x20(bind (\n\
         \x20\x20\x20\x20(buf (array_local 12 u8))\n\
         \x20\x20\x20\x20(orig (make_F 3.14159 2.5))\n\
         \x20\x20\x20\x20(n (encode_F orig (ptr_add buf 0)))\n\
         \x20\x20\x20\x20(ok {sum})\n\
         \x20\x20)\n\
         \x20\x20\x20\x20(if (eq ok 12) 0 1)))\n",
        src = float_record_native_src(),
        sum = sum,
    );
    let prog_path = tmp.join("parity.caap");
    std::fs::write(&prog_path, &program).expect("write native parity program");

    let caap = caap_binary();
    let bin = tmp.join("parity_bin");
    let build = Command::new(&caap)
        .arg(&composed)
        .arg(stdlib_path("../tools/s2_build.caap"))
        .arg(&prog_path)
        .arg(&bin)
        .output()
        .expect("run s2_build");
    let built = build.status.success() && bin.is_file();
    // run the binary BEFORE cleanup; capture everything into owned values so the
    // temp dir can be removed unconditionally (no leak if an assert below panics).
    let run_code = built.then(|| {
        Command::new(&bin)
            .output()
            .expect("run native parity binary")
            .status
            .code()
    });
    let build_stdout = String::from_utf8_lossy(&build.stdout).into_owned();
    let build_stderr = String::from_utf8_lossy(&build.stderr).into_owned();
    std::fs::remove_dir_all(&tmp).ok();

    assert!(
        built,
        "native build failed:\nstdout: {build_stdout}\nstderr: {build_stderr}\nprogram:\n{program}",
    );
    assert_eq!(
        run_code,
        Some(Some(0)),
        "native encode bytes must equal the eval backend's (byte-level eval=native parity); \
         exit {run_code:?}\nexpected eval bytes: {expected:?}",
    );
}
