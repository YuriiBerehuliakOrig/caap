use crate::eval::Evaluator;

pub fn register(ev: &mut Evaluator) {
    super::provider_context_runtime::register(ev);
}
