//! Language vocabulary, queried once from `caap-core` and cached, so the server
//! classifies and completes identifiers using the kernel's real reserved words
//! instead of a hardcoded list maintained in parallel here (which silently
//! drifts whenever the language gains or renames a special form or builtin).
//!
//! The source of truth is [`caap_core::language::kernel_vocabulary`]; this
//! module only adds editor-facing lookup sets over it.

use std::collections::HashSet;
use std::sync::LazyLock;

use caap_core::language::{kernel_vocabulary, KernelVocabulary, KERNEL_LITERALS};

static VOCAB: LazyLock<KernelVocabulary> = LazyLock::new(kernel_vocabulary);

static SPECIAL_FORMS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| VOCAB.special_forms.iter().map(String::as_str).collect());

static BUILTINS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| VOCAB.builtins.iter().map(String::as_str).collect());

/// A keyword-like form that controls its own evaluation (`lambda`, `if`, `do`,
/// `quote`, …) — derived from the kernel, not hardcoded.
pub fn is_special_form(name: &str) -> bool {
    SPECIAL_FORMS.contains(name)
}

/// A kernel literal atom (`true`, `false`, `null`).
pub fn is_literal(name: &str) -> bool {
    KERNEL_LITERALS.contains(&name)
}

/// A public eager builtin function (`int_add`, `get`, `map_of`, …).
pub fn is_builtin(name: &str) -> bool {
    BUILTINS.contains(name)
}

/// Sorted special-form names, for completion. Borrowed from the cached vocab.
pub fn special_forms() -> &'static [String] {
    &VOCAB.special_forms
}

/// Sorted builtin-function names, for completion.
pub fn builtins() -> &'static [String] {
    &VOCAB.builtins
}

/// Kernel literal names, for completion.
pub fn literals() -> &'static [&'static str] {
    KERNEL_LITERALS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classification_comes_from_the_kernel() {
        // Special forms (frontend + lazy builtins), literals, and eager builtin
        // functions are each classified from `caap_core::language`, not a list
        // maintained in this crate.
        assert!(is_special_form("lambda") && is_special_form("if") && is_special_form("do"));
        assert!(is_literal("true") && is_literal("null"));
        assert!(is_builtin("int_add") && is_builtin("get"));
        // Disjoint roles: a builtin function is not a special form, and vice versa.
        assert!(!is_special_form("int_add"));
        assert!(!is_builtin("if"));
        // The hardcoded list used to carry the phantom form `quote` (not a real
        // kernel form) — deriving from the kernel drops it.
        assert!(!is_special_form("quote"));
    }
}
