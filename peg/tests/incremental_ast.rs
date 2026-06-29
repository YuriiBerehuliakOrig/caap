//! Scenario: **incremental AST diff & reuse** — after an edit, `changed_ranges`
//! reports only the byte ranges whose subtree actually changed, and
//! `reparse_ast_incremental` physically shares (`Arc::ptr_eq`) the subtrees that
//! did not, while never sharing a same-span leaf whose source text changed.

use caap_peg::ast_diff::{changed_ranges, reparse_ast_incremental, AstEdit};
use caap_peg::{parse_ast, AstSpan, Grammar};

fn doc_grammar() -> Grammar {
    Grammar::trusted_new("doc <- item+\nitem <- [a-z]+ \".\"").with_start_rule("doc")
}

fn ranges(old_text: &str, new_text: &str, edit: AstEdit) -> Vec<AstSpan> {
    let g = doc_grammar();
    let old = parse_ast(&g, old_text, None).unwrap();
    let new = parse_ast(&g, new_text, None).unwrap();
    changed_ranges(&old, old_text, &new, new_text, &edit)
}

#[test]
fn unchanged_text_yields_no_ranges() {
    assert!(ranges("ab.cd.", "ab.cd.", AstEdit::new(3, 3, 3)).is_empty());
}

#[test]
fn same_length_leaf_edit_localises_to_that_leaf() {
    // 'd' -> 'e' at byte 4: only the second item's word leaf changed.
    let r = ranges("ab.cd.", "ab.ce.", AstEdit::new(4, 5, 5));
    assert!(!r.is_empty(), "a text change must be reported");
    assert!(
        r.iter().all(|s| s.start >= 3),
        "the first item is untouched: {r:?}"
    );
    // And it is narrow — within the second item, not the whole doc.
    assert!(r.iter().all(|s| s.end <= 6));
}

#[test]
fn mid_list_insertion_is_localised_via_lcs() {
    // Insert a new item "x." at byte 3: "ab.x.cd.". LCS alignment matches the
    // untouched "ab." and "cd." items, so only the inserted item is reported.
    let r = ranges("ab.cd.", "ab.x.cd.", AstEdit::new(3, 3, 5));
    assert!(!r.is_empty());
    assert!(
        r.iter().all(|s| s.start >= 3 && s.end <= 5),
        "only the inserted item [3,5) is reported: {r:?}"
    );
}

#[test]
fn mid_list_deletion_is_localised_via_lcs() {
    // Delete the middle item "x." (bytes 3..5): "ab.x.cd." -> "ab.cd.". The
    // surviving items align; the deletion is a zero-width marker at byte 3.
    let r = ranges("ab.x.cd.", "ab.cd.", AstEdit::new(3, 5, 3));
    assert!(!r.is_empty());
    assert!(
        r.iter().all(|s| s.start == 3),
        "deletion is marked at the splice point, not the whole doc: {r:?}"
    );
}

#[test]
fn structural_change_is_reported() {
    // Append a third item — doc's child arity grows, so the change surfaces.
    let r = ranges("ab.cd.", "ab.cd.ef.", AstEdit::new(6, 6, 9));
    assert!(!r.is_empty());
    assert!(r.iter().any(|s| s.end >= 6));
}

#[test]
fn reparse_shares_unchanged_subtree_allocations() {
    use std::sync::Arc;
    let g = doc_grammar();
    // 'd' -> 'e' at byte 4: only the second item's text changed.
    let old = parse_ast(&g, "ab.cd.", None).unwrap();
    let new = parse_ast(&g, "ab.ce.", None).unwrap();
    let merged = reparse_ast_incremental(&old, "ab.cd.", new, "ab.ce.", &AstEdit::new(4, 5, 5));

    // Result is structurally the freshly-parsed tree…
    assert_eq!(merged.children.len(), 2);
    assert_eq!(merged.children[1].span, AstSpan::new(3, 6));
    // …but the untouched first item is physically the same node.
    assert!(Arc::ptr_eq(
        &old.children[0].children,
        &merged.children[0].children
    ));
    // The changed item is NOT shared even though it has the same span and
    // (text-free) shape: its source slice differs, so `subtree_equal` rejects it.
    assert!(!Arc::ptr_eq(
        &old.children[1].children,
        &merged.children[1].children
    ));
}

#[test]
fn reparse_does_not_share_a_same_span_leaf_whose_text_changed() {
    // Guards the leaf-text trap: same span, identical (text-free) structure,
    // but different covered text → must be rebuilt, not shared.
    use std::sync::Arc;
    let g = doc_grammar();
    let old = parse_ast(&g, "ab.", None).unwrap();
    let new = parse_ast(&g, "xy.", None).unwrap();
    let merged = reparse_ast_incremental(&old, "ab.", new, "xy.", &AstEdit::new(0, 2, 2));
    assert!(!Arc::ptr_eq(
        &old.children[0].children,
        &merged.children[0].children
    ));
}

#[test]
fn large_list_single_edit_stays_localised_after_prefix_suffix_trim() {
    // A long flat list with one item changed deep in the middle. The equal common
    // prefix and suffix are matched directly (no quadratic LCS over the whole
    // list), and the result must still pin the change to exactly that one item —
    // proving the trim preserves the full-LCS semantics.
    let n = 200usize;
    let mk = |hit: usize| -> String {
        (0..n)
            .map(|i| if i == hit { "zz." } else { "aa." })
            .collect()
    };
    let hit = 137;
    let old_text = mk(usize::MAX); // all "aa."
    let new_text = mk(hit); // one item is "zz."
    let at = hit * 3; // byte offset of the changed item
    let r = ranges(&old_text, &new_text, AstEdit::new(at, at + 2, at + 2));
    assert!(!r.is_empty());
    assert!(
        r.iter().all(|s| s.start >= at && s.end <= at + 3),
        "only the single changed item [{at}, {}) is reported: {r:?}",
        at + 3
    );
}

#[test]
fn reparse_of_identical_tree_shares_the_whole_root() {
    use std::sync::Arc;
    let g = doc_grammar();
    let old = parse_ast(&g, "ab.cd.", None).unwrap();
    let new = parse_ast(&g, "ab.cd.", None).unwrap();
    let merged = reparse_ast_incremental(&old, "ab.cd.", new, "ab.cd.", &AstEdit::new(0, 0, 0));
    assert!(Arc::ptr_eq(&old.children, &merged.children));
}
