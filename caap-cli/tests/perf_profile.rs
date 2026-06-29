//! TEMPORARY perf-profiling harness (C7 perf unit). Times the CAAP compile/load
//! path stage-by-stage via `run_cli` (the same in-process entry the criterion
//! bench uses), so a breakdown can be written into `docs/perf-notes.md`.
//!
//! Run explicitly:
//!   CAAP_RUN_PERF_PROFILE=1 cargo test -p caap-cli --test perf_profile -- --nocapture
//!
//! NOTE: this file is throwaway scaffolding for the profiling report; it is not
//! part of the product. Numbers are printed to stdout.

use std::path::PathBuf;
use std::time::Instant;

use caap_cli::commands::run_cli;

const REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

fn root_join(rel: &str) -> String {
    PathBuf::from(REPO_ROOT)
        .join(rel)
        .to_string_lossy()
        .into_owned()
}

fn write_tmp(name: &str, contents: &str) -> String {
    let path = std::env::temp_dir().join(format!("caap-perf-{}-{name}", std::process::id()));
    std::fs::write(&path, contents).expect("write tmp");
    path.to_string_lossy().into_owned()
}

/// Run `run_cli(args)` once, asserting success, returning elapsed seconds.
fn time_run(label: &str, args: &[String]) -> f64 {
    let mut out = Vec::new();
    let mut err = Vec::new();
    let t = Instant::now();
    let code = run_cli(args.iter().cloned(), &mut out, &mut err);
    let secs = t.elapsed().as_secs_f64();
    // An int program-result becomes the exit code (CLI contract). Only the
    // usage (2) and runtime (70) error codes indicate an actual failure.
    if code == 2 || code == 70 {
        panic!(
            "{label}: exit {code}\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err),
        );
    }
    secs
}

/// Median of N runs (so JIT/cache warmup and noise don't dominate).
fn median_run(label: &str, args: &[String], n: usize) -> f64 {
    let mut samples: Vec<f64> = (0..n).map(|_| time_run(label, args)).collect();
    samples.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let med = samples[samples.len() / 2];
    println!(
        "{label:<46} median={med:7.3}s  (min={:.3} max={:.3}, n={n})",
        samples[0],
        samples[samples.len() - 1],
    );
    med
}

#[test]
fn perf_breakdown() {
    if std::env::var_os("CAAP_RUN_PERF_PROFILE").is_none() {
        eprintln!("skipping perf profile; set CAAP_RUN_PERF_PROFILE=1 to run it");
        return;
    }

    let bootstrap = root_join("stdlib/bootstrap.caap");
    let compose_native = root_join("tools/compose_native.caap");
    let compose_native_nopeval = root_join("tools/compose_native_nopeval.caap");
    let compose_peval = root_join("tools/compose_peval.caap");
    let s2_emit = root_join("tools/s2_emit.caap");

    // Trivial program: bare-kernel eval baseline + a stdlib-result program.
    let trivial = write_tmp("trivial.caap", "(int_add 40 2)\n");

    // A program that LOADS a real, mid-size stdlib module by name (sequence is
    // already loaded at bootstrap, so load string + json to force fresh loads
    // through the loader's per-module passes). Use stdlib.load directly.
    let stdlib = root_join("stdlib");
    let load_modules = write_tmp(
        "load_modules.caap",
        &format!(
            r#"(bind ((api (ctfe_compiler_lookup_value compiler "stdlib.load"))
        (load (get api "load")))
  (do
    (load "{stdlib}/lib/text/string.caap")
    (load "{stdlib}/lib/collections/result.caap")
    (load "{stdlib}/lib/collections/option.caap")
    (load "{stdlib}/lib/collections/set.caap")
    (load "{stdlib}/lib/text/json.caap")
    0))
"#
        ),
    );

    // A small self-contained native program (defn + main + final expr).
    let native_prog = write_tmp(
        "native_prog.caap",
        "(defn add ((a i32) (b i32)) i32 (int_add a b))\n\
         (bind main (lambda () (add 40 2)))\n(main)\n",
    );

    println!("\n=== CAAP compile/load path breakdown (debug build, in-process run_cli) ===\n");

    // 1. Bare kernel: process + eval, no stdlib at all.
    let bare = median_run(
        "1. bare kernel (no stdlib)",
        std::slice::from_ref(&trivial),
        5,
    );

    // 2. Bootstrap only + trivial program. Bootstrap cost ~= this minus bare.
    let boot = median_run(
        "2. bootstrap + trivial",
        &[bootstrap.clone(), trivial.clone()],
        5,
    );

    // 3. Bootstrap + load 5 stdlib modules (NO peval registered).
    let boot_load = median_run(
        "3. bootstrap + load 5 modules",
        &[bootstrap.clone(), load_modules.clone()],
        5,
    );

    // 4. Bootstrap + peval registered + load the SAME 5 modules. The delta vs
    //    (3) is the peval fixpoint cost per module load.
    let peval_load = median_run(
        "4. bootstrap+PEVAL + load 5 modules",
        &[compose_peval.clone(), load_modules.clone()],
        5,
    );

    // 5. Full native emit: compose_native (loads codegen layer + registers
    //    peval) + s2_emit on the tiny native program -> LLVM IR. Few samples
    //    (slow): this is the bench scenario.
    let native = median_run(
        "5. native emit (compose_native + s2_emit)",
        &[compose_native.clone(), s2_emit.clone(), native_prog.clone()],
        3,
    );

    // 6. Native emit WITHOUT peval registered: isolates codegen-layer module
    //    loading from the peval fixpoint. (5 - 6) = peval cost on the native path.
    let native_nopeval = median_run(
        "6. native emit, NO peval (codegen layer only)",
        &[
            compose_native_nopeval.clone(),
            s2_emit.clone(),
            native_prog.clone(),
        ],
        3,
    );

    println!("\n--- derived deltas ---");
    println!(
        "peval on native path (5 - 6) = {:7.3}s",
        native - native_nopeval
    );
    println!(
        "codegen-layer load+emit, no peval (6 - 2) = {:7.3}s",
        native_nopeval - boot
    );
    println!("bootstrap cost   (2 - 1) = {:7.3}s", boot - bare);
    println!("5-module load    (3 - 2) = {:7.3}s", boot_load - boot);
    println!(
        "PEVAL fixpoint   (4 - 3) = {:7.3}s   ({:.2}x the bare load)",
        peval_load - boot_load,
        if boot_load - boot > 0.0 {
            (peval_load - boot) / (boot_load - boot)
        } else {
            0.0
        }
    );
    println!(
        "native-emit layer load + emit (5 - 2) = {:7.3}s",
        native - boot
    );
    println!();
}
