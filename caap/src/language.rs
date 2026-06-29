//! Authoritative language vocabulary, derived from the kernel for editor tooling
//! (LSP, formatters) that must classify or complete identifiers without keeping
//! a parallel hardcoded list of reserved words that drifts from the real
//! language.
//!
//! The single source of truth is the builtin registry ([`builtins::register_all`])
//! plus the frontend's surface→IR lowering. A tool should call
//! [`kernel_vocabulary`] once and cache the result — it is the one place that
//! knows which identifiers are special forms vs. builtin functions.

use crate::builtins;
use crate::eval::Evaluator;
use crate::frontend::ParsedForm;
use crate::graph::IRGraph;
use crate::source::SourceSpan;

/// Kernel literal atoms — parsed as values, not callables.
pub const KERNEL_LITERALS: &[&str] = &["true", "false", "null"];

/// Special forms the frontend recognizes directly in its surface→IR lowering.
/// They are not builtins (they desugar to dedicated IR nodes), so they never
/// appear in [`Evaluator::builtin_names`] and must be listed here. This list is
/// the authoritative companion to the lowering match in `frontend.rs`; the test
/// `frontend_recognizes_exactly_the_listed_special_forms` keeps them in sync.
pub const FRONTEND_SPECIAL_FORMS: &[&str] = &["lambda", "bind", "set!", "block", "leave"];

/// The punctuation characters a kernel SYMBOL may contain, alongside ASCII
/// alphanumerics. The authoritative source is the `symbol` rule of the seed
/// grammar in [`crate::frontend`] (`/[A-Za-z_+\-*/<>=!?$%&:.][A-Za-z0-9_+\-*/<>=!?$%&:.]*/`);
/// the test `symbol_chars_match_the_grammar` pins this set to the real parser so
/// it cannot drift. Editor tooling (caap-lsp) scans and classifies identifiers
/// from [`is_symbol_char`] rather than a parallel hardcoded character set.
pub const SYMBOL_PUNCT_CHARS: &str = "_+-*/<>=!?$%&:.";

/// True if `c` may appear inside a kernel symbol: an ASCII alphanumeric or one of
/// [`SYMBOL_PUNCT_CHARS`]. (The grammar additionally forbids a leading digit; this
/// is character-membership only — what editor span-scanning and operator
/// classification need.)
pub fn is_symbol_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || SYMBOL_PUNCT_CHARS.contains(c)
}

/// The kernel's callable vocabulary, classified for editor tooling.
#[derive(Debug, Clone, Default)]
pub struct KernelVocabulary {
    /// Keyword-like forms that control their own argument evaluation: the
    /// frontend special forms plus every public builtin registered with a
    /// non-eager (special-form / lazy) evaluation policy — `if`, `do`, `while`,
    /// `quote`, `and`, `or`, … Sorted and deduplicated.
    pub special_forms: Vec<String>,
    /// Public eager builtin functions: `int_add`, `get`, `map_of`, … Sorted.
    pub builtins: Vec<String>,
}

/// Build the kernel vocabulary from the live builtin registry. Cheap: registers
/// the builtins into a throwaway evaluator (no stdlib bootstrap) and reads back
/// each public name with its evaluation policy. Intended to be called once and
/// cached by the consuming tool.
pub fn kernel_vocabulary() -> KernelVocabulary {
    let mut ev = Evaluator::new(IRGraph::new());
    builtins::register_all(&mut ev);

    let mut special_forms: Vec<String> = FRONTEND_SPECIAL_FORMS
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    let mut builtins_list: Vec<String> = Vec::new();

    for name in ev.builtin_names() {
        let Some(info) = ev.builtin_info(name) else {
            continue;
        };
        // Hide internal builtins (e.g. lowering targets) from editor surfaces.
        if !info.metadata.is_public() {
            continue;
        }
        if info.metadata.eager_args() {
            builtins_list.push(name.to_string());
        } else {
            special_forms.push(name.to_string());
        }
    }

    special_forms.sort();
    special_forms.dedup();
    builtins_list.sort();
    builtins_list.dedup();
    KernelVocabulary {
        special_forms,
        builtins: builtins_list,
    }
}

// ── Surface name-binding structure ──────────────────────────────────────────
//
// How the kernel special forms introduce names into scope, read off the surface
// `ParsedForm`. This is the authoritative description of kernel name-binding
// shape: editor tooling (LSP scope/outline/highlight) must query it rather than
// re-encoding "lambda binds its first list", "bind binds its pairs", … in a
// parallel mini-frontend that drifts from the real lowering. Forms that are not
// kernel name-binders (`do`, `set!`, calls, and every stdlib/grammar definer
// such as `register_module`/`define_class`) return `None`; those are resolved
// by the authoritative bootstrap analysis, not the kernel.

/// The role a [`IntroducedName`] plays in its form's body scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameRole {
    /// A function parameter (`lambda`).
    Parameter,
    /// A local binding (`bind`).
    Local,
}

/// A name a kernel special form introduces into its body's scope.
pub struct IntroducedName<'a> {
    /// The bound identifier, verbatim (a `lambda` rest parameter keeps its `&`).
    pub text: &'a str,
    /// The declaration span (the identifier token itself).
    pub span: &'a SourceSpan,
    /// Span of the smallest sub-form introducing this name — a `bind` pair, or
    /// the whole form for a single `bind` / a `lambda` parameter. Editors use it
    /// as the definition's "defining form" for outline nesting / scope bounds.
    pub form_span: &'a SourceSpan,
    /// The form initialising a `Local` binding (a `bind` value), if any. `None`
    /// for parameters. Lets the caller classify the binding (e.g. function vs
    /// value) without re-deciding which child holds the value.
    pub value: Option<&'a ParsedForm>,
    pub role: NameRole,
}

/// If `form` is a kernel special form that introduces names into its body's
/// scope (`lambda`, `bind`), return them in declaration order; otherwise `None`.
///
/// This is the single source of truth for kernel surface name-binding structure.
/// It is intentionally limited to the kernel: stdlib/grammar definer forms are
/// not described here because the kernel does not know them — their definitions
/// come from the bootstrapped compiler.
pub fn introduced_names(form: &ParsedForm) -> Option<Vec<IntroducedName<'_>>> {
    let ParsedForm::List { items, .. } = form else {
        return None;
    };
    let form_span = form.span();
    let head = match items.first() {
        Some(ParsedForm::Symbol { text, .. }) => text.as_str(),
        _ => return None,
    };
    match head {
        // `(lambda (p1 p2 …) body…)` — the params are the symbols in child 1.
        "lambda" => {
            let mut out = Vec::new();
            if let Some(ParsedForm::List { items: params, .. }) = items.get(1) {
                for param in params {
                    if let ParsedForm::Symbol { text, span } = param {
                        out.push(IntroducedName {
                            text: text.as_str(),
                            span,
                            form_span,
                            value: None,
                            role: NameRole::Parameter,
                        });
                    }
                }
            }
            Some(out)
        }
        // `(bind name value body…)` or `(bind ((n v) …) body…)`.
        "bind" => {
            let mut out = Vec::new();
            match items.get(1) {
                Some(ParsedForm::Symbol { text, span }) if items.len() >= 3 => {
                    out.push(IntroducedName {
                        text: text.as_str(),
                        span,
                        form_span,
                        value: items.get(2),
                        role: NameRole::Local,
                    });
                }
                Some(ParsedForm::List { items: pairs, .. }) => {
                    for pair in pairs {
                        if let ParsedForm::List {
                            items: pair_items,
                            span: pair_span,
                        } = pair
                        {
                            if let Some(ParsedForm::Symbol { text, span }) = pair_items.first() {
                                out.push(IntroducedName {
                                    text: text.as_str(),
                                    span,
                                    form_span: pair_span,
                                    value: pair_items.get(1),
                                    role: NameRole::Local,
                                });
                            }
                        }
                    }
                }
                _ => {}
            }
            Some(out)
        }
        _ => None,
    }
}

/// The body forms of a `(lambda (params…) body…)` — everything after the
/// parameter list — or `None` if `form` is not a lambda. Companion to
/// [`introduced_names`] for tools that outline a lambda's contents.
pub fn lambda_body(form: &ParsedForm) -> Option<&[ParsedForm]> {
    let ParsedForm::List { items, .. } = form else {
        return None;
    };
    match items.first() {
        Some(ParsedForm::Symbol { text, .. }) if text == "lambda" && items.len() >= 2 => {
            Some(&items[2..])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frontend::parse_forms;

    fn form(src: &str) -> ParsedForm {
        parse_forms(src)
            .expect("parse")
            .forms
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn vocabulary_is_derived_and_classified() {
        let vocab = kernel_vocabulary();

        // Frontend forms + lazy builtins land in special_forms.
        for form in ["lambda", "bind", "set!", "if", "do"] {
            assert!(
                vocab.special_forms.iter().any(|s| s == form),
                "expected `{form}` among special forms; got {:?}",
                vocab.special_forms
            );
        }
        // Eager value builtins land in builtins.
        for builtin in ["int_add", "get", "map_of"] {
            assert!(
                vocab.builtins.iter().any(|s| s == builtin),
                "expected `{builtin}` among builtins"
            );
        }
        // The two sets are disjoint and non-empty.
        assert!(!vocab.builtins.is_empty() && !vocab.special_forms.is_empty());
        for sform in &vocab.special_forms {
            assert!(
                !vocab.builtins.contains(sform),
                "`{sform}` classified as both special form and builtin"
            );
        }
    }

    #[test]
    fn symbol_chars_match_the_grammar() {
        // The editor predicate must accept EXACTLY the kernel `symbol` grammar's
        // character set, so caap-lsp never maintains a parallel list that drifts.
        // Probe each char mid-symbol (`a{c}b`) since the grammar forbids only a
        // LEADING digit, and read back whether it tokenized into one symbol.
        let inner_symbol = |src: &str| -> Option<String> {
            match parse_forms(src).ok()?.forms.into_iter().next()? {
                ParsedForm::List { items, .. } => match items.into_iter().next()? {
                    ParsedForm::Symbol { text, .. } => Some(text),
                    _ => None,
                },
                _ => None,
            }
        };
        for c in SYMBOL_PUNCT_CHARS.chars() {
            assert!(is_symbol_char(c), "`{c}` is in SYMBOL_PUNCT_CHARS");
            assert_eq!(
                inner_symbol(&format!("(a{c}b)")).as_deref(),
                Some(format!("a{c}b").as_str()),
                "`{c}` must tokenize inside one symbol per the grammar"
            );
        }
        // Characters the grammar does NOT allow in a symbol: the predicate must
        // reject them, and they must not tokenize into the single symbol `a{c}b`.
        for c in ['|', '^', '~', '@', '#', ','] {
            assert!(!is_symbol_char(c), "`{c}` is not a kernel symbol char");
            assert_ne!(
                inner_symbol(&format!("(a{c}b)")).as_deref(),
                Some(format!("a{c}b").as_str()),
                "`{c}` must NOT be part of a single symbol"
            );
        }
    }

    #[test]
    fn introduced_names_reads_lambda_and_bind() {
        let lambda = form("(lambda (a b &rest) (a b))");
        let names = introduced_names(&lambda).unwrap();
        assert_eq!(
            names.iter().map(|n| n.text).collect::<Vec<_>>(),
            ["a", "b", "&rest"]
        );
        assert!(names
            .iter()
            .all(|n| n.role == NameRole::Parameter && n.value.is_none()));

        let pairs = form("(bind ((x 1) (y 2)) x)");
        let pair = introduced_names(&pairs).unwrap();
        assert_eq!(pair.iter().map(|n| n.text).collect::<Vec<_>>(), ["x", "y"]);
        assert!(pair
            .iter()
            .all(|n| n.role == NameRole::Local && n.value.is_some()));

        let one = form("(bind name 42)");
        let single = introduced_names(&one).unwrap();
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].text, "name");

        // Non-binders and definer forms are not the kernel's to describe.
        assert!(introduced_names(&form("(do 1 2)")).is_none());
        assert!(introduced_names(&form("(register_module \"m\" x)")).is_none());
        assert!(introduced_names(&form("(int_add 1 2)")).is_none());
    }
}
