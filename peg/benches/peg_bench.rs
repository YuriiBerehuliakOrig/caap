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
//!   cargo bench -- --profile-time 10   (generates flamegraphs via pprof)

use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};
use pprof::criterion::{Output, PProfProfiler};

use caap_peg::{
    analyze_grammar, parse_ast, Directive, Grammar, PEGParser, ParseCache, ParseDriver,
    ParseEffect, ParseRequest, ParseView, ParserConfig,
};

/// Observe-only driver: returns `Proceed` for every effect. Used to measure the
/// cost of attaching the Parse Effects Protocol vs. a plain parse.
struct NoopDriver;
impl ParseDriver for NoopDriver {
    fn handle(&self, _effect: &ParseEffect<'_>, _view: &ParseView<'_>) -> Directive {
        Directive::Proceed
    }
}

// ── Grammar texts ────────────────────────────────────────────────────────────

/// Mirrors `LITERAL_GRAMMAR` — single literal.
fn literal_grammar() -> Grammar {
    Grammar::trusted_new("start <- 'hello'").with_start_rule("start")
}

/// Mirrors `IDENT_LIST_GRAMMAR` — sep-plus of identifiers.
fn ident_list_grammar() -> Grammar {
    Grammar::trusted_new("start <- sep_plus(/[a-zA-Z]+/, ',')").with_start_rule("start")
}

/// Mirrors `LR_EXPR_GRAMMAR` — arithmetic (simulated as right-assoc due to no LR in PEG).
fn lr_expr_grammar() -> Grammar {
    Grammar::trusted_new(
        "expr <- term ('+' term / '-' term)*\n\
         term <- factor ('*' factor / '/' factor)*\n\
         factor <- '(' expr ')' / /[0-9]+/",
    )
    .with_start_rule("expr")
}

/// Mirrors `JSON_GRAMMAR`.
fn json_grammar() -> Grammar {
    Grammar::trusted_new(
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
    Grammar::trusted_new("start <- 'aaa' / 'bbb' / 'ccc' / 'ddd' / 'eee' / 'fff' / 'ggg' / 'hhh'")
        .with_start_rule("start")
}

/// Mirrors `CHOICE_OVERLAP_GRAMMAR` — 8 literals sharing "ab" prefix.
fn choice_overlap_grammar() -> Grammar {
    Grammar::trusted_new("start <- 'abc' / 'abd' / 'abe' / 'abf' / 'abg' / 'abh' / 'abi' / 'abj'")
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

/// Replace the single byte at `at` with `repl` (same length → delta 0).
fn replace_byte(text: &str, at: usize, repl: char) -> String {
    let mut out = text.to_string();
    out.replace_range(at..at + 1, &repl.to_string());
    out
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
    let cfg_small_off = config(small.len() * 50, false);
    let cfg_large_on = config(large.len() + 64, true);
    let cfg_large_off = config(large.len() * 50, false);

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
            let g = Grammar::trusted_new("start <- a b\na <- 'hello'\nb <- 'world'")
                .with_start_rule("start");
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
            // Grammar::trusted_new() includes rule text parsing.
            let g = black_box(lr_expr_grammar());
            PEGParser.parse(&g, black_box(text.as_str()), black_box(&cfg))
        })
    });
}

fn bench_driver(c: &mut Criterion) {
    // Confirms the protocol is ~zero-cost without a driver, and measures the
    // overhead of an attached observe-only driver and of `parse_ast` (which runs
    // an internal collector driver).
    let grammar = lr_expr_grammar();
    let text = lr_expr_medium();
    let cfg = config(text.len() + 64, true);
    let driver = NoopDriver;

    let mut group = c.benchmark_group("driver");
    group.bench_function("no_driver", |b| {
        b.iter(|| PEGParser.parse(&grammar, black_box(text.as_str()), &cfg))
    });
    group.bench_function("noop_driver", |b| {
        b.iter(|| {
            PEGParser.parse_with_driver(&grammar, black_box(text.as_str()), &cfg, Some(&driver))
        })
    });
    group.bench_function("parse_ast", |b| {
        b.iter(|| parse_ast(&grammar, black_box(text.as_str()), None))
    });
    group.finish();
}

// ── 8. Incremental subtree reuse ──────────────────────────────────────────────

fn bench_incremental(c: &mut Criterion) {
    // Measures the incremental reparse cost (cache primed from the original
    // text, then a one-character edit applied) against a full from-scratch
    // parse. The JSON grammar has named recursive rules (`value`/`object`/
    // `member`/…) invoked at many positions, so the position cache replays the
    // untouched elements: both a head edit and a tail edit reparse only the one
    // edited element and reuse the rest, ~6× faster than a full parse.
    //
    // This relies on a *tight* regex read-extent: `string`/`number` are
    // multi-token regexes, and recording their examined interval as the exact
    // automaton-death offset (rather than conservatively end-of-input) is what
    // lets a tail edit avoid invalidating every preceding entry. The
    // examined-extent tracking also keeps that reuse sound under lookahead.
    let grammar = json_grammar();
    let original = json_large();
    let cfg = config(original.len() * 8 + 1024, true);

    // Every object embeds the word "item" inside a quoted string value; flipping
    // an 'm' to 'n' is a same-length, always-valid edit. `rfind` lands in the
    // last element (tail), `find` in the first (head).
    let tail_edit = replace_byte(&original, original.rfind('m').unwrap(), 'n');
    let head_edit = replace_byte(&original, original.find('m').unwrap(), 'n');

    // Prime a cache with the original text once; clone it per measured iteration.
    let mut primed = ParseCache::default();
    ParseRequest::new(&grammar)
        .config(cfg.clone())
        .run_incremental(&original, &mut primed)
        .expect("priming parse should succeed");

    let mut group = c.benchmark_group("incremental");
    group.bench_function("full_parse", |b| {
        b.iter(|| {
            PEGParser.parse(
                black_box(&grammar),
                black_box(original.as_str()),
                black_box(&cfg),
            )
        })
    });
    group.bench_function("reparse_tail_edit", |b| {
        b.iter_batched(
            || primed.clone(),
            |mut cache| {
                ParseRequest::new(black_box(&grammar))
                    .config(cfg.clone())
                    .run_incremental(black_box(tail_edit.as_str()), &mut cache)
            },
            BatchSize::SmallInput,
        )
    });
    group.bench_function("reparse_head_edit", |b| {
        b.iter_batched(
            || primed.clone(),
            |mut cache| {
                ParseRequest::new(black_box(&grammar))
                    .config(cfg.clone())
                    .run_incremental(black_box(head_edit.as_str()), &mut cache)
            },
            BatchSize::SmallInput,
        )
    });
    group.finish();
}

// ── Register ─────────────────────────────────────────────────────────────────

criterion_group!(
    name = benches;
    config = Criterion::default().with_profiler(PProfProfiler::new(997, Output::Flamegraph(None)));
    targets =
        bench_literal,
        bench_literal_list,
        bench_choice,
        bench_lr_expr,
        bench_memo,
        bench_json,
        bench_grammar_seal,
        bench_compile_overhead,
        bench_driver,
        bench_incremental,
);
criterion_main!(benches);
