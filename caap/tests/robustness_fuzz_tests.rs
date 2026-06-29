//! No-panic robustness harness (kernel must-have #2): no input — valid,
//! garbage, or adversarial — may PANIC the parser or the evaluator; the only
//! legal failure mode is a clean `Err`. The arithmetic audit found
//! build-profile-dependent panics by hand (`+` overflow, `%` MIN/-1,
//! `div_euclid`); this sweeps for the rest, deterministically (fixed-seed
//! xorshift, so CI failures reproduce) and tunably:
//!
//!   CAAP_FUZZ_ITERS=200000 cargo test -p caap-core --test robustness_fuzz_tests
//!
//! Coverage-guided fuzzing (cargo-fuzz) is the eventual upgrade; it needs a
//! nightly toolchain, so this in-tree harness is the floor that always runs.

use std::panic::{catch_unwind, AssertUnwindSafe};

use caap_core::frontend::parse;
use caap_core::semantic::PhasePolicy;
use caap_core::{Evaluator, RuntimeValue};

fn iters(default: usize) -> usize {
    std::env::var("CAAP_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct XorShift(u64);
impl XorShift {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// Byte soup biased toward the grammar's working set (parens, quotes,
/// comments, digits, escapes) — the inputs most likely to confuse a parser.
fn garbage_source(rng: &mut XorShift, max_len: usize) -> String {
    const ALPHABET: &[&str] = &[
        "(",
        ")",
        "\"",
        "\\",
        ";",
        "#|",
        "|#",
        "/*",
        "*/",
        " ",
        "\n",
        "\t",
        "a",
        "x",
        "_",
        "?",
        "!",
        "-",
        ".",
        ":",
        "0",
        "9",
        "-9223372036854775808",
        "bind",
        "lambda",
        "if",
        "set!",
        "null",
        "true",
        "\u{0}",
        "\u{7f}",
        "é",
        "𝄞",
    ];
    let len = (rng.next() % max_len as u64) as usize;
    let mut out = String::new();
    for _ in 0..len {
        out.push_str(ALPHABET.pick_via(rng));
    }
    out
}

trait PickVia<T> {
    fn pick_via(&self, rng: &mut XorShift) -> &T;
}
impl<T> PickVia<T> for &[T] {
    fn pick_via(&self, rng: &mut XorShift) -> &T {
        &self[(rng.next() % self.len() as u64) as usize]
    }
}

/// Structurally plausible expressions (balanced parens over the pure builtin
/// alphabet with hostile literals) so the EVALUATOR gets deep coverage, not
/// just the parser's error paths.
fn plausible_expr(rng: &mut XorShift, depth: usize) -> String {
    const LEAVES: &[&str] = &[
        "0",
        "1",
        "-1",
        "7",
        "9223372036854775807",
        "-9223372036854775808",
        "true",
        "false",
        "null",
        "\"a\"",
        "\"\"",
        "x",
    ];
    const OPS: &[(&str, usize)] = &[
        ("int_add", 2),
        ("int_sub", 2),
        ("int_mul", 2),
        ("int_div", 2),
        ("int_rem", 2),
        ("int_mod", 2),
        ("int_shl", 2),
        ("int_shr", 2),
        ("int_abs", 1),
        ("int_not", 1),
        ("not", 1),
        ("eq", 2),
        ("ne", 2),
        ("lt", 2),
        ("gt", 2),
        ("le", 2),
        ("ge", 2),
        ("string_concat_many", 2),
        ("string_slice", 2),
        ("size", 1),
        ("get", 2),
        ("list_of", 2),
        ("map_of", 2),
        ("append", 2),
        ("value_type", 1),
        ("if", 3),
        ("do", 2),
        ("while", 2),
    ];
    if depth == 0 || rng.next().is_multiple_of(4) {
        return (*LEAVES.pick_via(rng)).to_string();
    }
    let (op, arity) = OPS.pick_via(rng);
    let mut out = format!("({op}");
    for _ in 0..*arity {
        out.push(' ');
        out.push_str(&plausible_expr(rng, depth - 1));
    }
    out.push(')');
    out
}

fn assert_no_panic(label: &str, source: &str) {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Ok(graph) = parse(source) else {
            return; // clean parse error — legal outcome
        };
        for phase in [PhasePolicy::CompileTime, PhasePolicy::Runtime] {
            let graph = parse(source).expect("reparse");
            let mut ev = Evaluator::with_phase(graph, phase);
            let forms: Vec<_> = ev.graph().top_level_form_ids().to_vec();
            let env = ev.make_env();
            // Budgeted so hostile loops terminate; budget errors are legal.
            // Both budgets: steps bound CPU, allocation bounds memory so a
            // fuzz input that happens to allocate heavily fails cleanly rather
            // than OOM-aborting the fuzzer.
            let _ = ev.with_eval_alloc_budget(2_000_000, |ev| {
                ev.with_eval_step_budget(20_000, |ev| {
                    let mut last = Ok(RuntimeValue::Null);
                    for id in &forms {
                        last = ev.eval(*id, &env);
                        if last.is_err() {
                            break;
                        }
                    }
                    last
                })
            });
        }
        let _ = graph;
    }));
    assert!(
        result.is_ok(),
        "{label} PANICKED on input ({} bytes): {:?}",
        source.len(),
        source
    );
}

#[test]
fn parser_never_panics_on_garbage() {
    let mut rng = XorShift(0x_C0FF_EE00_DEAD_BEEF);
    for _ in 0..iters(3_000) {
        let source = garbage_source(&mut rng, 200);
        let result = catch_unwind(AssertUnwindSafe(|| {
            let _ = parse(&source);
        }));
        assert!(result.is_ok(), "parser PANICKED on: {source:?}");
    }
}

#[test]
fn evaluator_never_panics_on_plausible_expressions() {
    let mut rng = XorShift(0x_5EED_5EED_5EED_5EED);
    for _ in 0..iters(1_500) {
        let source = plausible_expr(&mut rng, 4);
        assert_no_panic("evaluator", &source);
    }
}

#[test]
fn evaluator_never_panics_on_garbage_that_happens_to_parse() {
    let mut rng = XorShift(0x_BAD5_EED5_0BAD_5EED);
    for _ in 0..iters(3_000) {
        let source = garbage_source(&mut rng, 120);
        assert_no_panic("evaluator(garbage)", &source);
    }
}
