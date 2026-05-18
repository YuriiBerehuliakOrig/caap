//! PEG parser benchmarks — mirrors benchmarks/bench_peg.py exactly.
//!
//! Groups:
//!   bench_literal_*      — baseline: simple literal / identifier list parsing
//!   bench_choice_*       — dispatch: distinct vs overlapping first chars
//!   bench_lr_expr_*      — expression grammar (left-assoc arithmetic)
//!   bench_memo_*         — memoization on/off
//!   bench_json_*         — JSON-like grammar (small, nested, large)
//!   bench_grammar_seal_* — cold grammar analysis cost
//!
//! Run:
//!   cargo bench
//!   cargo bench -- --save-baseline baseline
//!   cargo bench -- --baseline baseline

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};

use caap_peg_port::{analyze_grammar, Grammar, PEGParser, ParserConfig};

// ── Grammar texts ────────────────────────────────────────────────────────────

/// Mirrors `LITERAL_GRAMMAR` — single literal.
fn literal_grammar() -> Grammar {
    Grammar::new("start <- 'hello'").with_start_rule("start")
}

/// Mirrors `IDENT_LIST_GRAMMAR` — sep-plus of identifiers.
fn ident_list_grammar() -> Grammar {
    Grammar::new("start <- sep_plus(/[a-zA-Z]+/, ',')").with_start_rule("start")
}

/// Mirrors `LR_EXPR_GRAMMAR` — arithmetic (simulated as right-assoc due to no LR in PEG).
fn lr_expr_grammar() -> Grammar {
    Grammar::new(
        "expr <- term ('+' term / '-' term)*\n\
         term <- factor ('*' factor / '/' factor)*\n\
         factor <- '(' expr ')' / /[0-9]+/",
    )
    .with_start_rule("expr")
}

/// Mirrors `JSON_GRAMMAR`.
fn json_grammar() -> Grammar {
    Grammar::new(
        r#"value  <- object / array / string / number / 'null' / 'true' / 'false'
object <- '{' sep_plus(member, ',')? '}'
member <- string ':' value
array  <- '[' sep_plus(value, ',')? ']'
string <- /"[^"\\]*"/
number <- /-?[0-9]+(?:\.[0-9]+)?/"#,
    )
    .with_start_rule("value")
}

/// Mirrors `CHOICE_DISPATCHED_GRAMMAR` — 8 distinct-first-char literals.
fn choice_dispatched_grammar() -> Grammar {
    Grammar::new("start <- 'aaa' / 'bbb' / 'ccc' / 'ddd' / 'eee' / 'fff' / 'ggg' / 'hhh'")
        .with_start_rule("start")
}

/// Mirrors `CHOICE_OVERLAP_GRAMMAR` — 8 literals sharing "ab" prefix.
fn choice_overlap_grammar() -> Grammar {
    Grammar::new("start <- 'abc' / 'abd' / 'abe' / 'abf' / 'abg' / 'abh' / 'abi' / 'abj'")
        .with_start_rule("start")
}

// ── Inputs ──────────────────────────────────────────────────────────────────

fn ident_list_medium() -> String {
    (0..200).map(|_| "abc").collect::<Vec<_>>().join(",") // ~800 chars
}

fn ident_list_large() -> String {
    (0..5000).map(|_| "abc").collect::<Vec<_>>().join(",") // ~20 KB
}

fn lr_expr_short() -> String {
    "1+2*3".to_string()
}

fn lr_expr_medium() -> String {
    (0..100)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("+") // ~300 chars
}

fn lr_expr_large() -> String {
    (0..2000)
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join("+") // ~10 KB
}

fn json_small() -> String {
    r#"{"key": "value", "n": 42, "flag": true}"#.to_string()
}

fn json_nested() -> String {
    r#"{"a": [1, 2, {"b": [3, 4, {"c": [5, 6, null]}]}], "x": {"y": {"z": "deep"}}, "arr": [true, false, null]}"#
        .to_string()
}

fn json_large() -> String {
    let items: Vec<String> = (0..100)
        .map(|i| format!(r#"{{"id": {i}, "val": "item{i}"}}"#))
        .collect();
    format!("[{}]", items.join(", ")) // ~5 KB
}

// ── Helper: configs ──────────────────────────────────────────────────────────

fn config(max_steps: usize, memo: bool) -> ParserConfig {
    let mut c = ParserConfig::default().with_max_steps(max_steps);
    c.memo = memo;
    c
}

fn config_memo_on() -> ParserConfig {
    config(4096, true)
}

// ── 1. Literal baseline ───────────────────────────────────────────────────────

fn bench_literal(c: &mut Criterion) {
    let grammar = literal_grammar();
    let parser = PEGParser;
    let config = config_memo_on();

    c.bench_function("literal/short", |b| {
        b.iter(|| parser.parse(black_box(&grammar), black_box("hello"), black_box(&config)))
    });
}

fn bench_literal_list(c: &mut Criterion) {
    let grammar = ident_list_grammar();
    let parser = PEGParser;
    let medium = ident_list_medium();
    let large = ident_list_large();
    let cfg_medium = config(medium.len() + 64, true);
    let cfg_large = config(large.len() + 64, true);

    let mut group = c.benchmark_group("literal_list");
    group.bench_function("medium", |b| {
        b.iter(|| {
            parser.parse(
                black_box(&grammar),
                black_box(medium.as_str()),
                black_box(&cfg_medium),
            )
        })
    });
    group.bench_function("large", |b| {
        b.iter(|| {
            parser.parse(
                black_box(&grammar),
                black_box(large.as_str()),
                black_box(&cfg_large),
            )
        })
    });
    group.finish();
}

// ── 2. Choice dispatch ────────────────────────────────────────────────────────

fn bench_choice(c: &mut Criterion) {
    let parser = PEGParser;
    let config = config_memo_on();
    let dispatched = choice_dispatched_grammar();
    let overlap = choice_overlap_grammar();

    let mut group = c.benchmark_group("choice");
    // Match last alternative — worst case for sequential scan.
    group.bench_function("dispatched_last", |b| {
        b.iter(|| parser.parse(black_box(&dispatched), black_box("hhh"), black_box(&config)))
    });
    group.bench_function("overlap_last", |b| {
        b.iter(|| parser.parse(black_box(&overlap), black_box("abj"), black_box(&config)))
    });
    group.finish();
}

// ── 3. Expression grammar ─────────────────────────────────────────────────────

fn bench_lr_expr(c: &mut Criterion) {
    let grammar = lr_expr_grammar();
    let parser = PEGParser;
    let short = lr_expr_short();
    let medium = lr_expr_medium();
    let large = lr_expr_large();
    let cfg_short = config(short.len() + 64, true);
    let cfg_medium = config(medium.len() + 64, true);
    let cfg_large = config(large.len() + 64, true);

    let mut group = c.benchmark_group("lr_expr");
    group.bench_function("short", |b| {
        b.iter(|| {
            parser.parse(
                black_box(&grammar),
                black_box(short.as_str()),
                black_box(&cfg_short),
            )
        })
    });
    group.bench_function("medium", |b| {
        b.iter(|| {
            parser.parse(
                black_box(&grammar),
                black_box(medium.as_str()),
                black_box(&cfg_medium),
            )
        })
    });
    group.bench_with_input(
        BenchmarkId::new("large", large.len()),
        &large,
        |b, input| {
            b.iter(|| {
                parser.parse(
                    black_box(&grammar),
                    black_box(input.as_str()),
                    black_box(&cfg_large),
                )
            })
        },
    );
    group.finish();
}

// ── 4. Memoization on / off ───────────────────────────────────────────────────

fn bench_memo(c: &mut Criterion) {
    let grammar = json_grammar();
    let parser = PEGParser;
    let small = json_small();
    let large = json_large();
    let cfg_small_on = config(small.len() + 64, true);
    let cfg_small_off = config(small.len() + 64, false);
    let cfg_large_on = config(large.len() + 64, true);
    let cfg_large_off = config(large.len() + 64, false);

    let mut group = c.benchmark_group("memo");
    group.bench_function("on_small", |b| {
        b.iter(|| {
            parser.parse(
                black_box(&grammar),
                black_box(small.as_str()),
                black_box(&cfg_small_on),
            )
        })
    });
    group.bench_function("off_small", |b| {
        b.iter(|| {
            parser.parse(
                black_box(&grammar),
                black_box(small.as_str()),
                black_box(&cfg_small_off),
            )
        })
    });
    group.bench_function("on_large", |b| {
        b.iter(|| {
            parser.parse(
                black_box(&grammar),
                black_box(large.as_str()),
                black_box(&cfg_large_on),
            )
        })
    });
    group.bench_function("off_large", |b| {
        b.iter(|| {
            parser.parse(
                black_box(&grammar),
                black_box(large.as_str()),
                black_box(&cfg_large_off),
            )
        })
    });
    group.finish();
}

// ── 5. JSON grammar ──────────────────────────────────────────────────────────

fn bench_json(c: &mut Criterion) {
    let grammar = json_grammar();
    let parser = PEGParser;
    let small = json_small();
    let nested = json_nested();
    let large = json_large();
    let cfg_small = config(small.len() + 64, true);
    let cfg_nested = config(nested.len() + 64, true);
    let cfg_large = config(large.len() + 64, true);

    let mut group = c.benchmark_group("json");
    group.bench_function("small", |b| {
        b.iter(|| {
            parser.parse(
                black_box(&grammar),
                black_box(small.as_str()),
                black_box(&cfg_small),
            )
        })
    });
    group.bench_function("nested", |b| {
        b.iter(|| {
            parser.parse(
                black_box(&grammar),
                black_box(nested.as_str()),
                black_box(&cfg_nested),
            )
        })
    });
    group.bench_with_input(
        BenchmarkId::new("large", large.len()),
        &large,
        |b, input| {
            b.iter(|| {
                parser.parse(
                    black_box(&grammar),
                    black_box(input.as_str()),
                    black_box(&cfg_large),
                )
            })
        },
    );
    group.finish();
}

// ── 6. Cold grammar analysis ──────────────────────────────────────────────────

fn bench_grammar_seal(c: &mut Criterion) {
    let mut group = c.benchmark_group("grammar_seal");

    group.bench_function("simple", |b| {
        b.iter(|| {
            let g =
                Grammar::new("start <- a b\na <- 'hello'\nb <- 'world'").with_start_rule("start");
            black_box(analyze_grammar(&g))
        })
    });

    group.bench_function("complex_json", |b| {
        b.iter(|| {
            let g = json_grammar();
            black_box(analyze_grammar(&g))
        })
    });

    group.finish();
}

// ── 7. Grammar compilation (compile + parse) vs parse-only ───────────────────

fn bench_compile_overhead(c: &mut Criterion) {
    // Measures how much of parse() time is grammar compilation vs actual parsing.
    let text = lr_expr_medium();
    let cfg = config(text.len() + 64, true);

    c.bench_function("compile_overhead/lr_expr_medium", |b| {
        b.iter(|| {
            // Grammar::new() includes rule text parsing.
            let g = black_box(lr_expr_grammar());
            PEGParser.parse(&g, black_box(text.as_str()), black_box(&cfg))
        })
    });
}

// ── Register ─────────────────────────────────────────────────────────────────

criterion_group!(
    benches,
    bench_literal,
    bench_literal_list,
    bench_choice,
    bench_lr_expr,
    bench_memo,
    bench_json,
    bench_grammar_seal,
    bench_compile_overhead,
);
criterion_main!(benches);
