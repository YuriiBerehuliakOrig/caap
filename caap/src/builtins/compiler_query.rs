use crate::eval::Evaluator;

pub fn register(ev: &mut Evaluator) {
    super::compiler_query_runtime::register(ev);
}
