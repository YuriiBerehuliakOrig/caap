use crate::eval::Evaluator;

pub fn register(ev: &mut Evaluator) {
    super::compiler_units_runtime::register(ev);
}
