use std::rc::Rc;

use crate::diagnostics::Diagnostic;
use crate::eval::Evaluator;
use crate::semantic::{PhasePolicy, SymbolKind};
use crate::unit::Unit;
use crate::values::{Environment, EvalResult, EvalSignal, RuntimeValue};

use super::bootstrap::EvaluationCapture;
use super::bridge::CompilerBridgeValue;
use super::session::Compiler;

pub struct CompilerEvaluationService<'a> {
    pub(super) compiler: &'a mut Compiler,
}

impl<'a> CompilerEvaluationService<'a> {
    pub fn evaluate(
        &mut self,
        unit: &Unit,
        phase: PhasePolicy,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> EvalResult {
        let bridge = Rc::new(
            CompilerBridgeValue::from_session_state(self.compiler.clone())
                .with_current_unit(unit.unit_id().to_string()),
        );
        let mut bindings: Vec<(String, RuntimeValue)> = initial.into_iter().collect();
        bindings.push((
            "compiler".to_string(),
            RuntimeValue::HostObject(bridge.clone()),
        ));
        let result = evaluate_unit(unit, phase, bindings);
        bridge.commit_session_into(self.compiler);
        if let Err(EvalSignal::Error(error)) = &result {
            self.compiler
                .push_diagnostic(Diagnostic::from_evaluation_error(error))
                .map_err(EvalSignal::from)?;
        }
        let _ = phase;
        result
    }

    pub fn evaluate_registered(
        &mut self,
        unit_id: &str,
        phase: PhasePolicy,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> EvalResult {
        let unit = self
            .compiler
            .get_unit(unit_id)
            .map_err(EvalSignal::from)?
            .cloned()
            .ok_or_else(|| {
                EvalSignal::from(crate::error::CaapError::compiler(format!(
                    "compiled unit not found: {unit_id}"
                )))
            })?;
        self.evaluate(&unit, phase, initial)
    }

    pub fn evaluate_with_host_libraries(
        &mut self,
        unit: &Unit,
        phase: PhasePolicy,
        libraries: impl IntoIterator<Item = String>,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    ) -> EvalResult {
        let mut bindings = Vec::new();
        {
            let services = match phase {
                PhasePolicy::CompileTime => self.compiler.host.compile_time_services(),
                PhasePolicy::Runtime | PhasePolicy::Dual => self.compiler.host.runtime_services(),
            };
            for library in libraries {
                let library_entry = services
                    .library(&library)
                    .map_err(|message| EvalSignal::from(crate::error::CaapError::host(message)))?
                    .ok_or_else(|| {
                        EvalSignal::from(crate::error::CaapError::host(format!(
                            "host service library does not exist: {library}"
                        )))
                    })?;
                for export in library_entry.export_names() {
                    let value = services
                        .export(&library, export, phase)
                        .map_err(|message| {
                            EvalSignal::from(crate::error::CaapError::host(message))
                        })?;
                    bindings.push((format!("{library}.{export}"), value));
                }
            }
        }
        bindings.extend(initial);
        self.evaluate(unit, phase, bindings)
    }

    pub fn evaluate_capture(
        &mut self,
        unit: &Unit,
        phase: PhasePolicy,
        initial: impl IntoIterator<Item = (String, RuntimeValue)>,
        skip_leading_forms: usize,
    ) -> Result<EvaluationCapture, EvalSignal> {
        let bridge = Rc::new(
            CompilerBridgeValue::from_session_state(self.compiler.clone())
                .with_current_unit(unit.unit_id().to_string()),
        );
        let mut bindings: Vec<(String, RuntimeValue)> = initial.into_iter().collect();
        bindings.push((
            "compiler".to_string(),
            RuntimeValue::HostObject(bridge.clone()),
        ));
        let result = evaluate_unit_capture_with_bindings(unit, phase, bindings, skip_leading_forms);
        bridge.commit_session_into(self.compiler);
        match result {
            Ok((value, captured_bindings)) => Ok(EvaluationCapture {
                unit_id: unit.unit_id().to_string(),
                phase,
                value: Some(value),
                bindings: captured_bindings,
                diagnostics: Vec::new(),
                skipped_forms: skip_leading_forms,
            }),
            Err(EvalSignal::Error(error)) => {
                let diagnostic = Diagnostic::from_evaluation_error(&error);
                self.compiler
                    .push_diagnostic(diagnostic.clone())
                    .map_err(EvalSignal::from)?;
                Ok(EvaluationCapture {
                    unit_id: unit.unit_id().to_string(),
                    phase,
                    value: None,
                    bindings: Vec::new(),
                    diagnostics: vec![diagnostic],
                    skipped_forms: skip_leading_forms,
                })
            }
            Err(signal) => Err(signal),
        }
    }
}

pub(super) fn evaluate_unit(
    unit: &Unit,
    phase: PhasePolicy,
    initial: impl IntoIterator<Item = (String, RuntimeValue)>,
) -> EvalResult {
    evaluate_unit_capture(unit, phase, initial, 0)
}

pub(super) fn evaluate_unit_capture(
    unit: &Unit,
    phase: PhasePolicy,
    initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    skip_leading_forms: usize,
) -> EvalResult {
    evaluate_unit_capture_with_bindings(unit, phase, initial, skip_leading_forms)
        .map(|(value, _)| value)
}

pub(super) fn evaluate_unit_capture_with_bindings(
    unit: &Unit,
    phase: PhasePolicy,
    initial: impl IntoIterator<Item = (String, RuntimeValue)>,
    skip_leading_forms: usize,
) -> Result<(RuntimeValue, Vec<(String, RuntimeValue)>), EvalSignal> {
    let top_level_names = unit
        .semantics()
        .symbols()
        .values()
        .filter(|entry| entry.kind == SymbolKind::TopLevel)
        .map(|entry| entry.name.clone())
        .collect();
    let mut evaluator =
        Evaluator::with_top_level_names_and_phase(unit.ir().clone(), top_level_names, phase);
    let env = evaluator.make_env();
    for (name, value) in initial {
        if name.is_empty() {
            return Err(EvalSignal::Error(crate::values::EvaluationError::new(
                "initial binding name must be non-empty",
            )));
        }
        Environment::define(&env, name, value);
    }
    let forms = evaluator.graph().top_level_form_ids().to_vec();
    if skip_leading_forms > forms.len() {
        return Err(EvalSignal::Error(crate::values::EvaluationError::new(
            "cannot skip more forms than unit contains",
        )));
    }
    let value = evaluator.eval_top_level_sequence(&forms[skip_leading_forms..], &env)?;
    Ok((value, capture_environment_bindings(&env)))
}

pub(super) fn capture_environment_bindings(
    env: &crate::values::EnvRef,
) -> Vec<(String, RuntimeValue)> {
    let mut bindings: Vec<(String, RuntimeValue)> = Environment::snapshot_bindings(env)
        .into_iter()
        .filter(|(_, value)| !matches!(value, RuntimeValue::UninitializedTopLevel))
        .collect();
    bindings.sort_by(|left, right| left.0.cmp(&right.0));
    bindings
}
