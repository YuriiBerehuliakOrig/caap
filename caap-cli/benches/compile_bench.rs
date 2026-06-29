//! Benchmark for the full CAAP compile pipeline on stdlib's own backend.
//!
//! Drives `run_cli` end-to-end with `tools/s2_emit.caap` as the target, which
//! exercises: host registration, stdlib bootstrap loading, PEG parsing, CTFE
//! evaluation, the load-time check/typecheck gate, and LLVM-IR lowering — the
//! whole frontend, without paying for clang/link.
//!
//! Run with profiling:
//!   cargo bench -p caap-cli --bench compile_bench -- --profile-time 10

use std::path::{Path, PathBuf};

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pprof::criterion::{Output, PProfProfiler};

use caap_cli::commands::run_cli;

const S2_BOOTSTRAP: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../stdlib/bootstrap.caap");
const S2_NATIVE_EMIT: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../stdlib/boot/native_emit.caap"
);
const S2_EMIT_TOOL: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../tools/s2_emit.caap");

/// A small self-contained native program (`defn` + `main` + final expr) written
/// to the temp dir, so the benchmark depends on no external corpus file.
const SAMPLE_PROGRAM: &str =
    "(defn add ((a i32) (b i32)) i32 (int_add a b))\n(bind main (lambda () (add 40 2)))\n(main)\n";

/// Compose stdlib's bootstrap with the native-emit leg (which registers the
/// own backend, `stdlib.backend.emit.llvm`) into one sys-authorized policy file — the
/// same shape the CLI scenarios use. Written once to the temp dir.
fn composed_bootstrap() -> PathBuf {
    let forms = format!(
        "(do \
           (ctfe_compiler_execute_bootstrap_file compiler {S2_BOOTSTRAP:?} (list_of \"sys\")) \
           (ctfe_compiler_execute_bootstrap_file compiler {S2_NATIVE_EMIT:?} (list_of \"sys\")))"
    );
    let path = std::env::temp_dir().join("caap-compile-bench-composed.caap");
    std::fs::write(&path, forms).expect("write composed bootstrap");
    path
}

/// Write the sample native program to the temp dir and return its path.
fn sample_program() -> PathBuf {
    let path = std::env::temp_dir().join("caap-compile-bench-program.caap");
    std::fs::write(&path, SAMPLE_PROGRAM).expect("write sample program");
    path
}

fn emit_args(composed: &Path, program: &Path) -> Vec<String> {
    vec![
        composed.display().to_string(),
        S2_EMIT_TOOL.into(),
        program.display().to_string(),
    ]
}

fn bench_compile_emit(c: &mut Criterion) {
    let composed = composed_bootstrap();
    let program = sample_program();
    let args = emit_args(&composed, &program);

    let mut group = c.benchmark_group("compile/emit");
    // Slow: reduce sample count so criterion doesn't run for hours.
    group.sample_size(10);

    group.bench_function("native_emit", |b| {
        b.iter(|| {
            let mut out = Vec::new();
            let mut err = Vec::new();
            black_box(run_cli(args.clone(), &mut out, &mut err))
        })
    });

    group.finish();
}

criterion_group!(
    name = benches;
    config = Criterion::default()
        .with_profiler(PProfProfiler::new(997, Output::Flamegraph(None)));
    targets = bench_compile_emit,
);
criterion_main!(benches);
