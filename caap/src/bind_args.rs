use crate::ir::NodeId;

/// Split canonical flat bind arguments into binding-pair ids and the body id.
///
/// Canonical flat bind shape is:
/// bind <name-1> <value-1> ... <name-n> <value-n> <body>
pub(crate) fn flat_bind_pairs_and_body(args: &[NodeId]) -> Option<(&[NodeId], NodeId)> {
    if args.len() < 3 || !(args.len() - 1).is_multiple_of(2) {
        return None;
    }
    args.split_last()
        .map(|(body_id, pair_ids)| (pair_ids, *body_id))
}
