//! Integration tests for the Parse Effects Protocol (the `ParseDriver` control
//! surface). Each test exercises one of the implemented steps:
//!
//! 1. `AltMatched` semantic rejection → the choice backtracks to the next
//!    alternative.
//! 2. Transactional host state: a rejected alternative's state mutations are
//!    rolled back.
//! 3. Per-rule facet memo: a rule declared `Depends` is not packrat-memoised.
//! 4. Scoped sub-parse: a driver resolves a decision by parsing another rule.
//! 5. `@!guard(e)` grammar sugar + `ChoiceEnter` restrict + `RuleExit` rewrite.

use std::cell::RefCell;

use caap_peg::{
    Directive, DriverCheckpoint, Grammar, LexToken, MemoFacet, ParseDriver, ParseDriverBuilder,
    ParseEffect, ParseRequest, ParseValue, ParseView,
};

// ── Step 1: semantic rejection of an alternative ────────────────────────────

/// Rejects the first alternative of `root`, forcing the choice onwards.
struct RejectFirstNamed;

impl ParseDriver for RejectFirstNamed {
    fn handle(&self, effect: &ParseEffect<'_>, _view: &ParseView<'_>) -> Directive {
        if let ParseEffect::AltMatched {
            value: ParseValue::Named(name, _),
            ..
        } = effect
        {
            if name.as_ref() == "first" {
                return Directive::Reject;
            }
        }
        Directive::Proceed
    }
}

#[test]
fn alt_matched_reject_backtracks_to_next_alternative() {
    let grammar = Grammar::trusted_new("root <- first:[a-z]+ / second:[a-z]+");
    let driver = RejectFirstNamed;

    // Without a driver, the first alternative wins.
    let plain = ParseRequest::new(&grammar).run("foo").unwrap();
    assert!(matches!(plain, ParseValue::Named(ref n, _) if n.as_ref() == "first"));

    // With the driver rejecting it, the second alternative is chosen instead —
    // proving the engine treated a *semantic* rejection like a syntactic fail.
    let driven = ParseRequest::new(&grammar)
        .driver(&driver)
        .run("foo")
        .unwrap();
    assert!(matches!(driven, ParseValue::Named(ref n, _) if n.as_ref() == "second"));
}

// ── Step 2: transactional host state across backtracking ────────────────────

/// Journals side effects so a rolled-back branch leaves no trace. A guard named
/// `note` records a mark; the driver rejects the first `root` alternative, which
/// must unwind the mark recorded while exploring it.
#[derive(Default)]
struct Journaling {
    log: RefCell<Vec<char>>,
}

impl ParseDriver for Journaling {
    fn handle(&self, effect: &ParseEffect<'_>, _view: &ParseView<'_>) -> Directive {
        match effect {
            ParseEffect::Guard { name, .. } if *name == "note" => {
                self.log.borrow_mut().push('x');
                Directive::Proceed
            }
            ParseEffect::AltMatched {
                rule: Some(rule),
                index: 0,
                ..
            } if *rule == "root" => Directive::Reject,
            _ => Directive::Proceed,
        }
    }

    fn checkpoint(&self) -> DriverCheckpoint {
        // Journal by absolute length — robust to nested checkpoints.
        DriverCheckpoint::of(self.log.borrow().len())
    }

    fn rollback(&self, snapshot: DriverCheckpoint) {
        if let Some(len) = snapshot.take::<usize>() {
            self.log.borrow_mut().truncate(len);
        }
    }
}

#[test]
fn rejected_alternative_state_is_rolled_back() {
    // Alternative 0 (`a`) records a mark via its guard, but is rejected at the
    // `root` choice; alternative 1 (`b`) matches the same input cleanly.
    let grammar = Grammar::trusted_new(
        "root <- a / b\n\
         a <- @!note('x')\n\
         b <- [a-z]",
    );
    let driver = Journaling::default();

    let value = ParseRequest::new(&grammar)
        .driver(&driver)
        .run("x")
        .unwrap();

    // `b` produced the value (a plain text), and the mark recorded while
    // exploring the rejected `a` was rolled back.
    assert_eq!(value.inner(), &ParseValue::Text("x".into()));
    assert!(
        driver.log.borrow().is_empty(),
        "expected rolled-back log, got {:?}",
        driver.log.borrow()
    );
}

// ── Step 3: per-rule facet memo soundness ───────────────────────────────────

/// Counts how many times rule `dup`'s body is entered, and reports a memo facet
/// for it according to `mode` — pure, a constant state digest, or a digest that
/// changes on every query.
enum FacetMode {
    Pure,
    Constant,
    Varying,
}

struct EntryCounter {
    entries: RefCell<usize>,
    facet_queries: RefCell<u64>,
    mode: FacetMode,
}

impl EntryCounter {
    fn new(mode: FacetMode) -> Self {
        Self {
            entries: RefCell::new(0),
            facet_queries: RefCell::new(0),
            mode,
        }
    }
}

impl ParseDriver for EntryCounter {
    fn handle(&self, effect: &ParseEffect<'_>, _view: &ParseView<'_>) -> Directive {
        if let ParseEffect::RuleEnter { rule, .. } = effect {
            if *rule == "dup" {
                *self.entries.borrow_mut() += 1;
            }
        }
        Directive::Proceed
    }

    fn memo_facet(&self, rule: &str) -> MemoFacet {
        if rule != "dup" {
            return MemoFacet::Pure;
        }
        match self.mode {
            FacetMode::Pure => MemoFacet::Pure,
            FacetMode::Constant => MemoFacet::Depends(7),
            FacetMode::Varying => {
                let mut queries = self.facet_queries.borrow_mut();
                *queries += 1;
                MemoFacet::Depends(*queries)
            }
        }
    }
}

#[test]
fn keyed_memo_reuses_on_matching_state_and_recomputes_on_change() {
    // `&dup dup` evaluates `dup` at position 0 twice (once in the lookahead).
    let grammar = Grammar::trusted_new("root <- &dup dup\ndup <- 'x'");

    // Pure → memoised: the body runs once, the second eval is a cache hit.
    let pure = EntryCounter::new(FacetMode::Pure);
    ParseRequest::new(&grammar).driver(&pure).run("x").unwrap();
    assert_eq!(*pure.entries.borrow(), 1, "pure rule should be memoised");

    // State-dependent but identical digest → keyed memo still reuses it.
    let constant = EntryCounter::new(FacetMode::Constant);
    ParseRequest::new(&grammar)
        .driver(&constant)
        .run("x")
        .unwrap();
    assert_eq!(
        *constant.entries.borrow(),
        1,
        "matching state digest should reuse the memo entry"
    );

    // State-dependent with a digest that changes each eval → recomputed, never
    // served a stale result.
    let varying = EntryCounter::new(FacetMode::Varying);
    ParseRequest::new(&grammar)
        .driver(&varying)
        .run("x")
        .unwrap();
    assert_eq!(
        *varying.entries.borrow(),
        2,
        "changed state digest must recompute, not replay a stale memo"
    );
}

// ── Step 4: scoped sub-parse ────────────────────────────────────────────────

/// A guard that rejects an identifier when it is exactly a keyword, decided by
/// running an isolated sub-parse of the `kw` rule at the same position.
struct KeywordExcluder;

impl ParseDriver for KeywordExcluder {
    fn handle(&self, effect: &ParseEffect<'_>, view: &ParseView<'_>) -> Directive {
        if let ParseEffect::Guard {
            name: "reject_kw",
            pos,
            end,
            ..
        } = effect
        {
            let probe = view.sub_parse("kw", *pos);
            if probe.ok && probe.consumed == end - pos {
                return Directive::Reject;
            }
        }
        Directive::Proceed
    }
}

#[test]
fn sub_parse_resolves_a_semantic_decision() {
    let grammar = Grammar::trusted_new("root <- @!reject_kw([a-z]+)\nkw <- 'let'");
    let driver = KeywordExcluder;

    // A keyword is rejected (the sub-parse of `kw` consumes the whole word).
    let kw = ParseRequest::new(&grammar).driver(&driver).run("let");
    assert!(kw.is_err(), "keyword should be rejected, got {kw:?}");

    // A non-keyword identifier is accepted.
    let id = ParseRequest::new(&grammar).driver(&driver).run("foo");
    assert!(id.is_ok(), "identifier should be accepted, got {id:?}");
}

// ── Step 5: ChoiceEnter restrict + RuleExit value rewrite ───────────────────

struct Steerer;

impl ParseDriver for Steerer {
    fn handle(&self, effect: &ParseEffect<'_>, _view: &ParseView<'_>) -> Directive {
        match effect {
            // Restrict `root`'s choice to alternatives 2 then 1 (drop 0).
            ParseEffect::ChoiceEnter {
                rule: Some(rule), ..
            } if *rule == "root" => Directive::Restrict(vec![2, 1]),
            // Rewrite the whole-rule value of `root` to a number.
            ParseEffect::RuleExit { rule, .. } if *rule == "root" => {
                Directive::Accept(ParseValue::Number(42))
            }
            _ => Directive::Proceed,
        }
    }
}

#[test]
fn choice_enter_restrict_and_rule_exit_rewrite() {
    let grammar = Grammar::trusted_new("root <- 'a' / 'b' / 'c'");
    let driver = Steerer;

    // Alternative 0 ('a') is excluded by Restrict → parsing "a" fails.
    let excluded = ParseRequest::new(&grammar).driver(&driver).run("a");
    assert!(
        excluded.is_err(),
        "alt 0 should be excluded, got {excluded:?}"
    );

    // "c" matches (restricted set still contains index 2) and RuleExit rewrites
    // the value to Number(42).
    let rewritten = ParseRequest::new(&grammar)
        .driver(&driver)
        .run("c")
        .unwrap();
    assert_eq!(rewritten, ParseValue::Number(42));
}

// ── Failed effect: the driver can rewrite the final diagnostic ──────────────

struct RewriteError;

impl ParseDriver for RewriteError {
    fn handle(&self, effect: &ParseEffect<'_>, _view: &ParseView<'_>) -> Directive {
        if let ParseEffect::Failed { .. } = effect {
            return Directive::Fail("custom diagnostic".to_string());
        }
        Directive::Proceed
    }
}

#[test]
fn failed_effect_rewrites_diagnostic() {
    let grammar = Grammar::trusted_new("root <- 'a'");
    let err = ParseRequest::new(&grammar)
        .driver(&RewriteError)
        .run("b")
        .unwrap_err();
    assert_eq!(&*err.message, "custom diagnostic");
}

// ── Driver runs on the token-stream path too ────────────────────────────────

#[test]
fn driver_runs_on_token_stream_path() {
    let grammar = Grammar::trusted_new("root <- @!reject(tok(NAME))");
    let tokens = vec![LexToken::new("NAME", "x", 0, 1)];

    // With a rejecting guard the token parse fails.
    let driver = ParseDriverBuilder::new()
        .accept_if("reject", |_value, _view| false)
        .build();
    let rejected = ParseRequest::new(&grammar)
        .driver(&driver)
        .tokens(tokens.clone())
        .run("x");
    assert!(rejected.is_err(), "guard should reject, got {rejected:?}");

    // With no driver the guard passes through.
    let ok = ParseRequest::new(&grammar).tokens(tokens).run("x");
    assert!(ok.is_ok(), "without driver the guard is inert, got {ok:?}");
}

// ── ParseDriverBuilder ergonomics ───────────────────────────────────────────

#[test]
fn builder_value_guard_accepts_and_rejects() {
    let grammar = Grammar::trusted_new("root <- @!even(/[0-9]+/)");
    let driver = ParseDriverBuilder::new()
        .accept_if("even", |value, _view| {
            matches!(value, ParseValue::Text(t)
                if t.parse::<i64>().map(|n| n % 2 == 0).unwrap_or(false))
        })
        .build();

    assert!(ParseRequest::new(&grammar).driver(&driver).run("4").is_ok());
    assert!(ParseRequest::new(&grammar)
        .driver(&driver)
        .run("3")
        .is_err());
}

// ── @action / @?pred routed through the driver (unified protocol) ───────────

#[test]
fn builder_action_transforms_value_via_driver() {
    let grammar = Grammar::trusted_new("root <- @up(/[a-z]+/)").with_start_rule("root");
    let driver = ParseDriverBuilder::new()
        .action("up", |value, _view| match value {
            ParseValue::Text(text) => ParseValue::Text(text.to_uppercase().into()),
            other => other,
        })
        .build();
    assert_eq!(
        ParseRequest::new(&grammar)
            .driver(&driver)
            .run("abc")
            .unwrap(),
        ParseValue::Text("ABC".into())
    );
}

#[test]
fn builder_predicate_gates_parse_via_driver() {
    let grammar = Grammar::trusted_new("root <- @?gate 'x'").with_start_rule("root");

    let accept = ParseDriverBuilder::new()
        .predicate("gate", |_view| true)
        .build();
    assert!(ParseRequest::new(&grammar).driver(&accept).run("x").is_ok());

    let reject = ParseDriverBuilder::new()
        .predicate("gate", |_view| false)
        .build();
    assert!(ParseRequest::new(&grammar)
        .driver(&reject)
        .run("x")
        .is_err());
}

#[test]
fn builder_action_sees_rich_view() {
    let grammar = Grammar::trusted_new("root <- @ctx(key:/[a-z]+/)").with_start_rule("root");
    let driver = ParseDriverBuilder::new()
        .action("ctx", |_value, view| {
            assert_eq!(view.matched_text, "abc");
            assert_eq!(view.span, Some((0, 3)));
            assert_eq!(view.grammar().start_rule, "root");
            assert!(view.config().memo);
            assert!(view.named().contains_key("key"));
            assert!(view.rule_stack.contains(&"root"));
            ParseValue::Text("seen".into())
        })
        .build();
    assert_eq!(
        ParseRequest::new(&grammar)
            .driver(&driver)
            .run("abc")
            .unwrap(),
        ParseValue::Text("seen".into())
    );
}

// ── Builder intercept: global control without implementing the trait ─────────

#[test]
fn builder_intercept_rejects_alternative() {
    let grammar = Grammar::trusted_new("root <- first:[a-z]+ / second:[a-z]+");
    let driver = ParseDriverBuilder::new()
        .intercept(|effect, _view| match effect {
            ParseEffect::AltMatched { index: 0, .. } => Directive::Reject,
            _ => Directive::Proceed,
        })
        .build();
    let value = ParseRequest::new(&grammar)
        .driver(&driver)
        .run("foo")
        .unwrap();
    assert!(matches!(value, ParseValue::Named(ref n, _) if n.as_ref() == "second"));
}

// ── Left recursion under a driver stays correct ─────────────────────────────

#[test]
fn left_recursion_with_driver_matches_no_driver() {
    // Left-recursive, left-associative subtraction.
    let grammar =
        Grammar::trusted_new("expr <- expr '-' num / num\nnum <- /[0-9]+/").with_start_rule("expr");

    let plain = ParseRequest::new(&grammar).run("9-3-2").unwrap();

    // An observe-only driver (forces the driver-aware memo/prune path) must
    // produce the identical tree.
    let observer = ParseDriverBuilder::new().on_event(|_, _| {}).build();
    let driven = ParseRequest::new(&grammar)
        .driver(&observer)
        .run("9-3-2")
        .unwrap();

    assert_eq!(plain, driven);
    // Sanity: it actually left-associated into nested expr nodes.
    assert!(matches!(driven, ParseValue::Node(ref n, _) if n.as_ref() == "sequence"));
}

// ── Zero-cost when unused: parsing without a driver is unchanged ─────────────

#[test]
fn no_driver_behaves_like_plain_peg() {
    let grammar = Grammar::trusted_new("root <- 'a' / 'b'");
    assert_eq!(
        ParseRequest::new(&grammar).run("b").unwrap().inner(),
        &ParseValue::Text("b".into())
    );
}

#[test]
fn on_event_observes_rule_lifecycle() {
    // The `on_event` hook sees RuleEnter/RuleExit effects for the matched rule.
    use std::sync::{Arc, Mutex};
    let grammar = Grammar::trusted_new("word <- [a-z]+").with_start_rule("word");
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let log_cb = Arc::clone(&log);
    let driver = ParseDriverBuilder::new()
        .on_event(move |effect, _view| match effect {
            ParseEffect::RuleEnter { rule, .. } => {
                log_cb.lock().unwrap().push(format!("enter:{rule}"))
            }
            ParseEffect::RuleExit { rule, .. } => {
                log_cb.lock().unwrap().push(format!("exit:{rule}"))
            }
            _ => {}
        })
        .build();
    ParseRequest::new(&grammar)
        .driver(&driver)
        .run("abc")
        .expect("parse succeeds");
    let recorded = log.lock().unwrap();
    assert!(recorded.iter().any(|e| e == "enter:word"));
    assert!(recorded.iter().any(|e| e == "exit:word"));
}
