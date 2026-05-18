use std::fs;
use std::process::{Command, Output};

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "caap-rust-cli-integration-{}-{name}",
        std::process::id()
    ))
}

fn caap(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_caap"))
        .args(args)
        .output()
        .expect("failed to run caap binary")
}

#[test]
fn cli_run_evaluates_file_end_to_end() {
    let path = temp_path("run.caap");
    fs::write(&path, "(int-add 2 3)\n").unwrap();

    let output = caap(&["run", path.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "5\n");
    assert!(String::from_utf8(output.stderr).unwrap().is_empty());
}

#[test]
fn cli_check_format_and_compile_syntax_only_validate_file() {
    let path = temp_path("check-format-compile.caap");
    fs::write(&path, "( int-add 1 2 )\n").unwrap();

    let check = caap(&["check", path.to_str().unwrap()]);
    assert!(check.status.success(), "{check:?}");

    let formatted = caap(&["format", "--stdout", path.to_str().unwrap()]);
    assert!(formatted.status.success(), "{formatted:?}");
    let formatted_stdout = String::from_utf8(formatted.stdout).unwrap();
    assert!(formatted_stdout.contains("(int-add 1 2)"));

    let compile = caap(&["compile", "--syntax-only", path.to_str().unwrap()]);
    assert!(compile.status.success(), "{compile:?}");
    assert!(String::from_utf8(compile.stderr).unwrap().is_empty());
}
