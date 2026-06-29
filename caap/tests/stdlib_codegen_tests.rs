//! Codegen / native / surface scenarios: the C-like surface lowering, module
//! globals and global arrays, the native-prep gate, a runtime-generated
//! program through eval + LLVM, and the URun freestanding vertical slice.
use caap_core::{frontend::parse, PhasePolicy, RuntimeValue, Unit};

mod common;

use common::{corpus_path, eval_ok, stdlib_bootstrap, stdlib_path, with_stdlib_root};

/// Emit a program through stdlib's OWN LLVM backend (backend/emit/llvm — no v1
/// involvement) and return (ir_text, diagnostic_count).
fn emit_own_llvm(name: &str, path: &str) -> (String, i64) {
    let v = eval_ok(
        name,
        &with_stdlib_root(&format!(
            "(bind ((res
                ((get (load_module \"stdlib.backend.emit.llvm\")
                      \"emit_program\" null)
                 (ctfe_compiler_load_surface_file_template compiler {path:?}))))
               (list_of
                 (get res \"text\" null)
                 (size (get res \"diagnostics\" (list_of)))
                 (sequence_join
                   (sequence_map (get res \"diagnostics\" (list_of))
                     (lambda (d) (get d \"message\" \"?\")))
                   \"; \")))"
        )),
    );
    let RuntimeValue::List(items) = v else {
        panic!("expected list, got {v:?}")
    };
    let items = items.borrow();
    let RuntimeValue::Str(text) = &items[0] else {
        panic!("text not a string")
    };
    let RuntimeValue::Int(n) = items[1] else {
        panic!("count not an int")
    };
    if n != 0 {
        panic!("emitter diagnostics: {}", items[2]);
    }
    (text.to_string(), n)
}

/// TIER 5 IN FULL — the C-like surface language (frontend/clike): the
/// guess_number_game written as `play (secret i32) i32 = { lo mut i32 = 1 …
/// while found == 0 { … } }` lowers through the PEG lexer + the CAAP
/// structure pass into defn/bind/while/if specs, rides the WHOLE tower
/// (expander, checker, TYPE PASS, eval) and the OWN LLVM backend. Binary
/// search over 1..100: secret 42 -> 7 attempts, secret 7 -> 6; main = 13.
#[test]
#[ignore = "acceptance: exercises the full C-like surface through LLVM lowering"]
fn stdlib_clike_game_lowers_evaluates_and_lowers_to_llvm() {
    let game = "pick_secret () i32 = { 42 }\n\nplay (secret i32) i32 = {\n  lo mut i32 = 1\n  hi mut i32 = 100\n  attempts mut i32 = 0\n  found mut i32 = 0\n  while found == 0 {\n    mid i32 = (lo + hi) / 2\n    attempts = attempts + 1\n    if mid == secret { found = 1 }\n    if mid < secret { lo = mid + 1 }\n    if mid > secret { hi = mid - 1 }\n  }\n  attempts\n}\n\nmain () i32 = {\n  play (pick_secret ()) + play (7)\n}\n";
    let mut compiler = common::codegen_bootstrapped_session();
    let src = format!(
        "(bind (
             (ck (ctfe_compiler_lookup_value compiler \"stdlib.frontend.clike\"))
             (rd (ctfe_compiler_lookup_value compiler \"stdlib.syntax.render\"))
           )
             (bind ((r ((get ck \"lower_program\" null) \"{game}\")))
               (if (get r \"ok\" false)
                 ((get rd \"render_program\" null) (get r \"forms\" (list_of)))
                 (runtime_error (get r \"error\" \"lowering failed\")))))"
    );
    let graph = parse(&src).expect("parse");
    let unit = Unit::from_graph("clike_lower", graph).expect("unit");
    let text = compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("clike lowering");
    let RuntimeValue::Str(text) = text else {
        panic!("expected rendered program, got {text:?}")
    };
    assert!(
        text.contains("(defn play ((secret i32)) i32")
            && text.contains("(while (eq (deref found) 0)")
            && text.ends_with("(main)\n"),
        "lowered program shape:\n{text}"
    );

    // the declaration's META position is checked, not decorative
    let bad = "(bind ((ck (ctfe_compiler_lookup_value compiler \"stdlib.frontend.clike\")))
             (get ((get ck \"lower_program\" null)
                   \"main () i32 = { x bogus = 1 }\")
                  \"error\" \"\"))"
        .to_string();
    let graph = parse(&bad).expect("parse");
    let unit = Unit::from_graph("clike_badtype", graph).expect("unit");
    let mut compiler2 = common::codegen_bootstrapped_session();
    let err = compiler2
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("lowering returns error as data");
    let RuntimeValue::Str(err) = err else {
        panic!("expected error text, got {err:?}")
    };
    assert!(
        err.contains("unknown type `bogus` in the declaration of `x`")
            && err.starts_with("clike: 1:"),
        "the NMV meta position is validated, with the token's location: {err}"
    );

    // and the declared type BINDS the value: a sized type range-checks an
    // integer-literal initializer
    let bad2 = "(bind ((ck (ctfe_compiler_lookup_value compiler \"stdlib.frontend.clike\")))
             (get ((get ck \"lower_program\" null)
                   \"main () i32 = { x u8 = 300 }\")
                  \"error\" \"\"))"
        .to_string();
    let graph = parse(&bad2).expect("parse");
    let unit = Unit::from_graph("clike_range", graph).expect("unit");
    let mut compiler3 = common::codegen_bootstrapped_session();
    let err2 = compiler3
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("range error as data");
    let RuntimeValue::Str(err2) = err2 else {
        panic!("expected error text, got {err2:?}")
    };
    assert!(
        err2.contains("literal 300 out of range for u8 (declaration of `x`)"),
        "sized meta range-checks the initializer: {err2}"
    );

    let path = std::env::temp_dir().join(format!("s2_clike_{}.caap", std::process::id()));
    std::fs::write(&path, text.as_ref()).unwrap();
    let p = path.to_str().unwrap();

    // the whole tower evaluates the generated program: 7 + 6 attempts
    let v = eval_ok("clike_eval", &with_stdlib_root(&format!("(load {p:?})")));
    assert_eq!(
        v,
        RuntimeValue::Int(13),
        "binary-search attempts, eval phase"
    );

    // and the OWN LLVM backend lowers it
    let (ir, diags) = emit_own_llvm("clike_llvm", p);
    std::fs::remove_file(&path).ok();
    assert_eq!(diags, 0, "no emit diagnostics:\n{ir}");
    assert!(
        ir.contains("define i32 @play(i32 %secret)") && ir.contains("define i32 @main()"),
        "typed native signatures from the C-like source:\n{ir}"
    );
}

/// The C-like surface reaches a module `global` BY NAME: a top-level
/// `name type = expr` lowers to `(global …)`, and inside a function a use of
/// that name lowers to `(deref name)`, an assignment to `(set_ref name …)` — so
/// the native backend's global ref-cells are addressable without raw-address
/// arithmetic (the idiom the bare-metal scheduler state now uses).
#[test]
fn stdlib_clike_lowers_module_globals_by_name() {
    let prog = "ticks mut u32 = 0\n\nbump () i32 = {\n  ticks = ticks + 1\n  ticks\n}\n";
    let mut compiler = common::codegen_bootstrapped_session();
    let src = format!(
        "(bind (
             (ck (ctfe_compiler_lookup_value compiler \"stdlib.frontend.clike\"))
             (rd (ctfe_compiler_lookup_value compiler \"stdlib.syntax.render\"))
           )
             (bind ((r ((get ck \"lower_program\" null) \"{prog}\")))
               (if (get r \"ok\" false)
                 ((get rd \"render_program\" null) (get r \"forms\" (list_of)))
                 (runtime_error (get r \"error\" \"lowering failed\")))))"
    );
    let graph = parse(&src).expect("parse");
    let unit = Unit::from_graph("clike_globals", graph).expect("unit");
    let RuntimeValue::Str(text) = compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("clike global lowering")
    else {
        panic!("expected rendered program")
    };
    assert!(
        text.contains("(global ticks u32 0)")
            && text.contains("(set_ref ticks (int_add (deref ticks) 1))")
            && text.contains("(deref ticks)"),
        "a module global is read/written by name (deref/set_ref):\n{text}"
    );
}

/// A C-like GLOBAL ARRAY: `name elem = array(N)` lowers to `(global_array …)` (a
/// zero-init BSS buffer), and the name DECAYS to a typed pointer — a use stays a
/// bare name (NOT deref'd) so `ptr_add`/`ptr_read` index it. This is what lets
/// the bare-metal task stacks be named instead of raw addresses.
#[test]
fn stdlib_clike_lowers_global_array() {
    let prog = "buf u32 = array(8)\n\nhead () i32 = {\n  ptr_read(buf)\n}\n";
    let mut compiler = common::codegen_bootstrapped_session();
    let src = format!(
        "(bind (
             (ck (ctfe_compiler_lookup_value compiler \"stdlib.frontend.clike\"))
             (rd (ctfe_compiler_lookup_value compiler \"stdlib.syntax.render\"))
           )
             (bind ((r ((get ck \"lower_program\" null) \"{prog}\")))
               (if (get r \"ok\" false)
                 ((get rd \"render_program\" null) (get r \"forms\" (list_of)))
                 (runtime_error (get r \"error\" \"lowering failed\")))))"
    );
    let graph = parse(&src).expect("parse");
    let unit = Unit::from_graph("clike_global_array", graph).expect("unit");
    let RuntimeValue::Str(text) = compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("clike global-array lowering")
    else {
        panic!("expected rendered program")
    };
    assert!(
        text.contains("(global_array buf 8 u32)") && text.contains("(ptr_read buf)"),
        "a global array lowers to (global_array …) and its name decays to a pointer:\n{text}"
    );
}

/// The closing arc of runtime codegen: a PROGRAM built from data — template
/// text for the body, a peephole rewrite over it, lib/ast `lam` for main —
/// rendered with render_program, written to a file, and consumed by BOTH
/// pipelines: the loader evaluates it to 42, and stdlib's own LLVM backend
/// lowers it to a `define i32 @main`.
#[test]
#[ignore = "acceptance: builds a runtime-generated program through eval and LLVM lowering"]
fn stdlib_runtime_built_program_renders_evals_and_lowers() {
    let mut compiler = common::codegen_bootstrapped_session();
    let src = "(bind (
             (sk (ctfe_compiler_lookup_value compiler \"stdlib.frontend.surface\"))
             (ir (ctfe_compiler_lookup_value compiler \"stdlib.syntax.ir\"))
             (ast (ctfe_compiler_lookup_value compiler \"stdlib.syntax.ast\"))
             (rd (ctfe_compiler_lookup_value compiler \"stdlib.syntax.render\"))
           )
             (bind (
               (tpl (get sk \"template\" null))
               (rl (get ir \"rule\" null))
               (rw (get ir \"rewrite\" null))
               (lam (get ast \"lam\" null))
               (calln (get ast \"calln\" null))
             )
               (bind (
                 (body (rw (tpl \"(int_add (int_add 40 2) 0)\" (map_of))
                           (list_of (rl (tpl \"(int_add ?x 0)\" (map_of))
                                        (tpl \"?x\" (map_of))))))
               )
                 ((get rd \"render_program\" null)
                   (list_of
                     (calln \"bind\"
                       (list_of (syntax_name \"main\") (lam (list_of) body)))
                     (calln \"main\" (list_of)))))))"
        .to_string();
    let graph = parse(&src).expect("parse");
    let unit = Unit::from_graph("rt_build", graph).expect("unit");
    let text = compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("build + render");
    let RuntimeValue::Str(text) = text else {
        panic!("expected rendered source text, got {text:?}")
    };
    assert!(
        text.contains("(int_add 40 2)") && !text.contains(" 0)"),
        "the x+0 layer was rewritten away before rendering:\n{text}"
    );

    let path = std::env::temp_dir().join(format!("s2_rt_build_{}.caap", std::process::id()));
    std::fs::write(&path, text.as_ref()).unwrap();
    let p = path.to_str().unwrap();

    // consumer 1: the loader evaluates the generated program
    let v = eval_ok("rt_build_eval", &with_stdlib_root(&format!("(load {p:?})")));
    assert_eq!(v, RuntimeValue::Int(42), "generated program evaluates");

    // consumer 2: stdlib's own LLVM backend lowers it
    let (ir, diags) = emit_own_llvm("rt_build_llvm", p);
    std::fs::remove_file(&path).ok();
    assert_eq!(diags, 0, "no emit diagnostics:\n{ir}");
    assert!(
        ir.contains("define i32 @main"),
        "generated program lowers to native main:\n{ir}"
    );
}

/// Phase parity: the native prep path now runs the SAME checker + type pass
/// the loader does, BEFORE codegen. A typo in a native program is a located
/// unknown-name diagnostic, not a cryptic "codegen: unsupported free name".
#[test]
fn stdlib_native_prep_gate_rejects_typo_before_codegen() {
    let bad = corpus_path("fixtures/native_typo.caap");
    let msg = {
        let mut compiler = common::codegen_bootstrapped_session();
        let src = format!(
            "(bind ((res
                  ((ctfe_compiler_lookup_value compiler \"stdlib.llvm.emit\")
                   (ctfe_compiler_load_surface_file_template compiler {bad:?}))))
                 (sequence_join
                   (sequence_map (get res \"diagnostics\" (list_of))
                     (lambda (d) (get d \"message\" \"?\")))
                   \" | \"))"
        );
        let graph = parse(&src).expect("parse");
        let unit = Unit::from_graph("native_typo", graph).expect("unit");
        match compiler
            .evaluation()
            .evaluate(&unit, PhasePolicy::CompileTime, [])
            .expect("emit")
        {
            RuntimeValue::Str(s) => s.to_string(),
            other => panic!("expected string, got {other:?}"),
        }
    };
    assert!(
        msg.contains("unknown name `squrae`") && msg.contains("native_typo.caap:"),
        "located unknown-name diagnostic before codegen: {msg}"
    );
}

/// Audit #1 regression: a pointer's storage size is TARGET-aware. prep's
/// `scalar_bytes("ptr")` (which sizes union/struct layout) must read the active
/// target's pointer width — 4 bytes on a 32-bit cross target, 8 by default
/// (host) and on a 64-bit target. backend/emit/llvm's `set_target!`/`clear_target!`
/// push the width into prep (derived from the datalayout `p:<n>:` field, with a
/// triple-arch fallback); `clear_target!` restores the host default.
#[test]
fn stdlib_native_prep_pointer_size_follows_target() {
    // Drive set_target!/clear_target! on backend/emit/llvm and read prep's
    // `target_pointer_bytes` (the value scalar_bytes("ptr") returns, which sizes
    // union/struct layout) at each step. Returns a flat list of the observed
    // ints. (Locals are `set_t`/`clear_t` — `set!`/`clear!` are reserved reader
    // names under the stdlib bootstrap.)
    let v = eval_ok(
        "ptr_size_target",
        &with_stdlib_root(
            "(bind (
                 (llvm (load_module \"stdlib.backend.emit.llvm\"))
                 (prep (load_module \"stdlib.backend.prep\"))
                 (set_t (get llvm \"set_target!\" null))
                 (clear_t (get llvm \"clear_target!\" null))
                 (tpb (get prep \"target_pointer_bytes\" null))
               )
                 (do
                   (bind ((host_default (tpb)))
                     (do
                       (set_t \"thumbv7m-none-eabi\"
                              \"e-m:e-p:32:32-Fi8-i64:64-v128:64:128-a:0:32-n32-S64\")
                       (bind ((cross_32 (tpb)))
                         (do
                           (clear_t)
                           (bind ((after_clear (tpb)))
                             (do
                               (set_t \"x86_64-unknown-linux-gnu\" \"e-m:e-p:64:64-i64:64\")
                               (bind ((cross_64 (tpb)))
                                 (do
                                   (set_t \"wasm32-unknown-unknown\" \"\")
                                   (bind ((triple_32 (tpb)))
                                     (do (clear_t)
                                       (list_of host_default cross_32 after_clear
                                                cross_64 triple_32)))))))))))))",
        ),
    );
    let RuntimeValue::List(items) = v else {
        panic!("expected list, got {v:?}")
    };
    let items = items.borrow();
    let got: Vec<i64> = items
        .iter()
        .map(|x| match x {
            RuntimeValue::Int(n) => *n,
            other => panic!("expected int, got {other:?}"),
        })
        .collect();
    assert_eq!(
        got,
        vec![8, 4, 8, 8, 4],
        "host=8, 32-bit datalayout=4, clear=8, 64-bit datalayout=8, 32-bit triple-only=4"
    );
}

/// A3b: `emit_with_target` pins the module to a target for the duration of a
/// body, then restores the host default NO MATTER WHAT — including when the body
/// throws (try/catch-rethrow). We observe the pinned/restored state through
/// prep's `target_pointer_bytes` (4 for a 32-bit cross target, 8 for the host).
/// On the success path the body sees the pinned width and the target is cleared
/// after; on the throw path the original error is re-raised AND the target is
/// still cleared (no leak into the next emit).
#[test]
fn stdlib_emit_with_target_scopes_and_always_clears() {
    let v = eval_ok(
        "emit_with_target_scope",
        &with_stdlib_root(
            "(bind (
                 (llvm (load_module \"stdlib.backend.emit.llvm\"))
                 (prep (load_module \"stdlib.backend.prep\"))
                 (ewt (get llvm \"emit_with_target\" null))
                 (tpb (get prep \"target_pointer_bytes\" null))
                 (target (assoc (map_of)
                            \"triple\" \"thumbv7m-none-eabi\"
                            \"datalayout\" \"e-m:e-p:32:32-Fi8-i64:64-v128:64:128-a:0:32-n32-S64\"))
               )
                 (do
                   ; success path: the body observes the PINNED width (4)
                   (bind ((seen_ok (ewt null target (lambda (u) (tpb)))))
                     (do
                       (bind ((after_ok (tpb)))     ; cleared back to host (8)
                         ; throw path: the body throws; emit_with_target must
                         ; re-raise AND still clear. Capture both facts.
                         (bind ((threw
                                  (try
                                    (do (ewt null target (lambda (u) (throw \"boom\"))) \"no-throw\")
                                    (catch err (value_to_string err)))))
                           (bind ((after_throw (tpb)))   ; still cleared (8)
                             (list_of seen_ok after_ok threw after_throw))))))))",
        ),
    );
    let RuntimeValue::List(items) = v else {
        panic!("expected list, got {v:?}")
    };
    let items = items.borrow();
    // [pinned_width_in_body, width_after_success, throw_message, width_after_throw]
    let (seen_ok, after_ok, after_throw) = (&items[0], &items[1], &items[3]);
    assert!(
        matches!(seen_ok, RuntimeValue::Int(4)),
        "the body should see the PINNED 32-bit target (4 bytes), got {seen_ok:?}"
    );
    assert!(
        matches!(after_ok, RuntimeValue::Int(8)),
        "the target must be cleared back to host default (8) after success, got {after_ok:?}"
    );
    assert!(
        matches!(&items[2], RuntimeValue::Str(s) if s.contains("boom")),
        "the body's throw must be re-raised unchanged, got {:?}",
        items[2]
    );
    assert!(
        matches!(after_throw, RuntimeValue::Int(8)),
        "the target must be cleared back to host default (8) even after a throw, got {after_throw:?}"
    );
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

/// The URun cross-build needs clang AND ld.lld: the host
/// default `ld.bfd` rejects the ARM ELF ("unrecognised emulation mode: armelf"),
/// so the build forces `-fuse-ld=lld` (see driver.caap's cortex-m3 target). A
/// missing LLVM linker is an ENVIRONMENT gap, not a URun regression — gate on
/// it so the test self-skips instead of failing at the link stage.
fn has_urun_toolchain() -> bool {
    on_path("clang") && on_path("ld.lld")
}

/// URun "common" VERTICAL SLICE — the portable URun kernel model
/// (priority-preemptive scheduler, thread suspend/resume, a counting semaphore
/// and a message queue) re-implemented in the NMV (`stdlib.frontend.clike`) surface
/// with real UR_THREAD/UR_SEMAPHORE/UR_QUEUE structs and pointer-linked
/// intrusive lists, cross-compiled freestanding to Cortex-M3. Authored as a
/// URun-style PROJECT (`examples/urun/`: per-object ur_*.caap fragments +
/// an ARM port + a linker script), assembled and built by its own
/// `ur_build.caap`. A high-priority consumer blocks on the semaphore + queue; a
/// low-priority producer sends 'A','B','C' and posts the semaphore, each post
/// preempting into the consumer, which receives and prints the byte. Then a
/// one-shot application timer fires ('P'), the producer sleeps and wakes ('D'),
/// and the consumer's timed semaphore get times out ('T'). QEMU UART shows
/// "MABCPDT": 'M' = kernel entered; A/B/C = priority preemption + semaphore +
/// queue; P = the ur_timer interrupt + application timer; D = ur_thread_sleep +
/// the delayed list; T = a timed wait expiring (UR_NO_INSTANCE). Needs clang +
/// ld.lld; qemu-system-arm runs the UART check.
#[test]
#[ignore = "acceptance: cross-compiles + runs a freestanding Cortex-M URun slice (clang + ld.lld; optional qemu-system-arm)"]
fn stdlib_urun_slice_phase_qemu() {
    if !has_urun_toolchain() {
        eprintln!(
            "skipping: URun cross-build needs clang + ld.lld — environment gap, not a regression"
        );
        return;
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let caap = ["../target/debug/caap", "../target/release/caap"]
        .iter()
        .map(|r| manifest.join(r))
        .find(|p| p.is_file())
        .expect("caap binary (build the workspace first)");

    let tmp = std::env::temp_dir().join(format!("s2-urun-{}", std::process::id()));
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

    // build the whole project through its own assembler/build tool
    let project = corpus_path("examples/urun");
    let tool = corpus_path("examples/urun/ur_build.caap");
    let elf = tmp.join("urun.elf");
    let out = std::process::Command::new(&caap)
        .arg(&composed)
        .arg(&tool)
        .arg(&project)
        .arg(&elf)
        .arg("cortex-m3")
        .output()
        .expect("run ur_build");
    assert!(
        out.status.success() && elf.is_file(),
        "ur_build failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let bytes = std::fs::read(&elf).expect("read elf");
    assert_eq!(&bytes[0..4], b"\x7fELF", "produced an ELF");
    assert_eq!(
        u16::from_le_bytes([bytes[18], bytes[19]]),
        0x28,
        "e_machine = ARM (thumbv7m cross build)"
    );

    let uart = if on_path("qemu-system-arm") && on_path("timeout") {
        let run = std::process::Command::new("timeout")
            .args([
                "5",
                "qemu-system-arm",
                "-M",
                "mps2-an385",
                "-cpu",
                "cortex-m3",
                "-nographic",
                "-kernel",
            ])
            .arg(&elf)
            .output()
            .expect("run qemu");
        String::from_utf8_lossy(&run.stdout).into_owned()
    } else {
        eprintln!("note: qemu-system-arm/timeout absent — ELF verified statically only");
        String::new()
    };
    std::fs::remove_dir_all(&tmp).ok();

    if !uart.is_empty() {
        assert!(
            uart.starts_with("MABCPDT"),
            "QEMU UART should show \"MABCPDT\" (preemption + sem + queue, then app \
             timer 'P', sleep/wake 'D', timed-wait timeout 'T'): {uart:?}",
        );
    }
}

/// Fault injection: a deliberately MISCONFIGURED entry (one thread that blocks, NO
/// idle thread) must drive the scheduler into _ur_misconfig_halt and emit the
/// 0xDEADBEEF sentinel over UART — proving the U4 sentinel actually fires, not just
/// compiles. Reuses ur_build's entry arg (4th positional) to build `bad_no_idle.caap`
/// against the same modules. Self-skips without the toolchain; the QEMU assertion is
/// on RAW bytes (the sentinel is non-ASCII).
#[test]
#[ignore = "acceptance: fault-injection — a no-idle URun entry must emit the 0xDEADBEEF sentinel (clang + ld.lld; optional qemu-system-arm)"]
fn stdlib_urun_misconfig_sentinel_phase_qemu() {
    if !has_urun_toolchain() {
        eprintln!(
            "skipping: URun fault-injection cross-build needs clang + ld.lld — environment gap, not a regression"
        );
        return;
    }
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let caap = ["../target/debug/caap", "../target/release/caap"]
        .iter()
        .map(|r| manifest.join(r))
        .find(|p| p.is_file())
        .expect("caap binary (build the workspace first)");

    let tmp = std::env::temp_dir().join(format!("s2-urun-bad-{}", std::process::id()));
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

    let project = corpus_path("examples/urun");
    let tool = corpus_path("examples/urun/ur_build.caap");
    let elf = tmp.join("urun_bad.elf");
    let out = std::process::Command::new(&caap)
        .arg(&composed)
        .arg(&tool)
        .arg(&project)
        .arg(&elf)
        .arg("cortex-m3")
        .arg("bad_no_idle.caap") // the fault-injection entry (no idle thread)
        .output()
        .expect("run ur_build");
    assert!(
        out.status.success() && elf.is_file(),
        "ur_build (bad entry) failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let bytes = std::fs::read(&elf).expect("read elf");
    assert_eq!(&bytes[0..4], b"\x7fELF", "produced an ELF");

    if on_path("qemu-system-arm") && on_path("timeout") {
        let run = std::process::Command::new("timeout")
            .args([
                "5",
                "qemu-system-arm",
                "-M",
                "mps2-an385",
                "-cpu",
                "cortex-m3",
                "-nographic",
                "-kernel",
            ])
            .arg(&elf)
            .output()
            .expect("run qemu");
        let uart = run.stdout; // RAW bytes — the sentinel is non-ASCII
        std::fs::remove_dir_all(&tmp).ok();
        assert_eq!(
            uart.first(),
            Some(&0x4Du8),
            "UART should start with 'M' (kernel entered) before the halt: {uart:?}"
        );
        let sentinel = [0xDEu8, 0xAD, 0xBE, 0xEF];
        assert!(
            uart.windows(4).any(|w| w == sentinel.as_slice()),
            "the no-idle misconfig must emit the 0xDEADBEEF sentinel; UART bytes: {uart:?}"
        );
    } else {
        std::fs::remove_dir_all(&tmp).ok();
        eprintln!("note: qemu-system-arm/timeout absent — bad-entry ELF verified statically only");
    }
}

/// `stdlib.bare` — the reusable bare-metal wrapper layer (stdlib/bare/*). A
/// native program `(use …)`s the wrappers; the backend prep path INLINES the
/// wrapper defns into the translation unit and the OWN LLVM backend lowers them
/// to the underlying primitives — a `volatile` MMIO store, `cpsid/cpsie` IRQ
/// masking and `mrs/msr primask` save/restore. This proves the wrappers are
/// policy over EXISTING primitives (no kernel/backend change): each wrapper body
/// is an ordinary Call the backend already understands. The modules are
/// native-only by design (`volatile_*`/`asm` have no meaning in the eval
/// sandbox, so they are reached through native emit, never `(load …)`).
#[test]
fn stdlib_bare_wrappers_lower_through_native_backend() {
    // mmio + cpu (irq) + critical (save/restore), all pulled in by (use …)
    let prog = "(use stdlib.bare.mmio mmio_write32)\n\
                (use stdlib.bare.cpu irq_disable irq_enable)\n\
                (use stdlib.bare.critical critical_save critical_restore)\n\
                (bind UART0_DATA 1073758208)\n\
                (bind main (lambda ()\n\
                  (bind ((prev (critical_save)))\n\
                    (do\n\
                      (mmio_write32 UART0_DATA 65)\n\
                      (critical_restore prev)\n\
                      0))))\n";
    let path = std::env::temp_dir().join(format!("s2_bare_{}.caap", std::process::id()));
    std::fs::write(&path, prog).expect("write program");
    let p = path.to_str().unwrap();

    let (ir, diags) = emit_own_llvm("bare_wrappers_llvm", p);
    std::fs::remove_file(&path).ok();

    assert_eq!(
        diags, 0,
        "the bare wrappers lower with no diagnostics:\n{ir}"
    );
    // mmio_write32 -> a volatile store of the named width
    assert!(
        ir.contains("store volatile i32"),
        "mmio_write32 lowers to a volatile MMIO store:\n{ir}"
    );
    // cpu irq helpers -> the cpsid/cpsie inline asm (single-core mutual exclusion)
    assert!(
        ir.contains("asm sideeffect \"cpsid i\"") && ir.contains("asm sideeffect \"cpsie i\""),
        "the cpu IRQ helpers lower to cpsid/cpsie inline asm:\n{ir}"
    );
    // critical_save/restore -> PRIMASK read/write through asm output/input operands
    assert!(
        ir.contains("asm sideeffect \"mrs $0, primask\"")
            && ir.contains("asm sideeffect \"msr primask, $0\""),
        "the nesting-safe critical section lowers to mrs/msr primask:\n{ir}"
    );
    // each wrapper became a real inlined function in the TU
    assert!(
        ir.contains("define i32 @mmio_write32")
            && ir.contains("define i32 @critical_save")
            && ir.contains("define i32 @main"),
        "the wrapper defns are inlined into the native translation unit:\n{ir}"
    );
}

/// B4 — `stdlib.bare.atomic`: lock-free atomic-word helpers over the `atomic_*`
/// heads. A native program `(use …)`s the wrappers; the backend prep gate
/// accepts the heads (seeded into native_vocab) and the OWN LLVM backend lowers
/// them to LLVM atomics — `store atomic` / `atomicrmw add` / `cmpxchg` / `load
/// atomic`, all seq_cst. This is the SMP / multi-core mechanism a future target
/// needs (the single-core Cortex-M masks interrupts instead). Native-only by
/// design (`atomic_*` has no meaning in the eval sandbox).
#[test]
fn stdlib_bare_atomics_lower_through_native_backend() {
    // a u64 cell (the host's pointer width) exercised through every wrapper:
    // store 40, fetch-add 2 (=> 42), CAS 42->42 (succeeds), atomic-load, exit 42
    let prog = "(use stdlib.bare.atomic atomic_load64 atomic_store64 atomic_add64 atomic_cas64)\n\
                (bind main (lambda ()\n\
                  (bind ((cell (array_local 1 u64))\n\
                         (addr (ptr_to_int cell)))\n\
                    (do\n\
                      (atomic_store64 addr 40)\n\
                      (atomic_add64 addr 2)\n\
                      (atomic_cas64 addr 42 42)\n\
                      (cast (atomic_load64 addr) i32)))))\n";
    let path = std::env::temp_dir().join(format!("s2_atomic_{}.caap", std::process::id()));
    std::fs::write(&path, prog).expect("write program");
    let p = path.to_str().unwrap();

    let (ir, diags) = emit_own_llvm("bare_atomics_llvm", p);
    std::fs::remove_file(&path).ok();

    assert_eq!(
        diags, 0,
        "the atomic wrappers lower with no diagnostics:\n{ir}"
    );
    // each head lowers to its LLVM atomic, seq_cst
    assert!(
        ir.contains("store atomic i64") && ir.contains("seq_cst"),
        "atomic_store64 lowers to a `store atomic … seq_cst`:\n{ir}"
    );
    assert!(
        ir.contains("atomicrmw add ptr"),
        "atomic_add64 lowers to `atomicrmw add`:\n{ir}"
    );
    assert!(
        ir.contains("cmpxchg ptr") && ir.contains("seq_cst seq_cst"),
        "atomic_cas64 lowers to `cmpxchg … seq_cst seq_cst`:\n{ir}"
    );
    assert!(
        ir.contains("load atomic i64"),
        "atomic_load64 lowers to a `load atomic`:\n{ir}"
    );
    // each wrapper became a real inlined function in the TU
    assert!(
        ir.contains("define i64 @atomic_add64")
            && ir.contains("define i64 @atomic_cas64")
            && ir.contains("define i32 @main"),
        "the atomic wrapper defns are inlined into the native translation unit:\n{ir}"
    );
}

/// B4 end-to-end: the atomics program BUILDS with clang and RUNS with the
/// correct exit code (42) — the atomic ops are real, executable instructions,
/// not just text. Self-skips without clang.
#[test]
#[ignore = "acceptance: builds + runs a native program using atomics (clang) and asserts exit code 42"]
fn stdlib_bare_atomics_build_and_run() {
    if !on_path("clang") {
        eprintln!("skipping: clang unavailable");
        return;
    }
    let prog = "(use stdlib.bare.atomic atomic_load64 atomic_store64 atomic_add64 atomic_cas64)\n\
                (bind main (lambda ()\n\
                  (bind ((cell (array_local 1 u64))\n\
                         (addr (ptr_to_int cell)))\n\
                    (do\n\
                      (atomic_store64 addr 40)\n\
                      (atomic_add64 addr 2)\n\
                      (atomic_cas64 addr 42 99)\n\
                      (cast (atomic_load64 addr) i32)))))\n";
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let caap = ["../target/debug/caap", "../target/release/caap"]
        .iter()
        .map(|r| manifest.join(r))
        .find(|p| p.is_file())
        .expect("caap binary (build the workspace first)");

    let tmp = std::env::temp_dir().join(format!("s2-atomics-{}", std::process::id()));
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
    let progf = tmp.join("prog.caap");
    std::fs::write(&progf, prog).expect("write program");
    let bin = std::env::temp_dir().join(format!("s2-atomics-bin-{}", std::process::id()));

    let build = std::process::Command::new(&caap)
        .arg(&composed)
        .arg(stdlib_path("../tools/s2_build.caap"))
        .arg(&progf)
        .arg(&bin)
        .output()
        .expect("run s2_build");
    let built = build.status.success() && bin.is_file();
    if !built {
        std::fs::remove_dir_all(&tmp).ok();
        panic!(
            "native build failed:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
    }
    let run_code = std::process::Command::new(&bin)
        .output()
        .expect("run binary")
        .status
        .code();
    std::fs::remove_dir_all(&tmp).ok();
    std::fs::remove_file(&bin).ok();
    // store 40 -> add 2 (=42) -> CAS 42->99 succeeds (cell now 99)? No: the load
    // happens AFTER the CAS, so a successful CAS would make it 99. The CAS swaps
    // 42->99, then atomic_load64 reads 99. cast to i32 -> exit 99.
    assert_eq!(
        run_code,
        Some(99),
        "the atomics binary must run: store 40, +2=42, CAS(42->99) succeeds, load=99"
    );
}

// ── urun static-safety passes: each negative fixture must trip its pass ───────
// The five urun analysis passes (semantics/passes/ur_{norec,crit,isr,intrusive,
// create}) gate the urun native build. Here each is registered on a COW-cloned
// codegen session and run over a minimal kernel-defn fixture that violates
// exactly that invariant; a finding aborts at the native PREP gate (before
// codegen — no clang/linker needed), so these run in the default pool, not as
// acceptance. They are the regression net that proves the passes still FIRE
// (a clean urun build proving they don't false-positive is the acceptance slice).

/// Register one urun safety pass, emit a probe fixture through stdlib's native
/// gate, and return the joined diagnostic messages.
fn urun_pass_diag(pass_module: &str, fixture_rel: &str) -> String {
    let fix = corpus_path(fixture_rel);
    let mut compiler = common::codegen_bootstrapped_session();
    let src = format!(
        "(bind ((lm (get (ctfe_compiler_lookup_value compiler \"stdlib.load\") \"load_module\")))
           (do
             ((get (lm {pass_module:?}) \"register!\"))
             (bind ((res ((ctfe_compiler_lookup_value compiler \"stdlib.llvm.emit\")
                          (ctfe_compiler_load_surface_file_template compiler {fix:?}))))
               (sequence_join
                 (sequence_map (get res \"diagnostics\" (list_of))
                   (lambda (d) (get d \"message\" \"?\")))
                 \" | \"))))"
    );
    let graph = parse(&src).expect("parse urun pass probe");
    let unit = Unit::from_graph("urun_pass_probe", graph).expect("unit");
    match compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .expect("emit urun probe")
    {
        RuntimeValue::Str(s) => s.to_string(),
        other => panic!("expected joined diagnostics string, got {other:?}"),
    }
}

#[test]
fn urun_pass_ur_norec_rejects_recursion() {
    let msg = urun_pass_diag(
        "stdlib.semantics.passes.ur_norec",
        "fixtures/urun_probe_norec.caap",
    );
    assert!(
        msg.contains("ur-norec") && msg.contains("urun_probe_norec.caap:"),
        "ur-norec must reject the recursive defn: {msg}"
    );
}

#[test]
fn urun_pass_ur_crit_rejects_unbalanced_save() {
    let msg = urun_pass_diag(
        "stdlib.semantics.passes.ur_crit",
        "fixtures/urun_probe_crit.caap",
    );
    assert!(
        msg.contains("ur-crit") && msg.contains("urun_probe_crit.caap:"),
        "ur-crit must reject the unrestored critical section: {msg}"
    );
}

#[test]
fn urun_pass_ur_isr_rejects_blocking_from_handler() {
    let msg = urun_pass_diag(
        "stdlib.semantics.passes.ur_isr",
        "fixtures/urun_probe_isr.caap",
    );
    assert!(
        msg.contains("ur-isr") && msg.contains("urun_probe_isr.caap:"),
        "ur-isr must reject a blocking call from an _interrupt handler: {msg}"
    );
}

#[test]
fn urun_pass_ur_intrusive_rejects_double_push() {
    let msg = urun_pass_diag(
        "stdlib.semantics.passes.ur_intrusive",
        "fixtures/urun_probe_intrusive.caap",
    );
    assert!(
        msg.contains("ur-intrusive") && msg.contains("urun_probe_intrusive.caap:"),
        "ur-intrusive must reject the double push of one (list,node) pair: {msg}"
    );
}

#[test]
fn urun_pass_ur_create_rejects_double_create() {
    let msg = urun_pass_diag(
        "stdlib.semantics.passes.ur_create",
        "fixtures/urun_probe_create.caap",
    );
    assert!(
        msg.contains("ur-create") && msg.contains("urun_probe_create.caap:"),
        "ur-create must reject re-initialising a live control block: {msg}"
    );
}

#[test]
fn urun_pass_ur_suspend_rejects_missing_enqueue() {
    let msg = urun_pass_diag(
        "stdlib.semantics.passes.ur_suspend",
        "fixtures/urun_probe_suspend.caap",
    );
    assert!(
        msg.contains("ur-suspend") && msg.contains("urun_probe_suspend.caap:"),
        "ur-suspend must reject a park with no reachable waiter enqueue: {msg}"
    );
}

#[test]
fn urun_pass_ur_fastpath_rejects_unconditional_park() {
    let msg = urun_pass_diag(
        "stdlib.semantics.passes.ur_fastpath",
        "fixtures/urun_probe_fastpath.caap",
    );
    assert!(
        msg.contains("ur-fastpath") && msg.contains("urun_probe_fastpath.caap:"),
        "ur-fastpath must reject a suspend at conditional-depth 0: {msg}"
    );
}

// Silence (no-false-positive) probes: the two newest passes must stay QUIET on
// tricky-but-valid code, the complement of the urun clean build.
#[test]
fn urun_pass_ur_fastpath_silent_on_guarded_suspend() {
    let msg = urun_pass_diag(
        "stdlib.semantics.passes.ur_fastpath",
        "fixtures/urun_ok_fastpath.caap",
    );
    assert!(
        !msg.contains("ur-fastpath"),
        "ur-fastpath must stay silent on a conditionally-guarded suspend: {msg}"
    );
}

#[test]
fn urun_pass_ur_suspend_silent_on_paired_park() {
    let msg = urun_pass_diag(
        "stdlib.semantics.passes.ur_suspend",
        "fixtures/urun_ok_suspend.caap",
    );
    assert!(
        !msg.contains("ur-suspend"),
        "ur-suspend must stay silent on an enqueue-then-park pair: {msg}"
    );
}
