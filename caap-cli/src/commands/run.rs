use std::io::Write;

use caap_core::frontend::eval_source;
use caap_core::values::RuntimeValue;

use super::output::{write_caap_error, write_eval_signal};
use super::shared::{
    canonical_path_string, evaluate_launch_command, launch_command_source,
    make_streaming_diagnostic_sink, read_input,
};
use super::{EXIT_RUNTIME, EXIT_USAGE};

/// `caap PROGRAM` — evaluate a source file (or stdin via `-`) on the bare
/// kernel, with no bootstrap and no host services. The REPL-ish degenerate
/// case: a non-null result is printed.
pub(super) fn cmd_run_bare(input: &str, stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let source = match read_input(input) {
        Ok(source) => source,
        Err(error) => {
            write_caap_error(stderr, input, &error);
            return EXIT_USAGE;
        }
    };
    match eval_source(&source) {
        Ok(RuntimeValue::Null) => 0,
        Ok(value) => {
            let _ = writeln!(stdout, "{value}");
            let _ = stdout.flush();
            0
        }
        Err(error) => {
            write_eval_signal(stderr, input, &error);
            EXIT_RUNTIME
        }
    }
}

/// `caap BOOTSTRAP PROGRAM [ARG...]` — the launcher. Executes BOOTSTRAP as a
/// bootstrap file, then hands PROGRAM and the args to the bootstrap's
/// `cli.main` policy (falling back to executing PROGRAM as another bootstrap
/// file when the bootstrap registers no `cli.main`). The artifact is whatever
/// the execution produces; an int result becomes the process exit code, which
/// keeps interpreted programs and natively compiled ones on the same contract.
pub(super) fn cmd_launch(
    bootstrap: &str,
    program: &str,
    args: &[String],
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> i32 {
    let bootstrap_path = match canonical_path_string(bootstrap) {
        Ok(path) => path,
        Err(error) => {
            write_caap_error(stderr, bootstrap, &error);
            return EXIT_USAGE;
        }
    };
    let program_path = match canonical_path_string(program) {
        Ok(path) => path,
        Err(error) => {
            write_caap_error(stderr, program, &error);
            return EXIT_USAGE;
        }
    };
    // Make `sys` process args report the program-relative argv so the same
    // `args` call works identically interpreted and natively compiled.
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push(program_path.clone());
    argv.extend(args.iter().cloned());
    caap_sys_runtime::proc::set_args_override(argv);
    let source = match launch_command_source(&bootstrap_path, &program_path, args) {
        Ok(source) => source,
        Err(error) => {
            write_caap_error(stderr, program, &error);
            return EXIT_USAGE;
        }
    };
    let sink = make_streaming_diagnostic_sink();
    match evaluate_launch_command(&source, sink) {
        Ok(RuntimeValue::Null) => 0,
        Ok(RuntimeValue::Int(code)) => (code & 0xff) as i32,
        Ok(value) => {
            let _ = writeln!(stdout, "{value}");
            let _ = stdout.flush();
            0
        }
        Err(error) => {
            write_caap_error(stderr, program, &error);
            EXIT_RUNTIME
        }
    }
}
