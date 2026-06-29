use std::io::{self, Write};

mod dispatch;
mod output;
mod run;
mod shared;

pub const EXIT_USAGE: i32 = 2;
pub const EXIT_RUNTIME: i32 = 70;

pub fn main_with_stdio(args: impl IntoIterator<Item = String>) -> i32 {
    let mut stdout = io::stdout();
    let mut stderr = io::stderr();
    run_cli(args, &mut stdout, &mut stderr)
}

pub fn run_cli(
    args: impl IntoIterator<Item = String>,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let args: Vec<String> = args.into_iter().collect();
    dispatch::dispatch(&args, stdout, stderr)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use caap_core::diagnostics::{Diagnostic, DiagnosticCode};
    use caap_core::error::CaapError;

    use super::output::diagnostic_from_caap_error;
    use super::run_cli;
    use super::{EXIT_RUNTIME, EXIT_USAGE};

    fn temp_path(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("caap-cli-{}-{name}", std::process::id()))
            .to_string_lossy()
            .to_string()
    }

    fn run(args: &[&str]) -> (i32, String, String) {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let code = run_cli(
            args.iter().map(|arg| arg.to_string()),
            &mut stdout,
            &mut stderr,
        );
        (
            code,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    }

    #[test]
    fn cli_bare_run_evaluates_source_file_and_prints_value() {
        let path = temp_path("bare_run.caap");
        fs::write(&path, "(int_add 20 22)").unwrap();

        let (code, stdout, stderr) = run(&[&path]);
        fs::remove_file(&path).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "42\n");
    }

    #[test]
    fn cli_bare_run_renders_parse_error_as_diagnostic() {
        let path = temp_path("bare_run_parse_error.caap");
        fs::write(&path, "(int_add 1 @bad)").unwrap();

        let (code, _, stderr) = run(&[&path]);
        fs::remove_file(&path).ok();

        assert_eq!(code, EXIT_RUNTIME);
        assert!(stderr.contains("error["), "{stderr}");
    }

    #[test]
    fn cli_help_describes_launcher_contract() {
        for args in [&[][..], &["--help"][..], &["-h"][..]] {
            let (code, stdout, _) = run(args);
            assert_eq!(code, 0);
            assert!(
                stdout.contains("caap BOOTSTRAP PROGRAM [ARG...]"),
                "{stdout}"
            );
        }
    }

    #[test]
    fn cli_rejects_flags() {
        let (code, _, stderr) = run(&["--bootstrap", "x.caap"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(stderr.contains("takes no flags"), "{stderr}");
    }

    #[test]
    fn cli_launch_reports_missing_paths_as_usage_errors() {
        let (code, _, stderr) = run(&["/nonexistent/bootstrap.caap", "/nonexistent/prog.caap"]);
        assert_eq!(code, EXIT_USAGE);
        assert!(stderr.contains("failed to resolve"), "{stderr}");
    }

    #[test]
    fn diagnostic_from_caap_error_preserves_embedded_diagnostic() {
        let diagnostic = Diagnostic::error("capability denied")
            .and_then(|diagnostic| diagnostic.with_code(DiagnosticCode::Capability))
            .unwrap();
        let rendered =
            diagnostic_from_caap_error("demo.caap", &CaapError::diagnostic(diagnostic)).unwrap();

        assert_eq!(rendered.code.as_deref(), Some("CAAP-CAP-001"));
        assert_eq!(rendered.message, "capability denied");
        assert_eq!(rendered.location.as_deref(), Some("demo.caap"));
        assert_eq!(rendered.context, vec!["diagnostic"]);
    }
}
