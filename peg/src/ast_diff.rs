//! Incremental AST diffing — given the AST before an edit, the edit, and the
//! freshly re-parsed AST (plus both source texts), report the minimal set of
//! byte ranges (in the *new* text's coordinates) whose subtree actually changed.
//!
//! This is the primitive an editor consumes for incremental work: re-run
//! highlighting / diagnostics / folding only over the returned ranges, and treat
//! everything else as unchanged. It is a pure comparison of two already-parsed
//! trees, so it cannot be unsound — it never *predicts* reuse, it *observes*
//! what differs. (Parsing the new text quickly is the position cache's job; this
//! is the layer above.)
//!
//! `AstNode` carries no text, so leaves are compared by their **source slice**:
//! a same-length character edit is detected at the leaf that covers it, while an
//! edit that restructures the tree is reported at the node whose shape changed.
//!
//! ```
//! use caap_peg::{parse_ast, Grammar};
//! use caap_peg::ast_diff::{changed_ranges, AstEdit};
//!
//! let g = Grammar::trusted_new("doc <- item+\nitem <- [a-z]+ \".\"").with_start_rule("doc");
//! let (old_text, new_text) = ("ab.cd.", "ab.ce."); // 'd' -> 'e' at byte 4
//! let old = parse_ast(&g, old_text, None).unwrap();
//! let new = parse_ast(&g, new_text, None).unwrap();
//! let ranges = changed_ranges(&old, old_text, &new, new_text, &AstEdit::new(4, 5, 5));
//! // Only the second item's leaf changed; the first item is untouched.
//! assert!(ranges.iter().all(|r| r.start >= 3));
//! assert!(!ranges.is_empty());
//! ```
//!
//! Children are aligned by an LCS over shift-invariant subtree hashes, so a
//! mid-list insertion or deletion is localised to the affected item rather than
//! reported as the whole list node. The equal common prefix and suffix are
//! matched directly first, so the quadratic LCS only runs over the changed
//! window — a single-item edit in a list of *n* children is O(n) (the hashing),
//! not O(n²).

use std::hash::{Hash, Hasher};

use crate::ast::{AstNode, AstSpan};

/// A single byte edit: the old text's `[start, old_end)` became the new text's
/// `[start, new_end)`. `old_end == new_end` for an in-place (same-length) edit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AstEdit {
    /// Inclusive start byte offset of the edit (shared by old and new).
    pub start: usize,
    /// Exclusive end of the replaced range in the old text.
    pub old_end: usize,
    /// Exclusive end of the replacement range in the new text.
    pub new_end: usize,
}

impl AstEdit {
    /// Build an edit: old `[start, old_end)` became new `[start, new_end)`.
    pub fn new(start: usize, old_end: usize, new_end: usize) -> Self {
        Self {
            start,
            old_end,
            new_end,
        }
    }

    fn delta(&self) -> isize {
        self.new_end as isize - self.old_end as isize
    }

    fn shift_add(&self, pos: usize) -> Option<usize> {
        usize::try_from(pos as isize + self.delta()).ok()
    }

    /// Map an old-text **start** (inclusive left edge) to the new text. At a pure
    /// insertion point a node that starts there is pushed right, so the boundary
    /// `pos == start` shifts. `None` if strictly inside a replaced region.
    fn shift_start(&self, pos: usize) -> Option<usize> {
        if pos < self.start {
            Some(pos)
        } else if pos >= self.old_end {
            self.shift_add(pos)
        } else {
            None
        }
    }

    /// Map an old-text **end** (exclusive right edge) to the new text. A node that
    /// ends exactly at an insertion point stays put, so the boundary `pos == start`
    /// does not shift. `None` if strictly inside a replaced region.
    fn shift_end(&self, pos: usize) -> Option<usize> {
        if pos <= self.start {
            Some(pos)
        } else if pos >= self.old_end {
            self.shift_add(pos)
        } else {
            None
        }
    }
}

/// Context carried through the recursive comparison.
struct Cx<'a> {
    old_text: &'a str,
    new_text: &'a str,
    edit: AstEdit,
}

/// Byte ranges (new-text coordinates) where `new` differs from `old` across
/// `edit`. Sorted by start; an empty result means the trees — and the text they
/// cover — are identical modulo the position shift.
pub fn changed_ranges(
    old: &AstNode,
    old_text: &str,
    new: &AstNode,
    new_text: &str,
    edit: &AstEdit,
) -> Vec<AstSpan> {
    let cx = Cx {
        old_text,
        new_text,
        edit: *edit,
    };
    let mut out = Vec::new();
    collect(old, new, &cx, &mut out);
    out.sort_by_key(|s| (s.start, s.end));
    out.dedup();
    out
}

/// Reconcile a freshly re-parsed tree against the previous one, **physically
/// reusing** every subtree that is genuinely unchanged.
///
/// Returns a tree equal to `new`, but wherever a subtree covers the same text at
/// the same position as the old one, the old node is kept — so the result shares
/// its [`Arc`](std::sync::Arc)-held children with `old`, and an editor can use
/// pointer identity (`Arc::ptr_eq` on `.children`) to skip re-processing
/// unchanged regions, tree-sitter style.
///
/// This is the structural-sharing companion to [`changed_ranges`]: that reports
/// *what* changed, this hands back a tree whose unchanged parts are *literally
/// the same nodes*. The unchanged test is the same text- and shift-aware
/// `subtree_equal` comparison — `AstNode` carries no text, so leaves are
/// distinguished by their source slice, never just by span.
///
/// Because spans are absolute, sharing is limited to subtrees that did **not**
/// shift (the prefix before the edit, plus any suffix the parser reproduced at
/// the identical offset): a shifted-but-equal subtree has stale absolute spans
/// and is rebuilt. Relative spans would lift that limit — the documented
/// follow-up.
///
/// ```
/// use std::sync::Arc;
/// use caap_peg::{parse_ast, Grammar};
/// use caap_peg::ast_diff::{reparse_ast_incremental, AstEdit};
///
/// let g = Grammar::trusted_new("doc <- item+\nitem <- [a-z]+ \".\"").with_start_rule("doc");
/// let old = parse_ast(&g, "ab.cd.", None).unwrap();
/// let new = parse_ast(&g, "ab.ce.", None).unwrap(); // 'd' -> 'e' in the 2nd item
/// let merged = reparse_ast_incremental(&old, "ab.cd.", new, "ab.ce.", &AstEdit::new(4, 5, 5));
/// // The untouched first item is the very same allocation.
/// assert!(Arc::ptr_eq(&old.children[0].children, &merged.children[0].children));
/// ```
pub fn reparse_ast_incremental(
    old: &AstNode,
    old_text: &str,
    new: AstNode,
    new_text: &str,
    edit: &AstEdit,
) -> AstNode {
    let cx = Cx {
        old_text,
        new_text,
        edit: *edit,
    };
    reconcile(old, new, &cx)
}

/// Walk `old`/`new` in lockstep, returning `new`'s shape but reusing `old`'s
/// node (and thus its children `Arc`) for any subtree that is unchanged *and*
/// sits at the same absolute position (so its spans are still valid).
fn reconcile(old: &AstNode, new: AstNode, cx: &Cx) -> AstNode {
    if old.span == new.span && subtree_equal(old, &new, cx) {
        // Unchanged and unshifted: hand back the old node. Cloning bumps the
        // children `Arc` rather than deep-copying, so the pointer is shared.
        return old.clone();
    }
    // Same shell (rule, error flag, arity): descend so any unchanged child keeps
    // its old allocation even though the parent changed.
    if old.rule == new.rule && old.error == new.error && old.children.len() == new.children.len() {
        let merged: Vec<AstNode> = old
            .children
            .iter()
            .zip(new.children.to_vec())
            .map(|(o, n)| reconcile(o, n, cx))
            .collect();
        return AstNode {
            children: merged.into(),
            ..new
        };
    }
    new
}

/// Source slice for a node's span, or `None` if the span is out of bounds / not
/// a char boundary (defensive — parser spans are always valid).
fn slice<'t>(text: &'t str, span: &AstSpan) -> Option<&'t str> {
    text.get(span.start..span.end)
}

/// Whether two subtrees are identical once `old`'s spans are shifted across the
/// edit: same shell, same children/captures recursively, and — at a leaf — the
/// same covered source text.
fn subtree_equal(old: &AstNode, new: &AstNode, cx: &Cx) -> bool {
    if !shell_equal(old, new, cx) {
        return false;
    }
    if old.children.is_empty() && old.captures.is_empty() {
        // Leaf: structure can't distinguish text, so compare the source slices.
        return slice(cx.old_text, &old.span) == slice(cx.new_text, &new.span);
    }
    old.children
        .iter()
        .zip(new.children.iter())
        .all(|(o, n)| subtree_equal(o, n, cx))
        && old
            .captures
            .iter()
            .zip(&new.captures)
            .all(|(o, n)| o.label == n.label && subtree_equal(&o.node, &n.node, cx))
}

/// The node's own identity (not its children/capture *contents*): rule, flags,
/// action, shifted span, and child/capture arity + capture labels.
fn shell_equal(old: &AstNode, new: &AstNode, cx: &Cx) -> bool {
    old.rule == new.rule
        && old.error == new.error
        && old.action == new.action
        && cx.edit.shift_start(old.span.start) == Some(new.span.start)
        && cx.edit.shift_end(old.span.end) == Some(new.span.end)
        && old.children.len() == new.children.len()
        && old.captures.len() == new.captures.len()
        && old
            .captures
            .iter()
            .zip(&new.captures)
            .all(|(o, n)| o.label == n.label)
}

fn collect(old: &AstNode, new: &AstNode, cx: &Cx, out: &mut Vec<AstSpan>) {
    if subtree_equal(old, new, cx) {
        return; // unchanged subtree — nothing to report
    }
    // Node identity beyond its children: rule, flags, action, and capture
    // labels/arity. If any of these differ the node itself changed shape — report
    // the whole new node rather than trying to align mismatched contents.
    let identity_ok = old.rule == new.rule
        && old.error == new.error
        && old.action == new.action
        && old.captures.len() == new.captures.len()
        && old
            .captures
            .iter()
            .zip(&new.captures)
            .all(|(o, n)| o.label == n.label);
    if !identity_ok {
        out.push(new.span.clone());
        return;
    }
    // A leaf (no children) whose identity matched but subtree differs → its
    // covered text changed; report it.
    if old.children.is_empty() && new.children.is_empty() {
        out.push(new.span.clone());
        // fall through to capture comparison below (a capture node may also differ)
    } else {
        align_children(&old.children, &new.children, cx, out);
    }
    // Captures are positional (their arity/labels already matched).
    for (o, n) in old.captures.iter().zip(&new.captures) {
        collect(&o.node, &n.node, cx, out);
    }
}

/// Align two child sequences by an LCS over shift-invariant subtree hashes, then:
/// recurse into matched pairs (which `collect` re-verifies, so hash collisions
/// only cost precision, never soundness), report inserted children by span, and
/// mark deletions with a zero-width range at the deletion site.
///
/// The equal common prefix and suffix (by hash) are peeled off and matched
/// directly, so the quadratic LCS only runs over the changed middle window. A
/// typical edit touches one child, leaving an empty or tiny window — alignment
/// is then dominated by the linear hashing pass, not the LCS.
fn align_children(old: &[AstNode], new: &[AstNode], cx: &Cx, out: &mut Vec<AstSpan>) {
    let old_h: Vec<u64> = old.iter().map(|n| subtree_hash(n, cx.old_text)).collect();
    let new_h: Vec<u64> = new.iter().map(|n| subtree_hash(n, cx.new_text)).collect();

    // Equal leading children: match in order. `collect` re-verifies each pair, so
    // a hash collision here narrows precision but never reports a false unchanged.
    let mut p = 0;
    while p < old.len() && p < new.len() && old_h[p] == new_h[p] {
        collect(&old[p], &new[p], cx, out);
        p += 1;
    }
    // Equal trailing children, not crossing into the already-matched prefix.
    let mut s = 0;
    while s < old.len() - p
        && s < new.len() - p
        && old_h[old.len() - 1 - s] == new_h[new.len() - 1 - s]
    {
        s += 1;
    }

    // Only the middle window is still misaligned; run the full LCS there.
    let (olo, ohi) = (p, old.len() - s);
    let (nlo, nhi) = (p, new.len() - s);
    let pairs = lcs_pairs(&old_h[olo..ohi], &new_h[nlo..nhi]);

    let (mut oi, mut ni) = (0usize, 0usize);
    for &(mo, mn) in &pairs {
        while ni < mn {
            out.push(new[nlo + ni].span.clone()); // inserted child
            ni += 1;
        }
        while oi < mo {
            push_deletion(&old[olo + oi], cx, out); // deleted child
            oi += 1;
        }
        collect(&old[olo + mo], &new[nlo + mn], cx, out); // matched pair — verify/narrow
        oi = mo + 1;
        ni = mn + 1;
    }
    while ni < nhi - nlo {
        out.push(new[nlo + ni].span.clone());
        ni += 1;
    }
    while oi < ohi - olo {
        push_deletion(&old[olo + oi], cx, out);
        oi += 1;
    }

    // Matched trailing children, in original order.
    for k in 0..s {
        collect(&old[old.len() - s + k], &new[new.len() - s + k], cx, out);
    }
}

/// Record a deleted old child as a zero-width range at its mapped new-text
/// position (the point where the editor should re-check).
fn push_deletion(old_child: &AstNode, cx: &Cx, out: &mut Vec<AstSpan>) {
    let at = cx
        .edit
        .shift_start(old_child.span.start)
        .unwrap_or(cx.edit.new_end);
    out.push(AstSpan::new(at, at));
}

/// Longest-common-subsequence matched index pairs between two hash sequences.
fn lcs_pairs(a: &[u64], b: &[u64]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut dp = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            pairs.push((i, j));
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    pairs
}

/// A position-independent hash of a subtree: rule/flags/action, leaf source text,
/// and children/captures recursively — but never absolute spans, so two equal
/// subtrees at shifted positions hash the same.
fn subtree_hash(node: &AstNode, text: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_node(node, text, &mut hasher);
    hasher.finish()
}

fn hash_node(node: &AstNode, text: &str, h: &mut impl Hasher) {
    node.rule.hash(h);
    node.error.hash(h);
    node.action.hash(h);
    if node.children.is_empty() && node.captures.is_empty() {
        slice(text, &node.span).unwrap_or("").hash(h);
        return;
    }
    node.children.len().hash(h);
    for child in node.children.iter() {
        hash_node(child, text, h);
    }
    node.captures.len().hash(h);
    for cap in &node.captures {
        cap.label.hash(h);
        hash_node(&cap.node, text, h);
    }
}
