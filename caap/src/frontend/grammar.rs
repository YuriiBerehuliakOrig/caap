use std::sync::LazyLock;

use caap_peg::{analyze_and_store, Grammar};

const GRAMMAR_TEXT: &str = concat!(
    "forms    <- form*\n",
    "form     <- list / string / integer / boolean / null / symbol\n",
    "list     <- '(' form* ')'\n",
    r#"string   <- /\"(?:[^\"\\]|\\.)*\"/"#,
    "\n",
    // Matches integers and decimal floats (with optional exponent); the surface
    // projection emits Float when a '.', 'e', or 'E' is present.
    "integer  <- /-?(?:0|[1-9][0-9]*)(?:\\.[0-9]+)?(?:[eE][-+]?[0-9]+)?/\n",
    "boolean  <- 'true' / 'false'\n",
    "null     <- 'null'\n",
    r"symbol   <- /[A-Za-z_+\-*\/<>=!?$%&:.][A-Za-z0-9_+\-*\/<>=!?$%&:.]*/",
    "\n",
);

static SURFACE_GRAMMAR: LazyLock<Grammar> = LazyLock::new(|| {
    let mut grammar = Grammar::trusted_new(GRAMMAR_TEXT).with_start_rule("forms");
    let analysis = analyze_and_store(&mut grammar);
    debug_assert!(
        analysis.errors.is_empty(),
        "built-in CAAP surface grammar must be valid: {:?}",
        analysis.errors
    );
    grammar.seal();
    grammar
});

pub(super) fn surface_grammar() -> &'static Grammar {
    &SURFACE_GRAMMAR
}
