use std::io::Write;

use super::run::{cmd_launch, cmd_run_bare};
use super::EXIT_USAGE;

pub(super) fn dispatch(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    match args {
        [] => {
            write_help(stdout);
            0
        }
        [only] if only == "-h" || only == "--help" => {
            write_help(stdout);
            0
        }
        [program] => cmd_run_bare(program, stdout, stderr),
        [bootstrap, program, rest @ ..] => {
            for arg in [bootstrap, program] {
                if arg.starts_with('-') {
                    let _ = writeln!(
                        stderr,
                        "the CAAP CLI takes no flags (got {arg}); usage: caap [BOOTSTRAP] PROGRAM [ARG...]"
                    );
                    return EXIT_USAGE;
                }
            }
            cmd_launch(bootstrap, program, rest, stdout, stderr)
        }
    }
}

pub(super) fn write_help(stdout: &mut dyn Write) {
    let _ = writeln!(
        stdout,
        "CAAP CLI — a launcher: it always executes\n\
         \n\
         usage:\n\
         \x20 caap PROGRAM                      evaluate PROGRAM on the bare kernel\n\
         \x20                                   (PROGRAM may be '-' to read stdin)\n\
         \x20 caap BOOTSTRAP PROGRAM [ARG...]   execute BOOTSTRAP (the stdlib policy\n\
         \x20                                   file), then run PROGRAM under it; ARGs\n\
         \x20                                   reach the program as cli.args and as\n\
         \x20                                   sys process args\n\
         \n\
         There are no other commands and no flags. Checking, emitting LLVM IR,\n\
         building native executables — each is a program you run (see tools/*.caap);\n\
         the artifact is whatever that execution produces.\n\
         \n\
         The program's result decides the outcome:\n\
         \x20 null   -> exit 0\n\
         \x20 int    -> the process exit code\n\
         \x20 other  -> printed to stdout, exit 0"
    );
}
