use crate::eval::Evaluator;
use crate::semantic::EffectSet;
use crate::values::{eval_err, EvalSignal, RuntimeValue};

pub fn register(ev: &mut Evaluator) {
    ev.register_special(
        "effect_scope",
        1,
        None,
        crate::values::BuiltinMetadata::special_form(),
        |ev, call, env| {
            let requested = ev.eval(call.args[0], env)?;
            let effects = effect_set_from_runtime(&requested, "effect_scope")?;
            // effect_scope is the kernel's untrusted-code boundary (see
            // docs/architecture.md). Attenuating effects is not enough: a
            // hostile loop could still OOM-abort the host. Bound memory too —
            // a default allocation budget when none is already active (a nested
            // scope inherits the outer, possibly-depleted budget, so it cannot
            // reset its own bound).
            ev.with_default_alloc_budget_if_unbounded(|ev| {
                ev.with_effect_scope(effects, |ev| ev.eval_sequence(&call.args[1..], env))
            })
        },
    );
}

fn effect_set_from_runtime(value: &RuntimeValue, context: &str) -> Result<EffectSet, EvalSignal> {
    let RuntimeValue::List(items) = value else {
        return Err(eval_err(format!(
            "{context}: expected a list of effect tags"
        )));
    };
    let values = items
        .borrow()
        .iter()
        .map(|item| match item {
            RuntimeValue::Str(tag) => Ok(tag.to_string()),
            _ => Err(eval_err(format!("{context}: effect tags must be strings"))),
        })
        .collect::<Result<Vec<_>, EvalSignal>>()?;
    EffectSet::from_unique_strings(values, context).map_err(|error| eval_err(error.to_string()))
}
