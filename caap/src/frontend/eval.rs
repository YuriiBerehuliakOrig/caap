use crate::error::CaapResult;
use crate::eval::Evaluator;
use crate::values::{eval_err, EvalResult};

use super::parse_segmental;

/// Parse and evaluate CAAP source; returns the value of the last top-level form.
///
/// Reading is **segmental** — forms are read one at a time so a top-level
/// `(extend_syntax "rule" "peg-source")` directive can grow the grammar before
/// later forms are read (see [`parse_segmental`]). The assembled whole-program
/// graph is then evaluated as a unit, exactly like the Unit/compile path: top
/// level forms share one environment, so forward references between definitions
/// (mutual recursion, a helper referenced before its source-order definition)
/// resolve the same way on both paths.
pub fn eval_source(source: &str) -> EvalResult {
    let graph = parse_segmental(source).map_err(eval_err)?;
    let mut evaluator = Evaluator::new(graph);
    evaluator.run()
}

/// Build an `Evaluator` from CAAP source (read segmentally) without running it.
pub fn evaluator_from_source(source: &str) -> CaapResult<Evaluator> {
    let graph = parse_segmental(source)?;
    Ok(Evaluator::new(graph))
}
