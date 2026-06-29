//! Benchmarks for the CAAP evaluator and query/compiler pipeline.
//!
//! Groups:
//!   parse/*       — text → ParsedSource and text → IRGraph latency
//!   eval/*        — evaluator dispatch: arithmetic, closures, recursion, collections
//!   compiler/*    — Compiler session overhead and query engine throughput
//!
//! Run:
//!   cargo bench -p caap-core
//!   cargo bench -p caap-core -- eval/recursion
//!   cargo bench -p caap-core -- --save-baseline main
//!   cargo bench -p caap-core -- --baseline main

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use pprof::criterion::{Output, PProfProfiler};

use caap_core::{
    frontend::{parse, parse_forms},
    CompilerHost, Evaluator, PhasePolicy, Unit,
};

// ── Source fixtures ───────────────────────────────────────────────────────────

fn src_arithmetic_simple() -> &'static str {
    "(int_add 1 (int_mul 2 3))"
}

fn src_arithmetic_chain() -> &'static str {
    "(int_add (int_add (int_add (int_add (int_add 1 2) 3) 4) 5) \
              (int_mul (int_mul (int_mul 2 3) 4) 5))"
}

fn src_closure_call() -> &'static str {
    "((lambda (x y) (int_add x y)) 40 2)"
}

fn src_bind_heavy() -> &'static str {
    "(bind (
       (a 1) (b 2) (c 3) (d 4) (e 5)
       (f 6) (g 7) (h 8) (i 9) (j 10)
     )
     (int_add (int_add (int_add a b) (int_add c d))
              (int_add (int_add e f) (int_add g h))))"
}

fn src_recursion_fib(n: u32) -> String {
    format!(
        "(bind (
           (fib (lambda (n)
             (if (lt n 2)
               n
               (int_add (fib (int_sub n 1)) (fib (int_sub n 2))))))
         )
         (fib {n}))"
    )
}

fn src_list_map() -> &'static str {
    "(bind (
       (xs (list_of 1 2 3 4 5 6 7 8 9 10
                    11 12 13 14 15 16 17 18 19 20))
       (double (lambda (x) (int_mul x 2)))
     )
     (sequence_map xs double))"
}

fn src_map_ops() -> &'static str {
    "(bind (
       (m (map_of \"a\" 1 \"b\" 2 \"c\" 3 \"d\" 4 \"e\" 5))
     )
     (int_add (int_add (get m \"a\" 0) (get m \"b\" 0))
              (int_add (get m \"c\" 0) (get m \"d\" 0))))"
}

fn src_string_ops() -> &'static str {
    "(string_concat_many \"hello\" \" \" \"world\" \"!\" \" foo\" \" bar\" \" baz\")"
}

fn src_medium_program() -> String {
    // A realistic-size program with multiple top-level bindings and logic.
    let mut s = String::from("(bind (\n");
    for i in 0..20usize {
        s.push_str(&format!("  (v{i} (int_add {i} {}))\n", i + 1));
    }
    s.push_str(")\n");
    s.push_str("(int_add v0 v19))");
    s
}

fn src_large_program() -> String {
    // Many nested bindings and calls.
    let mut s = String::from("(bind (\n");
    for i in 0..100usize {
        s.push_str(&format!("  (x{i} (int_mul {i} {}))\n", i + 2));
    }
    s.push_str(")\n");
    s.push_str("(int_add x0 x99))");
    s
}

// ── 1. Parse benchmarks ───────────────────────────────────────────────────────

fn bench_parse(c: &mut Criterion) {
    let medium = src_medium_program();
    let large = src_large_program();

    let mut group = c.benchmark_group("parse");

    group.bench_function("forms/simple", |b| {
        b.iter(|| parse_forms(black_box(src_arithmetic_simple())).unwrap())
    });
    group.bench_function("forms/medium", |b| {
        b.iter(|| parse_forms(black_box(medium.as_str())).unwrap())
    });
    group.bench_with_input(
        BenchmarkId::new("forms/large", large.len()),
        &large,
        |b, src| b.iter(|| parse_forms(black_box(src.as_str())).unwrap()),
    );

    group.bench_function("graph/simple", |b| {
        b.iter(|| parse(black_box(src_arithmetic_simple())).unwrap())
    });
    group.bench_function("graph/medium", |b| {
        b.iter(|| parse(black_box(medium.as_str())).unwrap())
    });
    group.bench_with_input(
        BenchmarkId::new("graph/large", large.len()),
        &large,
        |b, src| b.iter(|| parse(black_box(src.as_str())).unwrap()),
    );

    group.finish();
}

// ── 2. Evaluator benchmarks ──────────────────────────────────────────────────

fn make_eval(source: &str) -> Evaluator {
    let graph = parse(source).expect("bench source must parse");
    Evaluator::new(graph)
}

fn bench_eval_arithmetic(c: &mut Criterion) {
    let mut ev_simple = make_eval(src_arithmetic_simple());
    let env_simple = ev_simple.make_env();

    let mut ev_chain = make_eval(src_arithmetic_chain());
    let env_chain = ev_chain.make_env();

    let forms_simple: Vec<_> = ev_simple.graph().top_level_form_ids().to_vec();
    let forms_chain: Vec<_> = ev_chain.graph().top_level_form_ids().to_vec();

    let mut group = c.benchmark_group("eval/arithmetic");
    group.bench_function("simple", |b| {
        b.iter(|| {
            ev_simple
                .eval_sequence(black_box(&forms_simple), black_box(&env_simple))
                .unwrap()
        })
    });
    group.bench_function("chain", |b| {
        b.iter(|| {
            ev_chain
                .eval_sequence(black_box(&forms_chain), black_box(&env_chain))
                .unwrap()
        })
    });
    group.finish();
}

fn bench_eval_closure(c: &mut Criterion) {
    let mut ev = make_eval(src_closure_call());
    let env = ev.make_env();
    let forms: Vec<_> = ev.graph().top_level_form_ids().to_vec();

    c.bench_function("eval/closure/call", |b| {
        b.iter(|| {
            ev.eval_sequence(black_box(&forms), black_box(&env))
                .unwrap()
        })
    });
}

fn bench_eval_bind(c: &mut Criterion) {
    let src_heavy = src_bind_heavy();
    let mut ev = make_eval(src_heavy);
    let env = ev.make_env();
    let forms: Vec<_> = ev.graph().top_level_form_ids().to_vec();

    c.bench_function("eval/bind/heavy", |b| {
        b.iter(|| {
            // Bind writes into env, so use a fresh child env each iter.
            let child = caap_core::values::Environment::new(Some(env.clone()));
            ev.eval_sequence(black_box(&forms), black_box(&child))
                .unwrap()
        })
    });
}

fn bench_eval_recursion(c: &mut Criterion) {
    let mut group = c.benchmark_group("eval/recursion");
    for n in [10u32, 15, 20] {
        let src = src_recursion_fib(n);
        let mut ev = make_eval(&src);
        let env = ev.make_env();
        let forms: Vec<_> = ev.graph().top_level_form_ids().to_vec();
        group.bench_with_input(BenchmarkId::new("fib", n), &n, |b, _| {
            b.iter(|| {
                ev.eval_top_level_sequence(black_box(&forms), black_box(&env))
                    .unwrap()
            })
        });
    }
    group.finish();
}

fn bench_eval_collections(c: &mut Criterion) {
    let mut ev_list = make_eval(src_list_map());
    let env_list = ev_list.make_env();
    let forms_list: Vec<_> = ev_list.graph().top_level_form_ids().to_vec();

    let mut ev_map = make_eval(src_map_ops());
    let env_map = ev_map.make_env();
    let forms_map: Vec<_> = ev_map.graph().top_level_form_ids().to_vec();

    let mut ev_str = make_eval(src_string_ops());
    let env_str = ev_str.make_env();
    let forms_str: Vec<_> = ev_str.graph().top_level_form_ids().to_vec();

    let mut group = c.benchmark_group("eval/collections");
    group.bench_function("list/map20", |b| {
        b.iter(|| {
            ev_list
                .eval_top_level_sequence(black_box(&forms_list), black_box(&env_list))
                .unwrap()
        })
    });
    group.bench_function("map/get4", |b| {
        b.iter(|| {
            ev_map
                .eval_top_level_sequence(black_box(&forms_map), black_box(&env_map))
                .unwrap()
        })
    });
    group.bench_function("string/concat7", |b| {
        b.iter(|| {
            ev_str
                .eval_sequence(black_box(&forms_str), black_box(&env_str))
                .unwrap()
        })
    });
    group.finish();
}

// ── 3. Compiler / query benchmarks ───────────────────────────────────────────

fn bench_compiler_session(c: &mut Criterion) {
    let host = CompilerHost::new();

    c.bench_function("compiler/session/new", |b| {
        b.iter(|| black_box(host.new_session()))
    });
}

fn bench_compiler_template(c: &mut Criterion) {
    let src = src_medium_program();
    let host = CompilerHost::new();

    let mut group = c.benchmark_group("compiler/template");

    group.bench_function("cold", |b| {
        b.iter(|| {
            // Fresh session = empty cache every time.
            let mut compiler = host.new_session();
            compiler
                .load_surface_text_template(black_box(src.as_str()), "bench")
                .unwrap()
        })
    });

    group.bench_function("warm", |b| {
        // Single session: second and later calls are cache hits.
        let mut compiler = host.new_session();
        // Prime the cache.
        compiler
            .load_surface_text_template(src.as_str(), "bench")
            .unwrap();
        b.iter(|| {
            compiler
                .load_surface_text_template(black_box(src.as_str()), "bench")
                .unwrap()
        })
    });

    group.finish();
}

fn bench_compiler_query(c: &mut Criterion) {
    // Build a compiler session with one trivial provider that just evaluates
    // the unit's IR and stores a result — exercises the full query scheduling
    // and execution machinery without needing a bootstrap file.
    let host = CompilerHost::new();
    let mut session = host.new_session();

    session.register_stage("eval_unit").unwrap();
    session
        .register_provider(
            "bench.eval_unit",
            "eval_unit",
            PhasePolicy::CompileTime,
            |_ctx| Ok(()),
        )
        .unwrap();

    let graph = parse(src_medium_program().as_str()).unwrap();
    let unit = Unit::from_graph("bench.main", graph).unwrap();

    let mut group = c.benchmark_group("compiler/query");

    group.bench_function("single_stage", |b| {
        b.iter(|| {
            let mut u = unit.clone();
            session
                .queries()
                .query(
                    black_box("eval_unit"),
                    black_box(&mut u),
                    PhasePolicy::CompileTime,
                )
                .unwrap()
        })
    });

    // Measure plan-only (no execution).
    group.bench_function("plan_only", |b| {
        b.iter(|| {
            session
                .queries()
                .plan_query(black_box("eval_unit"), PhasePolicy::CompileTime)
                .unwrap()
        })
    });

    group.finish();
}

fn bench_compiler_unit(c: &mut Criterion) {
    // Unit construction from source — parse + IRGraph building.
    let src_simple = src_arithmetic_simple();
    let src_medium = src_medium_program();

    let mut group = c.benchmark_group("compiler/unit");
    group.bench_function("from_text/simple", |b| {
        b.iter(|| {
            let g = parse(black_box(src_simple)).unwrap();
            black_box(Unit::from_graph("bench", g).unwrap())
        })
    });
    group.bench_function("from_text/medium", |b| {
        b.iter(|| {
            let g = parse(black_box(src_medium.as_str())).unwrap();
            black_box(Unit::from_graph("bench", g).unwrap())
        })
    });
    group.finish();
}

// ── Register ──────────────────────────────────────────────────────────────────

criterion_group!(
    name = benches;
    config = Criterion::default().with_profiler(PProfProfiler::new(997, Output::Flamegraph(None)));
    targets =
        bench_parse,
        bench_eval_arithmetic,
        bench_eval_closure,
        bench_eval_bind,
        bench_eval_recursion,
        bench_eval_collections,
        bench_compiler_session,
        bench_compiler_template,
        bench_compiler_query,
        bench_compiler_unit,
);
criterion_main!(benches);
