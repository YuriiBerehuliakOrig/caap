use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn temp_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "caap-cli-integration-{}-{name}",
        std::process::id()
    ))
}

fn bootstrap_path() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../stdlib/bootstrap.caap")
        .display()
        .to_string()
}

fn caap(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_caap"))
        .args(args)
        .output()
        .expect("failed to run caap binary")
}

#[test]
fn cli_bare_run_evaluates_file_end_to_end() {
    let path = temp_path("run.caap");
    fs::write(&path, "(int_add 2 3)\n").unwrap();

    let output = caap(&[path.to_str().unwrap()]);
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8(output.stdout).unwrap(), "5\n");
    assert!(String::from_utf8(output.stderr).unwrap().is_empty());
}

#[test]
fn cli_launch_int_result_becomes_the_process_exit_code() {
    let path = temp_path("exit_code.caap");
    fs::write(&path, "(int_add 40 2)\n").unwrap();
    let bootstrap = bootstrap_path();

    let output = caap(&[&bootstrap, path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(42), "{output:?}");
    assert!(String::from_utf8(output.stdout).unwrap().is_empty());
}
