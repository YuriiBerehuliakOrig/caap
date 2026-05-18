use std::collections::BTreeMap;
use std::fs;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::compiler::CompilerHost;
use crate::diagnostics::{render_diagnostic, CompilerEvent, Diagnostic};
use crate::frontend::{
    ast_json, check_source, eval_source, format_parsed_source, format_source, parse, ParsedSource,
};
use crate::semantic::PhasePolicy;
use crate::unit::Unit;
use crate::values::RuntimeValue;

pub const EXIT_USAGE: i32 = 2;
pub const EXIT_PARSE: i32 = 65;
pub const EXIT_COMPILE: i32 = 66;
pub const EXIT_RUNTIME: i32 = 70;

#[derive(Clone, Debug, Eq, PartialEq)]
enum TraceFormat {
    Mermaid,
    Json,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TraceOptions {
    enabled: bool,
    format: TraceFormat,
    output: Option<String>,
}

impl Default for TraceOptions {
    fn default() -> Self {
        Self {
            enabled: false,
            format: TraceFormat::Mermaid,
            output: None,
        }
    }
}

struct BootstrappedValueResult {
    value: RuntimeValue,
    events: Vec<CompilerEvent>,
    diagnostics: Vec<Diagnostic>,
}

struct BootstrappedTextResult {
    text: String,
    events: Vec<CompilerEvent>,
    diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct BootstrapOptions {
    paths: Vec<String>,
    internal_capabilities: Vec<String>,
}

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
    match args.first().map(String::as_str) {
        Some("run") => cmd_run(&args[1..], stdout, stderr),
        Some("check") | Some("lint") => cmd_check(&args[1..], stderr),
        Some("format") => cmd_format(&args[1..], stdout, stderr),
        Some("ast-json") => cmd_ast_json(&args[1..], stdout, stderr),
        Some("compile") => cmd_compile(&args[1..], stderr),
        Some("llvm-ir") => cmd_llvm_ir(&args[1..], stdout, stderr),
        Some("-h") | Some("--help") | None => {
            write_help(stdout);
            0
        }
        Some(command) => {
            let _ = writeln!(stderr, "unknown command: {command}");
            EXIT_USAGE
        }
    }
}

fn cmd_run(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Ok(options) = parse_run_options(args, stderr) else {
        return EXIT_USAGE;
    };
    if !options.bootstrap.paths.is_empty() {
        return match options.root.as_deref() {
            Some(root) => run_bootstrapped_root(
                root,
                &options.input,
                &options.bootstrap,
                stdout,
                options.output.as_deref(),
                &options.trace,
                stderr,
            ),
            None => run_bootstrapped_source(
                &options.input,
                &options.bootstrap,
                stdout,
                options.output.as_deref(),
                &options.trace,
                stderr,
            ),
        };
    }
    if options.root.is_some() {
        let _ = writeln!(
            stderr,
            "run --root requires at least one --bootstrap file in the Rust CAAP CLI"
        );
        return EXIT_USAGE;
    }
    let source = match read_input(&options.input) {
        Ok(source) => source,
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            return EXIT_USAGE;
        }
    };
    match eval_source(&source) {
        Ok(RuntimeValue::Null) => 0,
        Ok(value) => write_output(
            stdout,
            options.output.as_deref(),
            &format!("{value}\n"),
            stderr,
        ),
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            EXIT_RUNTIME
        }
    }
}

fn cmd_check(args: &[String], stderr: &mut dyn Write) -> i32 {
    if args.is_empty() {
        let _ = writeln!(stderr, "check requires at least one input path");
        return EXIT_USAGE;
    }
    let mut exit_code = 0;
    for input in args {
        match read_input(input).and_then(|source| check_source(&source)) {
            Ok(()) => {}
            Err(error) => {
                let _ = writeln!(stderr, "{input}: {error}");
                exit_code = EXIT_PARSE;
            }
        }
    }
    exit_code
}

fn cmd_format(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let mut check = false;
    let mut write = false;
    let mut to_stdout = false;
    let mut paths = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--check" => check = true,
            "--write" => write = true,
            "--stdout" => to_stdout = true,
            other if other.starts_with('-') => {
                let _ = writeln!(stderr, "unknown format option: {other}");
                return EXIT_USAGE;
            }
            path => paths.push(path.to_string()),
        }
    }
    if paths.is_empty() {
        let _ = writeln!(stderr, "format requires at least one input path");
        return EXIT_USAGE;
    }
    if to_stdout && paths.len() != 1 {
        let _ = writeln!(stderr, "--stdout requires exactly one input path");
        return EXIT_USAGE;
    }
    if [check, write, to_stdout]
        .into_iter()
        .filter(|flag| *flag)
        .count()
        > 1
    {
        let _ = writeln!(
            stderr,
            "--check, --write, and --stdout are mutually exclusive"
        );
        return EXIT_USAGE;
    }

    let mut exit_code = 0;
    for path in paths {
        let source = match read_input(&path) {
            Ok(source) => source,
            Err(error) => {
                let _ = writeln!(stderr, "{path}: {error}");
                exit_code = EXIT_USAGE;
                continue;
            }
        };
        let formatted = match format_source(&source) {
            Ok(formatted) => formatted,
            Err(error) => {
                let _ = writeln!(stderr, "{path}: {error}");
                exit_code = EXIT_PARSE;
                continue;
            }
        };
        if to_stdout {
            let _ = write!(stdout, "{formatted}");
        } else if write {
            if source != formatted {
                if let Err(error) = fs::write(&path, formatted) {
                    let _ = writeln!(stderr, "{path}: failed to write formatted source: {error}");
                    exit_code = EXIT_USAGE;
                }
            }
        } else if check {
            if source != formatted {
                let _ = writeln!(stderr, "would reformat {path}");
                exit_code = EXIT_COMPILE;
            }
        } else {
            let _ = writeln!(stdout, "{formatted}");
        }
    }
    exit_code
}

fn cmd_ast_json(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Some(command) = args.first().map(String::as_str) else {
        let _ = writeln!(stderr, "ast-json requires a subcommand");
        return EXIT_USAGE;
    };
    let Some(input) = args.get(1) else {
        let _ = writeln!(stderr, "ast-json {command} requires an input path");
        return EXIT_USAGE;
    };
    match command {
        "to-json" => {
            let source = match read_input(input) {
                Ok(source) => source,
                Err(error) => {
                    let _ = writeln!(stderr, "{error}");
                    return EXIT_USAGE;
                }
            };
            match ast_json(&source) {
                Ok(json) => {
                    let _ = writeln!(stdout, "{json}");
                    0
                }
                Err(error) => {
                    let _ = writeln!(stderr, "{error}");
                    EXIT_PARSE
                }
            }
        }
        "to-caap" => {
            let text = match read_input(input) {
                Ok(text) => text,
                Err(error) => {
                    let _ = writeln!(stderr, "{error}");
                    return EXIT_USAGE;
                }
            };
            match serde_json::from_str::<ParsedSource>(&text) {
                Ok(parsed) => {
                    let _ = writeln!(stdout, "{}", format_parsed_source(&parsed));
                    0
                }
                Err(error) => {
                    let _ = writeln!(stderr, "failed to parse JSON AST: {error}");
                    EXIT_PARSE
                }
            }
        }
        "roundtrip-check" => {
            let text = match read_input(input) {
                Ok(text) => text,
                Err(error) => {
                    let _ = writeln!(stderr, "{error}");
                    return EXIT_USAGE;
                }
            };
            match ast_json(&text).and_then(|json| {
                let parsed: ParsedSource = serde_json::from_str(&json)
                    .map_err(|error| format!("failed to parse generated JSON AST: {error}"))?;
                let rendered = format_parsed_source(&parsed);
                let reparsed = ast_json(&rendered)?;
                if json == reparsed {
                    Ok(())
                } else {
                    Err("JSON AST roundtrip changed after CAAP rendering".to_string())
                }
            }) {
                Ok(()) => 0,
                Err(error) => {
                    let _ = writeln!(stderr, "{error}");
                    EXIT_COMPILE
                }
            }
        }
        other => {
            let _ = writeln!(stderr, "unknown ast-json subcommand: {other}");
            EXIT_USAGE
        }
    }
}

fn cmd_compile(args: &[String], stderr: &mut dyn Write) -> i32 {
    let mut syntax_only = false;
    let mut bootstrap = BootstrapOptions::default();
    let mut root = None;
    let mut target = "check".to_string();
    let mut entry = None;
    let mut output = None;
    let mut trace = TraceOptions::default();
    let mut input = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--syntax-only" => syntax_only = true,
            "--bootstrap" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "--bootstrap requires a path");
                    return EXIT_USAGE;
                };
                bootstrap.paths.push(path.clone());
            }
            "--internal-capability" => {
                index += 1;
                let Some(capability) = args.get(index) else {
                    let _ = writeln!(stderr, "--internal-capability requires a capability name");
                    return EXIT_USAGE;
                };
                bootstrap.internal_capabilities.push(capability.clone());
            }
            "--target" => {
                index += 1;
                let Some(target_value) = args.get(index) else {
                    let _ = writeln!(stderr, "--target requires a value");
                    return EXIT_USAGE;
                };
                if target_value != "check" && target_value != "native-exe" {
                    let _ = writeln!(stderr, "unsupported compile target: {target_value}");
                    return EXIT_USAGE;
                }
                target = target_value.clone();
            }
            "--root" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "--root requires a path");
                    return EXIT_USAGE;
                };
                root = Some(path.clone());
            }
            "--entry" => {
                index += 1;
                let Some(name) = args.get(index) else {
                    let _ = writeln!(stderr, "--entry requires a registered emitter name");
                    return EXIT_USAGE;
                };
                entry = Some(name.clone());
            }
            "-o" | "--output" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "{} requires a path", args[index - 1]);
                    return EXIT_USAGE;
                };
                output = Some(path.clone());
            }
            "--trace-compile" => trace.enabled = true,
            "--trace-format" => {
                index += 1;
                let Some(format) = args.get(index) else {
                    let _ = writeln!(stderr, "--trace-format requires a value");
                    return EXIT_USAGE;
                };
                let Some(format) = parse_trace_format(format) else {
                    let _ = writeln!(stderr, "--trace-format must be json or mermaid");
                    return EXIT_USAGE;
                };
                trace.format = format;
            }
            "--trace-output" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "--trace-output requires a path");
                    return EXIT_USAGE;
                };
                trace.output = Some(path.clone());
            }
            other if other.starts_with('-') => {
                let _ = writeln!(stderr, "unknown compile option: {other}");
                return EXIT_USAGE;
            }
            path => input = Some(path.to_string()),
        }
        index += 1;
    }
    let Some(input) = input else {
        let _ = writeln!(stderr, "compile requires an input path");
        return EXIT_USAGE;
    };
    if target == "native-exe" {
        if syntax_only {
            let _ = writeln!(
                stderr,
                "compile --target native-exe cannot be combined with --syntax-only"
            );
            return EXIT_USAGE;
        }
        if bootstrap.paths.is_empty() {
            let _ = writeln!(
                stderr,
                "compile --target native-exe requires at least one --bootstrap file"
            );
            return EXIT_USAGE;
        }
        let Some(output) = output else {
            let _ = writeln!(stderr, "compile --target native-exe requires -o/--output");
            return EXIT_USAGE;
        };
        let llvm_ir = match root.as_deref() {
            Some(root) => emit_bootstrapped_root_llvm(
                root,
                &input,
                entry.as_deref().unwrap_or(""),
                &bootstrap,
            ),
            None => {
                emit_bootstrapped_source_llvm(&input, entry.as_deref().unwrap_or(""), &bootstrap)
            }
        };
        return match llvm_ir.and_then(|llvm_ir| {
            compile_llvm_ir_to_native_executable(&llvm_ir.text, &output)?;
            Ok(llvm_ir)
        }) {
            Ok(llvm_ir) => finish_with_diagnostics_and_trace(
                0,
                &trace,
                llvm_ir.events,
                llvm_ir.diagnostics,
                stderr,
            ),
            Err(error) => {
                let _ = writeln!(stderr, "{error}");
                EXIT_COMPILE
            }
        };
    }
    if root.is_some() && bootstrap.paths.is_empty() {
        let _ = writeln!(
            stderr,
            "compile --root requires at least one --bootstrap file in the Rust CAAP CLI"
        );
        return EXIT_USAGE;
    }
    if !bootstrap.paths.is_empty() && !syntax_only {
        return match root.as_deref() {
            Some(root) => check_bootstrapped_root(root, &input, &bootstrap, &trace, stderr),
            None => check_bootstrapped_source(&input, &bootstrap, &trace, stderr),
        };
    }
    if !syntax_only {
        let _ = writeln!(
            stderr,
            "Rust compile without --bootstrap currently supports only --syntax-only"
        );
        return EXIT_USAGE;
    }
    match read_input(&input).and_then(|source| check_source(&source)) {
        Ok(()) => 0,
        Err(error) => {
            let _ = writeln!(stderr, "{input}: {error}");
            EXIT_PARSE
        }
    }
}

fn cmd_llvm_ir(args: &[String], stdout: &mut dyn Write, stderr: &mut dyn Write) -> i32 {
    let Ok(options) = parse_llvm_ir_options(args, stderr) else {
        return EXIT_USAGE;
    };
    if options.bootstrap.paths.is_empty() {
        let _ = writeln!(stderr, "no compiler stages registered");
        return EXIT_USAGE;
    }
    let result = match options.root.as_deref() {
        Some(root) => emit_bootstrapped_root_llvm(
            root,
            &options.input,
            options.entry.as_deref().unwrap_or(""),
            &options.bootstrap,
        ),
        None => emit_bootstrapped_source_llvm(
            &options.input,
            options.entry.as_deref().unwrap_or(""),
            &options.bootstrap,
        ),
    };
    match result {
        Ok(result) => {
            let code = write_output(stdout, options.output.as_deref(), &result.text, stderr);
            finish_with_diagnostics_and_trace(
                code,
                &options.trace,
                result.events,
                result.diagnostics,
                stderr,
            )
        }
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            EXIT_COMPILE
        }
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct RunOptions {
    input: String,
    output: Option<String>,
    bootstrap: BootstrapOptions,
    root: Option<String>,
    trace: TraceOptions,
}

fn parse_run_options(args: &[String], stderr: &mut dyn Write) -> Result<RunOptions, ()> {
    let mut input = None;
    let mut output = None;
    let mut bootstrap = BootstrapOptions::default();
    let mut root = None;
    let mut trace = TraceOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-o" | "--output" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "{} requires a path", args[index - 1]);
                    return Err(());
                };
                output = Some(path.clone());
            }
            "--bootstrap" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "--bootstrap requires a path");
                    return Err(());
                };
                bootstrap.paths.push(path.clone());
            }
            "--internal-capability" => {
                index += 1;
                let Some(capability) = args.get(index) else {
                    let _ = writeln!(stderr, "--internal-capability requires a capability name");
                    return Err(());
                };
                bootstrap.internal_capabilities.push(capability.clone());
            }
            "--root" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "--root requires a path");
                    return Err(());
                };
                root = Some(path.clone());
            }
            "--trace-compile" => trace.enabled = true,
            "--trace-format" => {
                index += 1;
                let Some(format) = args.get(index) else {
                    let _ = writeln!(stderr, "--trace-format requires a value");
                    return Err(());
                };
                let Some(format) = parse_trace_format(format) else {
                    let _ = writeln!(stderr, "--trace-format must be json or mermaid");
                    return Err(());
                };
                trace.format = format;
            }
            "--trace-output" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "--trace-output requires a path");
                    return Err(());
                };
                trace.output = Some(path.clone());
            }
            other if other.starts_with('-') => {
                let _ = writeln!(stderr, "unknown run option: {other}");
                return Err(());
            }
            path => input = Some(path.to_string()),
        }
        index += 1;
    }
    match input {
        Some(input) => Ok(RunOptions {
            input,
            output,
            bootstrap,
            root,
            trace,
        }),
        None => {
            let _ = writeln!(stderr, "run requires an input path");
            Err(())
        }
    }
}

#[derive(Debug, Default, Eq, PartialEq)]
struct LlvmIrOptions {
    input: String,
    output: Option<String>,
    bootstrap: BootstrapOptions,
    root: Option<String>,
    entry: Option<String>,
    trace: TraceOptions,
}

fn parse_llvm_ir_options(args: &[String], stderr: &mut dyn Write) -> Result<LlvmIrOptions, ()> {
    let mut input = None;
    let mut output = None;
    let mut bootstrap = BootstrapOptions::default();
    let mut root = None;
    let mut entry = None;
    let mut trace = TraceOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "-o" | "--output" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "{} requires a path", args[index - 1]);
                    return Err(());
                };
                output = Some(path.clone());
            }
            "--bootstrap" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "--bootstrap requires a path");
                    return Err(());
                };
                bootstrap.paths.push(path.clone());
            }
            "--internal-capability" => {
                index += 1;
                let Some(capability) = args.get(index) else {
                    let _ = writeln!(stderr, "--internal-capability requires a capability name");
                    return Err(());
                };
                bootstrap.internal_capabilities.push(capability.clone());
            }
            "--root" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "--root requires a path");
                    return Err(());
                };
                root = Some(path.clone());
            }
            "--entry" => {
                index += 1;
                let Some(name) = args.get(index) else {
                    let _ = writeln!(stderr, "--entry requires a registered emitter name");
                    return Err(());
                };
                entry = Some(name.clone());
            }
            "--trace-compile" => trace.enabled = true,
            "--trace-format" => {
                index += 1;
                let Some(format) = args.get(index) else {
                    let _ = writeln!(stderr, "--trace-format requires a value");
                    return Err(());
                };
                let Some(format) = parse_trace_format(format) else {
                    let _ = writeln!(stderr, "--trace-format must be json or mermaid");
                    return Err(());
                };
                trace.format = format;
            }
            "--trace-output" => {
                index += 1;
                let Some(path) = args.get(index) else {
                    let _ = writeln!(stderr, "--trace-output requires a path");
                    return Err(());
                };
                trace.output = Some(path.clone());
            }
            other if other.starts_with('-') => {
                let _ = writeln!(stderr, "unknown llvm-ir option: {other}");
                return Err(());
            }
            path => input = Some(path.to_string()),
        }
        index += 1;
    }
    match input {
        Some(input) => Ok(LlvmIrOptions {
            input,
            output,
            bootstrap,
            root,
            entry,
            trace,
        }),
        None => {
            let _ = writeln!(stderr, "llvm-ir requires an input path");
            Err(())
        }
    }
}

fn run_bootstrapped_source(
    input: &str,
    bootstrap: &BootstrapOptions,
    stdout: &mut dyn Write,
    output: Option<&str>,
    trace: &TraceOptions,
    stderr: &mut dyn Write,
) -> i32 {
    match evaluate_bootstrapped_source_command(input, bootstrap, "stdlib.module.run-source") {
        Ok(result) => {
            let code = match result.value {
                RuntimeValue::Null => 0,
                value => write_output(stdout, output, &format!("{value}\n"), stderr),
            };
            finish_with_diagnostics_and_trace(
                code,
                trace,
                result.events,
                result.diagnostics,
                stderr,
            )
        }
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            EXIT_RUNTIME
        }
    }
}

fn check_bootstrapped_source(
    input: &str,
    bootstrap: &BootstrapOptions,
    trace: &TraceOptions,
    stderr: &mut dyn Write,
) -> i32 {
    match evaluate_bootstrapped_source_command(input, bootstrap, "stdlib.module.check-source") {
        Ok(result) => {
            finish_with_diagnostics_and_trace(0, trace, result.events, result.diagnostics, stderr)
        }
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            EXIT_COMPILE
        }
    }
}

fn run_bootstrapped_root(
    root: &str,
    entry: &str,
    bootstrap: &BootstrapOptions,
    stdout: &mut dyn Write,
    output: Option<&str>,
    trace: &TraceOptions,
    stderr: &mut dyn Write,
) -> i32 {
    match evaluate_bootstrapped_root_command(root, entry, bootstrap, "stdlib.module.run-from-root")
    {
        Ok(result) => {
            let code = match result.value {
                RuntimeValue::Null => 0,
                value => write_output(stdout, output, &format!("{value}\n"), stderr),
            };
            finish_with_diagnostics_and_trace(
                code,
                trace,
                result.events,
                result.diagnostics,
                stderr,
            )
        }
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            EXIT_RUNTIME
        }
    }
}

fn check_bootstrapped_root(
    root: &str,
    entry: &str,
    bootstrap: &BootstrapOptions,
    trace: &TraceOptions,
    stderr: &mut dyn Write,
) -> i32 {
    match evaluate_bootstrapped_root_command(root, entry, bootstrap, "stdlib.module.check-root") {
        Ok(result) => {
            finish_with_diagnostics_and_trace(0, trace, result.events, result.diagnostics, stderr)
        }
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            EXIT_COMPILE
        }
    }
}

fn emit_bootstrapped_source_llvm(
    input: &str,
    entry: &str,
    bootstrap: &BootstrapOptions,
) -> Result<BootstrappedTextResult, String> {
    let result = evaluate_bootstrapped_source_emit_command(
        input,
        entry,
        bootstrap,
        "stdlib.module.emit-source-llvm",
    )?;
    Ok(BootstrappedTextResult {
        text: require_string_result(result.value, "emit-source-llvm")?,
        events: result.events,
        diagnostics: result.diagnostics,
    })
}

fn emit_bootstrapped_root_llvm(
    root: &str,
    module_entry: &str,
    emitter_entry: &str,
    bootstrap: &BootstrapOptions,
) -> Result<BootstrappedTextResult, String> {
    let result = evaluate_bootstrapped_root_emit_command(
        root,
        module_entry,
        emitter_entry,
        bootstrap,
        "stdlib.module.emit-root-llvm",
    )?;
    Ok(BootstrappedTextResult {
        text: require_string_result(result.value, "emit-root-llvm")?,
        events: result.events,
        diagnostics: result.diagnostics,
    })
}

fn evaluate_bootstrapped_source_command(
    input: &str,
    bootstrap: &BootstrapOptions,
    command_name: &str,
) -> Result<BootstrappedValueResult, String> {
    evaluate_bootstrapped_module_command(bootstrap, |forms| {
        forms.push(format!(
            "(bind ((callbacks (ctfe-compiler-module-root-callbacks compiler)) \
                    (command (ctfe-compiler-lookup-value compiler {}))) \
               (command {} callbacks))",
            caap_string_literal(command_name)?,
            caap_string_literal(input)?,
        ));
        Ok(())
    })
}

fn evaluate_bootstrapped_source_emit_command(
    input: &str,
    entry: &str,
    bootstrap: &BootstrapOptions,
    command_name: &str,
) -> Result<BootstrappedValueResult, String> {
    evaluate_bootstrapped_module_command(bootstrap, |forms| {
        forms.push(format!(
            "(bind ((callbacks (ctfe-compiler-module-root-callbacks compiler)) \
                    (command (ctfe-compiler-lookup-value compiler {}))) \
               (command {} {} callbacks))",
            caap_string_literal(command_name)?,
            caap_string_literal(input)?,
            caap_string_literal(entry)?,
        ));
        Ok(())
    })
}

fn evaluate_bootstrapped_root_command(
    root: &str,
    entry: &str,
    bootstrap: &BootstrapOptions,
    command_name: &str,
) -> Result<BootstrappedValueResult, String> {
    let root = canonical_path_string(root)?;
    evaluate_bootstrapped_module_command(bootstrap, |forms| {
        forms.push(format!(
            "(bind ((callbacks (ctfe-compiler-module-root-callbacks compiler)) \
                    (command (ctfe-compiler-lookup-value compiler {}))) \
               (command {} {} callbacks))",
            caap_string_literal(command_name)?,
            caap_string_literal(&root)?,
            caap_string_literal(entry)?,
        ));
        Ok(())
    })
}

fn evaluate_bootstrapped_root_emit_command(
    root: &str,
    module_entry: &str,
    emitter_entry: &str,
    bootstrap: &BootstrapOptions,
    command_name: &str,
) -> Result<BootstrappedValueResult, String> {
    let root = canonical_path_string(root)?;
    evaluate_bootstrapped_module_command(bootstrap, |forms| {
        forms.push(format!(
            "(bind ((callbacks (ctfe-compiler-module-root-callbacks compiler)) \
                    (command (ctfe-compiler-lookup-value compiler {}))) \
               (command {} {} {} callbacks))",
            caap_string_literal(command_name)?,
            caap_string_literal(&root)?,
            caap_string_literal(module_entry)?,
            caap_string_literal(emitter_entry)?,
        ));
        Ok(())
    })
}

fn evaluate_bootstrapped_module_command(
    bootstrap: &BootstrapOptions,
    append_command: impl FnOnce(&mut Vec<String>) -> Result<(), String>,
) -> Result<BootstrappedValueResult, String> {
    let host = bootstrapped_cli_host()?;
    let mut compiler = host.new_session();
    let source = bootstrapped_module_command(bootstrap, append_command)?;
    let graph = parse(&source)?;
    let unit = Unit::from_graph("cli.bootstrap-command", graph)?;
    let value = compiler
        .evaluation()
        .evaluate(&unit, PhasePolicy::CompileTime, [])
        .map_err(|signal| signal.to_string())?;
    Ok(BootstrappedValueResult {
        value,
        events: compiler.events().events().to_vec(),
        diagnostics: compiler.diagnostics().to_vec(),
    })
}

fn bootstrapped_cli_host() -> Result<CompilerHost, String> {
    let mut host = CompilerHost::new();
    host.register_default_runtime_system_libraries()?;
    host.register_default_compile_time_system_libraries()?;
    Ok(host)
}

fn bootstrapped_module_command(
    bootstrap: &BootstrapOptions,
    append_command: impl FnOnce(&mut Vec<String>) -> Result<(), String>,
) -> Result<String, String> {
    let mut forms = Vec::new();
    let capabilities = caap_string_list(&bootstrap.internal_capabilities)?;
    for path in &bootstrap.paths {
        if bootstrap.internal_capabilities.is_empty() {
            forms.push(format!(
                "(ctfe-compiler-execute-bootstrap-file compiler {})",
                caap_string_literal(path)?
            ));
        } else {
            forms.push(format!(
                "(ctfe-compiler-execute-bootstrap-file compiler {} {})",
                caap_string_literal(path)?,
                capabilities
            ));
        }
    }
    append_command(&mut forms)?;
    Ok(format!("(do {})", forms.join(" ")))
}

fn caap_string_literal(value: &str) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| format!("failed to quote CAAP string: {error}"))
}

fn caap_string_list(values: &[String]) -> Result<String, String> {
    let mut items = Vec::with_capacity(values.len());
    for value in values {
        items.push(caap_string_literal(value)?);
    }
    Ok(format!("(list-of {})", items.join(" ")))
}

fn canonical_path_string(path: &str) -> Result<String, String> {
    std::fs::canonicalize(path)
        .map_err(|error| format!("failed to resolve {}: {error}", Path::new(path).display()))
        .map(|path| path.display().to_string())
}

fn require_string_result(value: RuntimeValue, command: &str) -> Result<String, String> {
    match value {
        RuntimeValue::Str(text) => Ok(text.to_string()),
        RuntimeValue::Null => Err(format!("{command} did not return LLVM IR text")),
        other => Err(format!("{command} must return LLVM IR text, got {other}")),
    }
}

fn parse_trace_format(value: &str) -> Option<TraceFormat> {
    match value {
        "json" => Some(TraceFormat::Json),
        "mermaid" => Some(TraceFormat::Mermaid),
        _ => None,
    }
}

fn finish_with_trace(
    code: i32,
    trace: &TraceOptions,
    mut events: Vec<CompilerEvent>,
    stderr: &mut dyn Write,
) -> i32 {
    if !trace.enabled {
        return code;
    }
    if let Ok(event) = CompilerEvent::with_target(
        "cli.command.finish",
        None,
        "CLI command finished",
        [("exit_code".to_string(), code.to_string())],
    ) {
        events.push(event);
    }
    match write_compile_trace(trace, &events) {
        Ok(path) => {
            let _ = writeln!(stderr, "compile trace written to {}", path.display());
            code
        }
        Err(error) => {
            let _ = writeln!(stderr, "{error}");
            EXIT_USAGE
        }
    }
}

fn finish_with_diagnostics_and_trace(
    code: i32,
    trace: &TraceOptions,
    events: Vec<CompilerEvent>,
    diagnostics: Vec<Diagnostic>,
    stderr: &mut dyn Write,
) -> i32 {
    write_diagnostics(stderr, &diagnostics);
    finish_with_trace(code, trace, events, stderr)
}

fn write_diagnostics(stderr: &mut dyn Write, diagnostics: &[Diagnostic]) {
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

fn write_compile_trace(trace: &TraceOptions, events: &[CompilerEvent]) -> Result<PathBuf, String> {
    let path = trace.output.clone().unwrap_or_else(|| match trace.format {
        TraceFormat::Json => "compile-trace.json".to_string(),
        TraceFormat::Mermaid => "compile-trace.mmd".to_string(),
    });
    let path = PathBuf::from(path);
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create trace output directory {}: {error}",
                    parent.display()
                )
            })?;
        }
    }
    let text = match trace.format {
        TraceFormat::Json => render_trace_json(events)?,
        TraceFormat::Mermaid => render_trace_mermaid(events),
    };
    fs::write(&path, text)
        .map_err(|error| format!("failed to write compile trace {}: {error}", path.display()))?;
    Ok(path)
}

fn render_trace_json(events: &[CompilerEvent]) -> Result<String, String> {
    let items: Vec<serde_json::Value> = events
        .iter()
        .map(|event| {
            serde_json::json!({
                "kind": event.kind,
                "target": event.target,
                "message": event.message,
                "metadata": event.metadata.iter().map(|(key, value)| {
                    serde_json::json!({"key": key, "value": value})
                }).collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::to_string_pretty(&items)
        .map(|text| format!("{text}\n"))
        .map_err(|error| format!("failed to render compile trace JSON: {error}"))
}

fn render_trace_mermaid(events: &[CompilerEvent]) -> String {
    let mut text = String::from("sequenceDiagram\n    participant cli\n    participant compiler\n");
    for event in events {
        let label = mermaid_label(&format!("{}: {}", event.kind, event.message));
        text.push_str(&format!("    cli->>compiler: {label}\n"));
    }
    text
}

fn mermaid_label(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace(['\n', '\r'], " ")
        .replace(':', " -")
}

fn compile_llvm_ir_to_native_executable(llvm_ir: &str, output: &str) -> Result<(), String> {
    let clang = find_executable("clang")
        .ok_or_else(|| "clang is required for --target native-exe".to_string())?;
    let runtime_library = ensure_csys_runtime_static_library()?;
    let output_path = Path::new(output);
    if let Some(parent) = output_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create output directory {}: {error}",
                    parent.display()
                )
            })?;
        }
    }

    let temp_dir = std::env::temp_dir().join(format!(
        "caap-rust-native-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ));
    fs::create_dir_all(&temp_dir).map_err(|error| {
        format!(
            "failed to create native compile temp dir {}: {error}",
            temp_dir.display()
        )
    })?;
    let ir_path = temp_dir.join("program.ll");
    let object_path = temp_dir.join("program.o");
    let compile_result = (|| {
        fs::write(&ir_path, llvm_ir).map_err(|error| {
            format!(
                "failed to write temporary LLVM IR {}: {error}",
                ir_path.display()
            )
        })?;
        run_command(
            Command::new(&clang)
                .arg("-Wno-error=override-module")
                .arg("-x")
                .arg("ir")
                .arg("-c")
                .arg(&ir_path)
                .arg("-o")
                .arg(&object_path),
            "compile LLVM IR",
        )?;
        run_command(
            Command::new(&clang)
                .arg(&object_path)
                .arg(&runtime_library)
                .arg("-o")
                .arg(output_path),
            "link native executable",
        )
    })();
    fs::remove_dir_all(&temp_dir).ok();
    compile_result
}

fn ensure_csys_runtime_static_library() -> Result<PathBuf, String> {
    let clang = find_executable("clang")
        .ok_or_else(|| "clang is required to build the C sys runtime".to_string())?;
    let ar = find_executable("ar")
        .ok_or_else(|| "ar is required to build the C sys runtime".to_string())?;
    let repo = repo_root()?;
    let runtime_root = repo.join("runtime").join("csys");
    let include_dir = runtime_root.join("include");
    let source_dir = runtime_root.join("src");
    if !include_dir.is_dir() || !source_dir.is_dir() {
        return Err(format!(
            "C sys runtime sources are required for --target native-exe; expected {} and {}",
            include_dir.display(),
            source_dir.display()
        ));
    }
    let build_root = repo.join(".caap_build").join("csys");
    let build_hash = csys_build_hash(&repo, &include_dir, &source_dir, &clang, &ar)?;
    let build_dir = build_root.join(build_hash);
    let static_library = build_dir.join("libcaap_sys_runtime.a");
    if static_library.is_file() {
        return Ok(static_library);
    }

    fs::create_dir_all(&build_dir).map_err(|error| {
        format!(
            "failed to create C sys runtime build dir {}: {error}",
            build_dir.display()
        )
    })?;
    let object_dir = build_dir.join("obj");
    fs::create_dir_all(&object_dir).map_err(|error| {
        format!(
            "failed to create C sys runtime object dir {}: {error}",
            object_dir.display()
        )
    })?;

    let mut sources = fs::read_dir(&source_dir)
        .map_err(|error| format!("failed to read {}: {error}", source_dir.display()))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to read {}: {error}", source_dir.display()))?;
    sources.retain(|path| path.extension().is_some_and(|extension| extension == "c"));
    sources.sort();

    let mut objects = Vec::new();
    for source in sources {
        let object = object_dir.join(format!(
            "{}.o",
            source
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or_else(|| format!("invalid C source file name {}", source.display()))?
        ));
        run_command(
            Command::new(&clang)
                .arg("-c")
                .arg("-fPIC")
                .arg("-std=c11")
                .arg("-O2")
                .arg("-I")
                .arg(&include_dir)
                .arg("-I")
                .arg(&source_dir)
                .arg("-o")
                .arg(&object)
                .arg(&source),
            "build C sys runtime object",
        )?;
        objects.push(object);
    }

    let mut ar_command = Command::new(&ar);
    ar_command.arg("rcs").arg(&static_library);
    for object in &objects {
        ar_command.arg(object);
    }
    run_command(&mut ar_command, "archive C sys runtime")?;
    Ok(static_library)
}

fn csys_build_hash(
    repo: &Path,
    include_dir: &Path,
    source_dir: &Path,
    clang: &Path,
    ar: &Path,
) -> Result<String, String> {
    let mut hasher = DefaultHasher::new();
    std::env::consts::OS.hash(&mut hasher);
    std::env::consts::ARCH.hash(&mut hasher);
    clang.display().to_string().hash(&mut hasher);
    ar.display().to_string().hash(&mut hasher);
    let mut inputs = Vec::new();
    collect_files(include_dir, &mut inputs)?;
    collect_files(source_dir, &mut inputs)?;
    inputs.sort();
    for path in inputs {
        path.strip_prefix(repo)
            .unwrap_or(&path)
            .display()
            .to_string()
            .hash(&mut hasher);
        fs::read(&path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?
            .hash(&mut hasher);
    }
    Ok(format!("{:016x}", hasher.finish()))
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in
        fs::read_dir(root).map_err(|error| format!("failed to read {}: {error}", root.display()))?
    {
        let path = entry
            .map_err(|error| format!("failed to read {}: {error}", root.display()))?
            .path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn repo_root() -> Result<PathBuf, String> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .canonicalize()
        .map_err(|error| format!("failed to resolve repository root: {error}"))
}

fn find_executable(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let candidate = dir.join(format!("{name}.exe"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn run_command(command: &mut Command, action: &str) -> Result<(), String> {
    let debug_command = format!("{command:?}");
    let output = command
        .output()
        .map_err(|error| format!("failed to {action}: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut message = format!("{action} failed with status {}", output.status);
    if !stderr.trim().is_empty() {
        message.push_str(&format!(": {}", stderr.trim()));
    } else if !stdout.trim().is_empty() {
        message.push_str(&format!(": {}", stdout.trim()));
    }
    message.push_str(&format!(" (command: {debug_command})"));
    Err(message)
}

fn read_input(path: &str) -> Result<String, String> {
    if path == "-" {
        let mut source = String::new();
        io::stdin()
            .read_to_string(&mut source)
            .map_err(|error| format!("failed to read stdin: {error}"))?;
        return Ok(source);
    }
    fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", Path::new(path).display()))
}

fn write_output(
    stdout: &mut dyn Write,
    output: Option<&str>,
    text: &str,
    stderr: &mut dyn Write,
) -> i32 {
    if let Some(path) = output {
        match fs::write(path, text) {
            Ok(()) => 0,
            Err(error) => {
                let _ = writeln!(
                    stderr,
                    "failed to write {}: {error}",
                    Path::new(path).display()
                );
                EXIT_USAGE
            }
        }
    } else {
        let _ = write!(stdout, "{text}");
        0
    }
}

fn write_help(stdout: &mut dyn Write) {
    let _ = writeln!(
        stdout,
        "CAAP Rust CLI\n\ncommands:\n  run [--bootstrap PATH] [--internal-capability NAME] [--root ROOT] [-o PATH] [--trace-compile] INPUT\n  check INPUT...\n  lint INPUT...\n  format [--check|--write|--stdout] INPUT...\n  ast-json to-json|to-caap|roundtrip-check INPUT\n  compile [--bootstrap PATH] [--internal-capability NAME] [--root ROOT] [--target check] [--trace-compile] INPUT\n  compile --bootstrap PATH [--root ROOT] --target native-exe [--entry EMITTER] -o PATH INPUT\n  compile --syntax-only INPUT\n  llvm-ir --bootstrap PATH [--internal-capability NAME] [--root ROOT] [--entry EMITTER] [-o PATH] [--trace-compile] INPUT"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("caap-rust-cli-{}-{name}", std::process::id()))
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
    fn cli_run_evaluates_source_file() {
        let path = temp_path("run.caap");
        fs::write(&path, "(int-add 20 22)").unwrap();

        let (code, stdout, stderr) = run(&["run", &path]);
        fs::remove_file(&path).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "42\n");
    }

    #[test]
    fn cli_check_and_compile_syntax_only_validate_source() {
        let path = temp_path("check.caap");
        fs::write(&path, "(int-add 1 2)").unwrap();

        let (check_code, _, check_stderr) = run(&["check", &path]);
        let (compile_code, _, compile_stderr) = run(&["compile", "--syntax-only", &path]);
        fs::remove_file(&path).ok();

        assert_eq!(check_code, 0, "{check_stderr}");
        assert_eq!(compile_code, 0, "{compile_stderr}");
    }

    #[test]
    fn cli_run_with_bootstrap_uses_stdlib_module_run_source() {
        let path = temp_path("bootstrapped-run.caap");
        fs::write(
            &path,
            r#"
              (module "demo.cli_run")
              (int-add 40 2)
            "#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();

        let (code, stdout, stderr) = run(&["run", "--bootstrap", &bootstrap, &path]);
        fs::remove_file(&path).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "42\n");
    }

    #[test]
    fn cli_compile_with_bootstrap_uses_stdlib_module_check_source() {
        let path = temp_path("bootstrapped-check.caap");
        fs::write(
            &path,
            r#"
              (module "demo.cli_check")
              (int-add 1 2)
            "#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();

        let (code, stdout, stderr) = run(&["compile", "--bootstrap", &bootstrap, &path]);
        fs::remove_file(&path).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "");
    }

    #[test]
    fn cli_compile_with_bootstrap_renders_provider_diagnostics() {
        let path = temp_path("bootstrapped-diagnostic-source.caap");
        let provider = temp_path("bootstrapped-diagnostic-provider.caap");
        fs::write(
            &path,
            r#"
              (module "demo.cli_diagnostic_source")
              (int-add 1 2)
            "#,
        )
        .unwrap();
        fs::write(
            &provider,
            r#"
              (ctfe-compiler-provider-register
                compiler
                "demo.cli-diagnostic-provider"
                "validate_graph"
                (lambda (ctx root)
                  (ctfe-provider-diagnostics-warning
                    ctx
                    root
                    "CLI rendered provider diagnostic"
                    "demo.cli.warning"))
                (list-of "validate_graph")
                (list-of "emit-diagnostics")
                (map-of
                  "reads" (list-of "ir")
                  "writes" (list-of "diagnostics")
                  "cache_scope" "none"
                  "resume_policy" "safe"
                  "input_schema" null))
            "#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();

        let (code, stdout, stderr) = run(&[
            "compile",
            "--bootstrap",
            &bootstrap,
            "--bootstrap",
            &provider,
            &path,
        ]);
        fs::remove_file(&path).ok();
        fs::remove_file(&provider).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "");
        assert!(stderr.contains("warning[demo.cli.warning]"), "{stderr}");
        assert!(
            stderr.contains("CLI rendered provider diagnostic"),
            "{stderr}"
        );
    }

    #[test]
    fn cli_compile_purity_demo_reports_imported_host_effects() {
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();
        let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../example/purity_pass_demo/demo.caap")
            .display()
            .to_string();

        let (code, stdout, stderr) = run(&["compile", "--bootstrap", &bootstrap, &demo]);

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "");
        assert!(
            stderr.contains("warning[example.purity.impure_call_in_function]"),
            "{stderr}"
        );
        assert!(stderr.contains("impure-print"), "{stderr}");
        assert!(stderr.contains("println"), "{stderr}");
    }

    #[test]
    fn cli_compile_with_bootstrap_loads_imported_system_modules() {
        let path = temp_path("bootstrapped-sys-io.caap");
        fs::write(
            &path,
            r#"
              (module "demo.cli_sys_io")
              (import-symbols "sys.io" "println")
              (bind ((main (lambda () 0))) (main))
            "#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();

        let (code, stdout, stderr) = run(&["compile", "--bootstrap", &bootstrap, &path]);
        fs::remove_file(&path).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "");
    }

    #[test]
    fn cli_compile_with_bootstrap_writes_json_trace() {
        let path = temp_path("bootstrapped-trace.caap");
        let trace = temp_path("bootstrapped-trace.json");
        fs::write(
            &path,
            r#"
              (module "demo.cli_trace")
              (int-add 1 2)
            "#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();

        let (code, stdout, stderr) = run(&[
            "compile",
            "--bootstrap",
            &bootstrap,
            "--trace-compile",
            "--trace-format",
            "json",
            "--trace-output",
            &trace,
            &path,
        ]);
        let trace_text = fs::read_to_string(&trace).unwrap_or_default();
        fs::remove_file(&path).ok();
        fs::remove_file(&trace).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "");
        assert!(stderr.contains("compile trace written to"));
        assert!(trace_text.contains(r#""kind": "bootstrap.execute""#));
        assert!(trace_text.contains(r#""kind": "source.template.load""#));
        assert!(trace_text.contains(r#""key": "elapsed_ms""#));
        assert!(trace_text.contains(r#""key": "cache_hit""#));
        assert!(trace_text.contains(r#""kind": "cli.command.finish""#));
    }

    #[test]
    fn cli_compile_with_bootstrap_fails_on_stdlib_type_checker_error() {
        let path = temp_path("bootstrapped-type-error.caap");
        fs::write(
            &path,
            r#"
              (module "demo.cli_type_error")
              (import-symbols "stdlib.types.checker" "stdlib.types.checker")
              (int-add 1 "x")
            "#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();

        let (code, stdout, stderr) = run(&["compile", "--bootstrap", &bootstrap, &path]);
        fs::remove_file(&path).ok();

        assert_eq!(code, EXIT_COMPILE);
        assert_eq!(stdout, "");
        assert!(stderr.contains("CAAP compilation failed"));
        assert!(stderr.contains("stdlib.types.type_mismatch"));
        assert!(stderr.contains("type mismatch in numeric operand"));
    }

    #[test]
    fn cli_run_with_bootstrap_root_uses_stdlib_module_run_from_root() {
        let root = std::env::temp_dir().join(format!(
            "caap-rust-cli-root-run-{}-{}",
            std::process::id(),
            line!()
        ));
        fs::create_dir_all(&root).unwrap();
        let entry = root.join("entry.caap");
        fs::write(
            &entry,
            r#"
              (module "demo.cli_root_run")
              (int-add 21 21)
            "#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();
        let root_text = root.display().to_string();

        let (code, stdout, stderr) = run(&[
            "run",
            "--bootstrap",
            &bootstrap,
            "--root",
            &root_text,
            "demo.cli_root_run",
        ]);
        fs::remove_dir_all(&root).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "42\n");
    }

    #[test]
    fn cli_compile_with_bootstrap_root_uses_stdlib_module_check_root() {
        let root = std::env::temp_dir().join(format!(
            "caap-rust-cli-root-check-{}-{}",
            std::process::id(),
            line!()
        ));
        fs::create_dir_all(&root).unwrap();
        let entry = root.join("entry.caap");
        fs::write(
            &entry,
            r#"
              (module "demo.cli_root_check")
              (int-add 1 2)
            "#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();
        let root_text = root.display().to_string();

        let (code, stdout, stderr) = run(&[
            "compile",
            "--bootstrap",
            &bootstrap,
            "--root",
            &root_text,
            "--target",
            "check",
            "demo.cli_root_check",
        ]);
        fs::remove_dir_all(&root).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "");
    }

    #[test]
    fn cli_llvm_ir_with_bootstrap_uses_stdlib_module_emit_source_llvm() {
        let path = temp_path("llvm-source.caap");
        let emitter = temp_path("llvm-source-emitter.caap");
        fs::write(
            &path,
            r#"
              (module "demo.cli_llvm_source")
              (int-add 1 2)
            "#,
        )
        .unwrap();
        fs::write(
            &emitter,
            r#"(ctfe-compiler-register-value compiler "demo.emit" (lambda (unit) (map-of "text" "fake-ir" "diagnostics" (list-of))))"#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();

        let (code, stdout, stderr) = run(&[
            "llvm-ir",
            "--bootstrap",
            &bootstrap,
            "--bootstrap",
            &emitter,
            "--entry",
            "demo.emit",
            &path,
        ]);
        fs::remove_file(&path).ok();
        fs::remove_file(&emitter).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "fake-ir");
    }

    #[test]
    fn cli_llvm_ir_rejects_non_string_emitter_text() {
        let path = temp_path("llvm-source-bad-text.caap");
        let emitter = temp_path("llvm-source-bad-text-emitter.caap");
        fs::write(
            &path,
            r#"
              (module "demo.cli_llvm_source_bad_text")
              (int-add 1 2)
            "#,
        )
        .unwrap();
        fs::write(
            &emitter,
            r#"(ctfe-compiler-register-value compiler "demo.emit.bad-text" (lambda (unit) (map-of "text" 42 "diagnostics" (list-of))))"#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();

        let (code, stdout, stderr) = run(&[
            "llvm-ir",
            "--bootstrap",
            &bootstrap,
            "--bootstrap",
            &emitter,
            "--entry",
            "demo.emit.bad-text",
            &path,
        ]);
        fs::remove_file(&path).ok();
        fs::remove_file(&emitter).ok();

        assert_eq!(code, EXIT_COMPILE);
        assert_eq!(stdout, "");
        assert!(stderr.contains("llvm-ir emitter text field must be string or null"));
    }

    #[test]
    fn cli_llvm_ir_passes_internal_capabilities_to_bootstraps() {
        let path = temp_path("llvm-cap-source.caap");
        let emitter = temp_path("llvm-cap-emitter.caap");
        fs::write(
            &path,
            r#"
              (module "demo.cli_llvm_cap_source")
              (int-add 1 2)
            "#,
        )
        .unwrap();
        fs::write(
            &emitter,
            r#"
              (ctfe-compiler-register-value
                compiler
                "demo.cap-count"
                (size (ctfe-compiler-current-bootstrap-capabilities compiler)))
              (ctfe-compiler-register-value
                compiler
                "demo.cap-emit"
                (lambda (unit)
                  (map-of
                    "text"
                      (int-to-string (ctfe-compiler-lookup-value compiler "demo.cap-count"))
                    "diagnostics"
                      (list-of))))
            "#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();

        let (code, stdout, stderr) = run(&[
            "llvm-ir",
            "--bootstrap",
            &bootstrap,
            "--bootstrap",
            &emitter,
            "--internal-capability",
            "host_services",
            "--entry",
            "demo.cap-emit",
            &path,
        ]);
        fs::remove_file(&path).ok();
        fs::remove_file(&emitter).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "1");
    }

    #[test]
    fn cli_llvm_ir_with_bootstrap_root_uses_stdlib_module_emit_root_llvm() {
        let root = std::env::temp_dir().join(format!(
            "caap-rust-cli-root-llvm-{}-{}",
            std::process::id(),
            line!()
        ));
        fs::create_dir_all(&root).unwrap();
        let entry = root.join("entry.caap");
        let emitter = temp_path("llvm-root-emitter.caap");
        let output = temp_path("llvm-root-output.ll");
        fs::write(
            &entry,
            r#"
              (module "demo.cli_root_llvm")
              (int-add 1 2)
            "#,
        )
        .unwrap();
        fs::write(
            &emitter,
            r#"(ctfe-compiler-register-value compiler "demo.emit" (lambda (unit) (map-of "text" "root-ir" "diagnostics" (list-of))))"#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();
        let root_text = root.display().to_string();

        let (code, stdout, stderr) = run(&[
            "llvm-ir",
            "--bootstrap",
            &bootstrap,
            "--bootstrap",
            &emitter,
            "--root",
            &root_text,
            "--entry",
            "demo.emit",
            "-o",
            &output,
            "demo.cli_root_llvm",
        ]);
        let output_text = fs::read_to_string(&output).unwrap_or_default();
        fs::remove_dir_all(&root).ok();
        fs::remove_file(&emitter).ok();
        fs::remove_file(&output).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "");
        assert_eq!(output_text, "root-ir");
    }

    #[test]
    fn cli_compile_native_exe_with_bootstrap_builds_emitted_llvm_ir() {
        if find_executable("clang").is_none() || find_executable("ar").is_none() {
            eprintln!("skipping native executable CLI test because clang or ar is unavailable");
            return;
        }
        let runtime_root = repo_root()
            .expect("repository root should resolve")
            .join("runtime")
            .join("csys");
        if !runtime_root.join("include").is_dir() || !runtime_root.join("src").is_dir() {
            eprintln!(
                "skipping native executable CLI test because runtime/csys sources are unavailable"
            );
            return;
        }

        let path = temp_path("native-source.caap");
        let emitter = temp_path("native-emitter.caap");
        let output = temp_path("native-output");
        fs::write(
            &path,
            r#"
              (module "demo.cli_native_source")
              (int-add 1 2)
            "#,
        )
        .unwrap();
        fs::write(
            &emitter,
            r#"(ctfe-compiler-register-value compiler "demo.native.emit" (lambda (unit) (map-of "text" "; ModuleID = 'caap-test'\ndefine i32 @main() {\nentry:\n  ret i32 0\n}\n" "diagnostics" (list-of))))"#,
        )
        .unwrap();
        let bootstrap = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../stdlib/bootstrap.caap")
            .display()
            .to_string();

        let (code, stdout, stderr) = run(&[
            "compile",
            "--bootstrap",
            &bootstrap,
            "--bootstrap",
            &emitter,
            "--target",
            "native-exe",
            "--entry",
            "demo.native.emit",
            "-o",
            &output,
            &path,
        ]);
        let output_metadata = fs::metadata(&output).ok();
        fs::remove_file(&path).ok();
        fs::remove_file(&emitter).ok();
        fs::remove_file(&output).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(stdout, "");
        assert!(
            output_metadata.is_some_and(|metadata| metadata.len() > 0),
            "native executable was not created"
        );
    }

    #[test]
    fn cli_format_supports_stdout_and_check() {
        let path = temp_path("format.caap");
        fs::write(&path, "( int-add 1 2 )").unwrap();

        let (stdout_code, stdout, stdout_stderr) = run(&["format", "--stdout", &path]);
        let (check_code, _, check_stderr) = run(&["format", "--check", &path]);
        fs::remove_file(&path).ok();

        assert_eq!(stdout_code, 0, "{stdout_stderr}");
        assert_eq!(stdout, "(int-add 1 2)");
        assert_eq!(check_code, EXIT_COMPILE);
        assert!(check_stderr.contains("would reformat"));
    }

    #[test]
    fn cli_ast_json_roundtrips_surface_model() {
        let path = temp_path("ast.caap");
        fs::write(&path, "(int-add 1)").unwrap();

        let (code, stdout, stderr) = run(&["ast-json", "to-json", &path]);
        fs::write(&path, stdout).unwrap();
        let (to_caap_code, caap_stdout, caap_stderr) = run(&["ast-json", "to-caap", &path]);
        fs::remove_file(&path).ok();

        assert_eq!(code, 0, "{stderr}");
        assert_eq!(to_caap_code, 0, "{caap_stderr}");
        assert_eq!(caap_stdout, "(int-add 1)\n");
    }
}
