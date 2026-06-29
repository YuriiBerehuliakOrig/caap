//! End-to-end scenarios for the flagless CAAP CLI.
//!
//! The CLI is a launcher: `caap BOOTSTRAP PROGRAM [ARG…]`. Everything the old
//! subcommands did (check, llvm_ir, native builds) is now a PROGRAM you run —
//! see `tools/*.caap` — and multi-bootstrap setups are ordinary composed
//! bootstrap files. An int program result is the process exit code (the same
//! contract natively compiled programs follow); a string result is printed.

use std::fs;
use std::path::{Path, PathBuf};

use caap_cli::commands::run_cli;

fn temp_path(name: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "caap-cli-scenario-{}-{}-{name}",
            std::process::id(),
            std::thread::current().name().unwrap_or("thread")
        ))
        .to_string_lossy()
        .to_string()
}

fn tool(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tools")
        .join(name)
        .display()
        .to_string()
}

/// Compose the stdlib bootstrap with extra bootstrap files into one policy
/// file — the new-CLI equivalent of the old repeated `--bootstrap` flag. Each
/// part executes with the same full `sys` authority the CLI grants the
/// composed file itself.
fn composed_bootstrap(name: &str, extras: &[&str]) -> String {
    // Compose ONLY the given bootstraps (each with sys authority). Callers
    // pass stdlib/bootstrap.caap as the first extra — no implicit v1 leg.
    let forms: Vec<String> = extras
        .iter()
        .map(|extra| {
            format!("(ctfe_compiler_execute_bootstrap_file compiler {extra:?} (list_of \"sys\"))")
        })
        .collect();
    let path = temp_path(&format!("composed-{name}.caap"));
    fs::write(&path, format!("(do {})", forms.join(" "))).unwrap();
    path
}

fn stdlib_path(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../stdlib")
        .join(name)
        .display()
        .to_string()
}

fn run(args: &[&str]) -> (i32, String, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let code = run_cli(
        args.iter().map(|arg| arg.to_string()),
        &mut stdout,
        &mut stderr,
    );
    (
        code,
        String::from_utf8(stdout).unwrap(),
        String::from_utf8(stderr).unwrap(),
    )
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[test]
fn cli_launch_grants_sys_capability_to_bootstraps() {
    let path = temp_path("llvm_cap_source.caap");
    let emitter = temp_path("llvm_cap_emitter.caap");
    fs::write(
        &path,
        r#"
          (module "demo.cli_llvm_cap_source")
          (int_add 1 2)
        "#,
    )
    .unwrap();
    fs::write(
        &emitter,
        r#"
          (ctfe_compiler_register_value
            compiler
            "demo.cap_count"
            (size
              (get
                (ctfe_compiler_current_bootstrap_context compiler)
                "capabilities")))
          (ctfe_compiler_register_value
            compiler
            "demo.cap_emit"
            (lambda (unit)
              (map_of
                "text"
                  (int_to_string (ctfe_compiler_lookup_value compiler "demo.cap_count"))
                "diagnostics"
                  (list_of))))
        "#,
    )
    .unwrap();
    let s2_bootstrap = stdlib_path("bootstrap.caap");
    let bootstrap = composed_bootstrap("cap-count", &[&s2_bootstrap, &emitter]);
    let emit = tool("s2_emit.caap");

    let (code, stdout, stderr) = run(&[&bootstrap, &emit, &path, "demo.cap_emit"]);
    fs::remove_file(&path).ok();
    fs::remove_file(&emitter).ok();
    fs::remove_file(&bootstrap).ok();

    assert_eq!(code, 0, "{stderr}");
    assert_eq!(stdout, "1\n");
}

/// Named SURFACE MODULES + the checked run command: a clike file headed
/// `(surface stdlib.frontend.clike demo.rounds)` is discovered BY NAME and
/// loads as a module (functions exported, main NOT run); and
/// run_source_checked returns failures as DATA.
#[test]
fn cli_stdlib_named_surface_module_and_checked_run() {
    let s2_bootstrap = stdlib_path("bootstrap.caap");
    let composed = composed_bootstrap("s2-named-surface", &[&s2_bootstrap]);
    let dir = temp_path("s2_surface_mods");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        format!("{dir}/rounds.caap"),
        "(surface stdlib.frontend.clike demo.rounds)\npub twice (n i32) i32 = { n * 2; }\n",
    )
    .unwrap();
    let typo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/typo.caap")
        .display()
        .to_string();
    let driver = temp_path("s2_named_driver.caap");
    fs::write(
        &driver,
        format!(
            "(bind api (ctfe_compiler_lookup_value compiler \"stdlib.load\"))
             (bind caps (ctfe_compiler_lookup_value compiler \"caap.session.commands\"))
             (bind r ((get caps \"run_source_checked\" null) {typo:?}))
             (do ((get api \"discover\" null) {dir:?})
                 (bind m ((get api \"load_module\" null) \"demo.rounds\")
                   (if (and (eq ((get m \"twice\" null) 21) 42)
                            (and (eq (get r \"ok\" true) false)
                                 (string_contains
                                   (get (get r \"diagnostics\" (list_of)) 0 \"\")
                                   \"sequence_fold_lett\")))
                     0
                     1)))"
        ),
    )
    .unwrap();
    let (code, _stdout, stderr) = run(&[&composed, &driver]);
    fs::remove_file(&driver).ok();
    fs::remove_file(format!("{dir}/rounds.caap")).ok();
    fs::remove_dir(&dir).ok();
    fs::remove_file(&composed).ok();
    assert_eq!(code, 0, "named surface module + checked run: {stderr}");
}

/// A6: an else-less `if` cannot be a clike function's return value (it
/// would silently yield 0 on the false branch) — a located load error.
#[test]
fn cli_stdlib_clike_elseless_if_tail_errors() {
    let s2_bootstrap = stdlib_path("bootstrap.caap");
    let composed = composed_bootstrap("s2-a6", &[&s2_bootstrap]);
    let bad = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/elseless_tail.caap")
        .display()
        .to_string();
    let driver = temp_path("s2_a6_driver.caap");
    fs::write(
        &driver,
        format!(
            "(bind api (ctfe_compiler_lookup_value compiler \"stdlib.load\"))
             (bind r (try (do ((get api \"load\" null) {bad:?}) \"LOADED\")
                       (catch e (get e \"message\" (value_to_string e)))))
             (if (string_contains r \"else-less `if` cannot be a block's value\")
               0 1)"
        ),
    )
    .unwrap();
    let (code, _o, stderr) = run(&[&composed, &driver]);
    fs::remove_file(&driver).ok();
    fs::remove_file(&composed).ok();
    assert_eq!(code, 0, "else-less if in tail must error: {stderr}");
}

/// stdlib compile_ir, ONE CALL: a driver program BUILDS the specs at run
/// time (lib/ast builders), hands them to stdlib.backend.driver compile_ir, and
/// gets a native binary — render + own-LLVM lowering + clang driven inside
/// the call (host services resolve lazily under the composed bootstrap's sys
/// authority). The generated binary exits 42.
#[test]
fn cli_stdlib_compile_ir_builds_specs_to_binary() {
    if find_executable("clang").is_none() {
        eprintln!("skipping: clang not on PATH");
        return;
    }
    let s2_bootstrap = stdlib_path("bootstrap.caap");
    let s2_native_emit = stdlib_path("boot/native_emit.caap");
    let composed = composed_bootstrap("s2-compile-ir", &[&s2_bootstrap, &s2_native_emit]);
    let driver = temp_path("s2_compile_ir_driver.caap");
    let out = temp_path("s2_compile_ir_bin");
    fs::write(
        &driver,
        format!(
            "(bind bk (ctfe_compiler_lookup_value compiler \"stdlib.backend.driver\"))
             (bind ast (ctfe_compiler_lookup_value compiler \"stdlib.syntax.ast\"))
             (bind res ((get bk \"compile_ir\" null)
               (list_of
                 ((get ast \"calln\" null) \"bind\"
                   (list_of (syntax_name \"main\")
                     ((get ast \"lam\" null) (list_of)
                       ((get ast \"calln\" null) \"int_add\"
                         (list_of ((get ast \"lit\" null) 30)
                                  ((get ast \"lit\" null) 12))))))
                 ((get ast \"calln\" null) \"main\" (list_of)))
               {out:?} \"none\"))
             (if (get res \"ok\" false) 0 (runtime_error (value_to_string res)))"
        ),
    )
    .unwrap();

    let (code, _stdout, stderr) = run(&[&composed, &driver]);
    assert_eq!(code, 0, "compile_ir driver: {stderr}");
    let status = std::process::Command::new(&out)
        .status()
        .expect("run generated binary");
    for f in [&driver, &out, &composed] {
        fs::remove_file(f).ok();
    }
    assert_eq!(status.code(), Some(42), "specs -> binary in one call");
}

/// W4/A3a: the native driver's `find_runtime_lib` must NOT silently shell out
/// to `cargo build` when the caap-sys-runtime staticlib is missing. With every
/// candidate path absent and the explicit `CAAP_SYS_RUNTIME_AUTOBUILD` opt-in
/// OFF, it returns null (so `link_ir!`'s "set CAAP_SYS_RUNTIME_LIB" diagnostic
/// fires) instead of performing a hidden build. We run a real `caap` subprocess
/// from an empty CWD with `CAAP_SYS_RUNTIME_LIB`/`CARGO_TARGET_DIR` aimed at
/// non-existent locations, so the relative `target/`/`../target/` candidates
/// cannot accidentally resolve the dev-tree lib.
#[test]
fn cli_native_driver_no_silent_autobuild_when_lib_absent() {
    let s2_bootstrap = stdlib_path("bootstrap.caap");
    let s2_native_emit = stdlib_path("boot/native_emit.caap");
    let composed = composed_bootstrap("s2-no-autobuild", &[&s2_bootstrap, &s2_native_emit]);

    // An empty scratch dir: used as BOTH the process CWD (so relative
    // `target/debug/...` candidates miss) and as a fake CARGO_TARGET_DIR (which
    // also has no `debug/libcaap_sys_runtime_ffi.a`).
    let scratch = temp_path("no_autobuild_scratch");
    fs::create_dir_all(&scratch).unwrap();
    let missing_lib = format!("{scratch}/does-not-exist.a");

    let driver = temp_path("no_autobuild_driver.caap");
    let out = format!("{scratch}/probe_bin");
    // Always-on leg: with the lib absent and autobuild off, find_runtime_lib
    // MUST be null — proving no silent cargo build resolved a path. This needs
    // no clang. When clang IS present, additionally exercise link_ir! and
    // require its "set CAAP_SYS_RUNTIME_LIB" remediation diagnostic to fire
    // (rather than a hidden build).
    let want_diagnostic = find_executable("clang").is_some();
    let body = if want_diagnostic {
        // Valid LLVM IR whose `main` calls an undefined external — clang `-c`
        // succeeds, so the FAILURE happens at the LINK step (the undefined
        // symbol cannot resolve without the runtime staticlib). Because
        // `find_runtime_lib` is null, `link_ir!` appends the remediation hint.
        // In the Rust source `\\n` writes the two chars `\n` into the .caap
        // file, which the CAAP reader then decodes to a real newline in the IR.
        let ir = "declare i32 @caap_runtime_missing_symbol()\\n\
                  define i32 @main() {\\n  %r = call i32 @caap_runtime_missing_symbol()\\n  ret i32 %r\\n}\\n";
        format!(
            "(bind bk (ctfe_compiler_lookup_value compiler \"stdlib.backend.driver\"))
             (bind found ((get bk \"find_runtime_lib\" null)))
             (if (not (eq found null))
               (runtime_error (string_concat_many \"expected null, got: \" found))
               (bind r ((get bk \"link_ir!\" null) \"{ir}\" {out:?})
                 (if (and (eq (get r \"ok\" true) false)
                          (string_contains (get r \"error\" \"\")
                            \"set CAAP_SYS_RUNTIME_LIB\"))
                   0
                   (runtime_error (value_to_string r)))))"
        )
    } else {
        "(bind bk (ctfe_compiler_lookup_value compiler \"stdlib.backend.driver\"))
             (if (eq ((get bk \"find_runtime_lib\" null)) null) 0
               (runtime_error \"expected find_runtime_lib to be null with autobuild off\"))"
            .to_string()
    };
    fs::write(&driver, body).unwrap();

    // A REAL subprocess (not in-process run_cli) so we can pin the CWD and env.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_caap"))
        .arg(&composed)
        .arg(&driver)
        .current_dir(&scratch)
        .env("CAAP_SYS_RUNTIME_LIB", &missing_lib)
        .env("CARGO_TARGET_DIR", &scratch)
        .env_remove("CAAP_SYS_RUNTIME_AUTOBUILD")
        .output()
        .expect("run caap subprocess");

    for f in [&driver, &composed] {
        fs::remove_file(f).ok();
    }
    fs::remove_file(&out).ok();
    fs::remove_dir_all(&scratch).ok();

    assert_eq!(
        output.status.code(),
        Some(0),
        "lib absent + autobuild off: find_runtime_lib must be null{} (no silent build).\n\
         stdout: {}\nstderr: {}",
        if want_diagnostic {
            " and link_ir! must surface the 'set CAAP_SYS_RUNTIME_LIB' diagnostic"
        } else {
            ""
        },
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The OPT-IN partial-evaluation leg (`boot/peval.caap`), composed after the base
/// bootstrap, registers the `peval` load-time transform — so a module loaded
/// afterwards is partially evaluated while results stay identical. The base
/// bootstrap (without the leg) does NOT register it: PE is opt-in because the
/// fixpoint adds load-time cost, so the default bootstrap stays fast.
#[test]
fn cli_peval_leg_enables_partial_evaluation() {
    let bootstrap = stdlib_path("bootstrap.caap");
    let peval_leg = stdlib_path("boot/peval.caap");
    let dir = temp_path("pe_mod");
    fs::create_dir_all(&dir).unwrap();
    let module = format!("{dir}/m.caap");
    fs::write(
        &module,
        "(module demo.pe)\n\
         (bind f (lambda () (bind ((x 2) (y (int_mul x 3))) (int_add y 1))))\n\
         (export f)\n",
    )
    .unwrap();

    // WITH the leg: PE is registered and the (folded) module still returns 7.
    let driver = temp_path("pe_driver.caap");
    fs::write(
        &driver,
        format!(
            "(bind peval (ctfe_compiler_lookup_value compiler \"stdlib.semantics.passes.peval\" null))
             (bind api   (ctfe_compiler_lookup_value compiler \"stdlib.load\"))
             (bind m     ((get api \"load\" null) {module:?}))
             (if (and (not (eq peval null)) (eq ((get m \"f\" null)) 7)) 0 1)"
        ),
    )
    .unwrap();
    let composed = composed_bootstrap("peval-on", &[&bootstrap, &peval_leg]);
    let (code, _o, stderr) = run(&[&composed, &driver]);
    assert_eq!(
        code, 0,
        "peval leg should register PE and keep results correct: {stderr}"
    );

    // WITHOUT the leg: the base bootstrap does NOT register PE.
    let base = composed_bootstrap("peval-off", &[&bootstrap]);
    let off_driver = temp_path("pe_off_driver.caap");
    fs::write(
        &off_driver,
        "(if (eq (ctfe_compiler_lookup_value compiler \"stdlib.semantics.passes.peval\" null) null) 0 1)",
    )
    .unwrap();
    let (off_code, _o2, off_stderr) = run(&[&base, &off_driver]);
    assert_eq!(
        off_code, 0,
        "base bootstrap must NOT have PE registered: {off_stderr}"
    );

    for f in [&driver, &off_driver, &module, &composed, &base] {
        fs::remove_file(f).ok();
    }
    fs::remove_dir(&dir).ok();
}

/// The OPT-IN polyvariant-specialization leg (`boot/pe.caap`), composed after the
/// base bootstrap, registers the `pe` load-time transform — so a module loaded
/// afterwards is specialized (literal-static calls fold or redirect to variants).
/// pe is BEHAVIOUR-PRESERVING (a variant is the partial evaluation of the original,
/// and it runs peval over every form), so results stay identical. The base
/// bootstrap (without the leg) does NOT register it: pe is opt-in because
/// specialization adds load-time cost (and the (static_params …) form only exists
/// once pe loads).
#[test]
fn cli_pe_leg_registers_specialization_transform() {
    let bootstrap = stdlib_path("bootstrap.caap");
    let pe_leg = stdlib_path("boot/pe.caap");
    let dir = temp_path("pe_spec_mod");
    fs::create_dir_all(&dir).unwrap();
    // A module loaded under pe: results are preserved (the body folds to 7, which
    // is what `f` returned before), proving pe is registered and behaviour-safe.
    let module = format!("{dir}/m.caap");
    fs::write(
        &module,
        "(module demo.pe_noop)\n\
         (bind f (lambda () (bind ((x 2) (y (int_mul x 3))) (int_add y 1))))\n\
         (export f)\n",
    )
    .unwrap();

    // WITH the leg: pe is registered (the 3-arg lookup is non-null) and the
    // specialized module still returns 7 (behaviour-preserving).
    let driver = temp_path("pe_driver.caap");
    fs::write(
        &driver,
        format!(
            // 3-arg lookup with a null default — the 2-arg form THROWS on a miss.
            "(bind pe (ctfe_compiler_lookup_value compiler \"stdlib.semantics.passes.pe\" null))
             (bind api (ctfe_compiler_lookup_value compiler \"stdlib.load\"))
             (bind m   ((get api \"load\" null) {module:?}))
             (if (and (not (eq pe null)) (eq ((get m \"f\" null)) 7)) 0 1)"
        ),
    )
    .unwrap();
    let composed = composed_bootstrap("pe-on", &[&bootstrap, &pe_leg]);
    let (code, _o, stderr) = run(&[&composed, &driver]);
    assert_eq!(
        code, 0,
        "pe leg should register the transform and keep results correct: {stderr}"
    );

    // WITHOUT the leg: the base bootstrap does NOT register pe.
    let base = composed_bootstrap("pe-off", &[&bootstrap]);
    let off_driver = temp_path("pe_off_driver.caap");
    fs::write(
        &off_driver,
        "(if (eq (ctfe_compiler_lookup_value compiler \"stdlib.semantics.passes.pe\" null) null) 0 1)",
    )
    .unwrap();
    let (off_code, _o2, off_stderr) = run(&[&base, &off_driver]);
    assert_eq!(
        off_code, 0,
        "base bootstrap must NOT have pe registered: {off_stderr}"
    );

    for f in [&driver, &off_driver, &module, &composed, &base] {
        fs::remove_file(f).ok();
    }
    fs::remove_dir(&dir).ok();
}

/// Composing the native/codegen leg (`boot/native_emit.caap`) after the base
/// bootstrap also registers the `peval` load-time transform — so a native build
/// gets PE-optimized IR. PE is behaviour-preserving (constant folding + DCE), so
/// codegen still produces a correct, runnable binary. This proves both halves:
/// `peval` is registered AND a codegen path still works (the binary exits 42).
#[test]
fn cli_native_emit_leg_enables_partial_evaluation() {
    if find_executable("clang").is_none() {
        eprintln!("skipping: clang not on PATH");
        return;
    }
    let bootstrap = stdlib_path("bootstrap.caap");
    let native_emit = stdlib_path("boot/native_emit.caap");
    let composed = composed_bootstrap("native-emit-pe", &[&bootstrap, &native_emit]);
    let driver = temp_path("native_emit_pe_driver.caap");
    let out = temp_path("native_emit_pe_bin");
    fs::write(
        &driver,
        format!(
            // The native leg must have registered peval (3-arg lookup with a null
            // default — lookup THROWS on a missing key), and a codegen path must
            // still build a runnable binary that exits 42.
            "(bind peval (ctfe_compiler_lookup_value compiler \"stdlib.semantics.passes.peval\" null))
             (bind bk (ctfe_compiler_lookup_value compiler \"stdlib.backend.driver\"))
             (bind ast (ctfe_compiler_lookup_value compiler \"stdlib.syntax.ast\"))
             (bind res ((get bk \"compile_ir\" null)
               (list_of
                 ((get ast \"calln\" null) \"bind\"
                   (list_of (syntax_name \"main\")
                     ((get ast \"lam\" null) (list_of)
                       ((get ast \"calln\" null) \"int_add\"
                         (list_of ((get ast \"lit\" null) 30)
                                  ((get ast \"lit\" null) 12))))))
                 ((get ast \"calln\" null) \"main\" (list_of)))
               {out:?} \"none\"))
             (if (and (not (eq peval null)) (get res \"ok\" false)) 0
               (runtime_error (value_to_string res)))"
        ),
    )
    .unwrap();

    let (code, _stdout, stderr) = run(&[&composed, &driver]);
    assert_eq!(
        code, 0,
        "native_emit leg should register peval and still build: {stderr}"
    );
    let status = std::process::Command::new(&out)
        .status()
        .expect("run generated binary");
    for f in [&driver, &out, &composed] {
        fs::remove_file(f).ok();
    }
    assert_eq!(
        status.code(),
        Some(42),
        "PE is behaviour-preserving: native build still exits 42"
    );
}
