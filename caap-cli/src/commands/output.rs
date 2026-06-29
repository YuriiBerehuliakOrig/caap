use std::collections::BTreeMap;
use std::fs;
use std::io::Write;

use caap_core::diagnostics::{render_diagnostic, Diagnostic, DiagnosticCode};
use caap_core::error::CaapError;
use caap_core::values::EvalSignal;

pub(super) fn write_diagnostics(stderr: &mut dyn Write, diagnostics: &[Diagnostic]) {
    let mut source_cache: BTreeMap<String, Option<String>> = BTreeMap::new();
    for (index, diagnostic) in diagnostics.iter().enumerate() {
        if index > 0 {
            let _ = writeln!(stderr);
        }
        let rendered =
            if let Some(path) = diagnostic.span.as_ref().and_then(|span| span.path.as_ref()) {
                let source = source_cache
                    .entry(path.clone())
                    .or_insert_with(|| fs::read_to_string(path).ok());
                render_diagnostic(diagnostic, source.as_deref())
            } else {
                render_diagnostic(diagnostic, None)
            };
        let _ = writeln!(stderr, "{rendered}");
    }
}

pub(super) fn write_caap_error(stderr: &mut dyn Write, input: &str, error: &CaapError) {
    if let Some(diagnostic) = diagnostic_from_caap_error(input, error) {
        write_diagnostics(stderr, &[diagnostic]);
    } else {
        let _ = writeln!(stderr, "{input}: {error}");
    }
}

pub(super) fn write_eval_signal(stderr: &mut dyn Write, input: &str, error: &EvalSignal) {
    let diagnostic = match error {
        EvalSignal::Error(error) => {
            let mut diagnostic = Diagnostic::from_evaluation_error(error);
            if diagnostic.location.is_none() {
                diagnostic.location = Some(input.to_string());
            }
            if diagnostic.context.is_empty() {
                diagnostic.context.push("eval".to_string());
            }
            diagnostic
        }
        EvalSignal::Leave(_) | EvalSignal::Exception(_) | EvalSignal::TailCall(_) => {
            let Ok(mut diagnostic) = Diagnostic::error(error.to_string())
                .and_then(|diagnostic| diagnostic.with_code(DiagnosticCode::Runtime))
            else {
                let _ = writeln!(stderr, "{input}: {error}");
                return;
            };
            diagnostic.location = Some(input.to_string());
            diagnostic.context.push("eval".to_string());
            diagnostic
        }
    };
    write_diagnostics(stderr, &[diagnostic]);
}

pub(super) fn diagnostic_from_caap_error(input: &str, error: &CaapError) -> Option<Diagnostic> {
    Diagnostic::from_caap_error(error, Some(input))
}
