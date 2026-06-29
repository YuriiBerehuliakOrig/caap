use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{ErrorKind, Read, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use crate::ffi_value::{SysArgs, SysError, SysResult, SysValue};

// ── Handle table ──────────────────────────────────────────────────────────────

const MAX_PROC_HANDLES: usize = 1024;

/// Spawned-child handle table for one runtime session. Owned explicitly by the
/// caller and passed into every process operation; see [`crate::fs::FsState`]
/// for the rationale behind state injection.
pub struct ProcState {
    handles: BTreeMap<i64, ChildProcess>,
}

struct ChildProcess {
    child: Child,
    timeout_ms: Option<u64>,
    deadline: Option<Instant>,
}

impl Default for ProcState {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcState {
    pub fn new() -> Self {
        Self {
            handles: BTreeMap::new(),
        }
    }

    fn alloc(
        &mut self,
        child: Child,
        timeout_ms: Option<u64>,
        deadline: Option<Instant>,
    ) -> Result<i64, SysError> {
        if self.handles.len() >= MAX_PROC_HANDLES {
            return Err(SysError::resource_exhausted(
                "proc: too many open process handles (limit 1024)",
            ));
        }
        let h = (1..=(MAX_PROC_HANDLES as i64))
            .find(|id| !self.handles.contains_key(id))
            .ok_or_else(|| "proc: process handle allocation failed".to_string())?;
        self.handles.insert(
            h,
            ChildProcess {
                child,
                timeout_ms,
                deadline,
            },
        );
        Ok(h)
    }

    fn reject_elapsed_deadline(&mut self, handle: i64, context: &str) -> Result<(), SysError> {
        let process = self
            .handles
            .get(&handle)
            .ok_or_else(|| format!("{context}: unknown handle {handle}"))?;
        if let Some(error) =
            process_timeout_error_if_elapsed(process.timeout_ms, process.deadline, context)
        {
            let mut process = self
                .handles
                .remove(&handle)
                .ok_or_else(|| format!("{context}: unknown handle {handle}"))?;
            let _ = process.child.kill();
            let _ = process.child.wait();
            return Err(error);
        }
        Ok(())
    }

    fn read_stdout(&mut self, handle: i64, context: &str) -> Result<String, SysError> {
        self.read_pipe(handle, context, ProcPipeKind::Stdout)
    }

    fn read_stderr(&mut self, handle: i64, context: &str) -> Result<String, SysError> {
        self.read_pipe(handle, context, ProcPipeKind::Stderr)
    }

    fn read_pipe(
        &mut self,
        handle: i64,
        context: &str,
        pipe_kind: ProcPipeKind,
    ) -> Result<String, SysError> {
        self.reject_elapsed_deadline(handle, context)?;
        let result = {
            let process = self
                .handles
                .get_mut(&handle)
                .ok_or_else(|| format!("{context}: unknown handle {handle}"))?;
            match pipe_kind {
                ProcPipeKind::Stdout => {
                    let stdout = process
                        .child
                        .stdout
                        .as_mut()
                        .ok_or(format!("{context}: stdout not captured"))?;
                    read_live_pipe_until(
                        stdout,
                        process.timeout_ms,
                        process.deadline,
                        context,
                        "stdout",
                    )
                }
                ProcPipeKind::Stderr => {
                    let stderr = process
                        .child
                        .stderr
                        .as_mut()
                        .ok_or(format!("{context}: stderr not captured"))?;
                    read_live_pipe_until(
                        stderr,
                        process.timeout_ms,
                        process.deadline,
                        context,
                        "stderr",
                    )
                }
            }
        };
        match result {
            Ok(text) => Ok(text),
            Err(ProcPipeReadError::Runtime(error)) => Err(error),
            Err(ProcPipeReadError::Timeout(error)) => {
                let mut process = self
                    .handles
                    .remove(&handle)
                    .ok_or_else(|| format!("{context}: unknown handle {handle}"))?;
                let _ = process.child.kill();
                let _ = process.child.wait();
                Err(error)
            }
        }
    }
}

impl Drop for ProcState {
    /// Reap any child still live when the session ends. `std::process::Child`'s
    /// own `Drop` neither kills nor waits, so without this a session that ends
    /// with un-waited children (the registry dropped, the thread-local cleared)
    /// would leak them as zombies on Unix. Mirror the timeout-cleanup path —
    /// kill, then wait — so a long-running child can't block the drop forever.
    fn drop(&mut self) {
        for process in self.handles.values_mut() {
            let _ = process.child.kill();
            let _ = process.child.wait();
        }
    }
}

#[derive(Clone, Copy)]
enum ProcPipeKind {
    Stdout,
    Stderr,
}

// ── Public invoke ─────────────────────────────────────────────────────────────

/// Process-wide override for what `process.args` reports.
///
/// A natively compiled CAAP binary sees its own OS argv, so `args` returns
/// `[program, arg…]`. Under the interpreter the OS argv belongs to the host
/// (`caap bootstrap.caap program.caap arg…`), so the launcher sets this
/// override to the program-relative view before evaluation — the same `args`
/// call then behaves identically in both worlds.
static ARGS_OVERRIDE: std::sync::RwLock<Option<Vec<String>>> = std::sync::RwLock::new(None);

pub fn set_args_override(args: Vec<String>) {
    *ARGS_OVERRIDE
        .write()
        .expect("process args override lock poisoned") = Some(args);
}

fn args_override() -> Option<Vec<String>> {
    ARGS_OVERRIDE
        .read()
        .expect("process args override lock poisoned")
        .clone()
}

pub fn invoke(state: &mut ProcState, name: &str, args: SysArgs) -> SysResult {
    tracing::debug!(name, "proc invoke");
    match name {
        "id" => Ok(SysValue::Int(std::process::id() as i64)),
        "args" => match args_override() {
            Some(overridden) => Ok(SysValue::List(
                overridden.into_iter().map(SysValue::Str).collect(),
            )),
            None => process_args_value(std::env::args_os()),
        },
        "run" => proc_run(args),
        "spawn" => proc_spawn(state, args),
        "wait" => proc_wait(state, args),
        "wait_result" => proc_wait_result(state, args),
        "kill" => proc_kill(state, args),
        "write_stdin" => proc_write_stdin(state, args),
        "close_stdin" => proc_close_stdin(state, args),
        "read_stdout" => proc_read_stdout(state, args),
        "read_stderr" => proc_read_stderr(state, args),
        _ => Err(format!("process: unknown export '{name}'").into()),
    }
}

fn process_args_value(args: impl IntoIterator<Item = OsString>) -> SysResult {
    args.into_iter()
        .map(|arg| {
            arg.into_string().map(SysValue::Str).map_err(|_| {
                SysError::invalid_argument("process.args: argument is not valid UTF-8")
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map(SysValue::List)
}

fn proc_run(args: SysArgs) -> SysResult {
    let spec = parse_spec(args.require_map(0, "process.run")?)?;
    let deadline = process_timeout_deadline(spec.timeout_ms, "process.run")?;
    let mut cmd = build_command(&spec);
    configure_stdio(&mut cmd, &spec, true, true, false);
    let child = cmd
        .spawn()
        .map_err(|e| SysError::from_io("process.run", e))?;
    wait_child_value_until(child, "process.run", spec.timeout_ms, deadline)
}

fn proc_spawn(state: &mut ProcState, args: SysArgs) -> SysResult {
    let spec = parse_spec(args.require_map(0, "process.spawn")?)?;
    let deadline = process_timeout_deadline(spec.timeout_ms, "process.spawn")?;
    let mut cmd = build_command(&spec);
    configure_stdio(&mut cmd, &spec, false, false, true);
    let child = cmd
        .spawn()
        .map_err(|e| SysError::from_io("process.spawn", e))?;
    let handle = state.alloc(child, spec.timeout_ms, deadline)?;
    Ok(SysValue::Int(handle))
}

fn proc_wait(state: &mut ProcState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "process.wait")?;
    let process = state
        .handles
        .remove(&handle)
        .ok_or_else(|| format!("process.wait: unknown handle {handle}"))?;
    wait_child_value_until(
        process.child,
        "process.wait",
        process.timeout_ms,
        process.deadline,
    )
}

fn proc_wait_result(state: &mut ProcState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "process.wait_result")?;
    // reject_elapsed_deadline kills + removes the handle and returns Err if the
    // deadline has passed, so reaching the code below guarantees it has not.
    state.reject_elapsed_deadline(handle, "process.wait_result")?;
    match state
        .handles
        .get_mut(&handle)
        .ok_or_else(|| format!("process.wait_result: unknown handle {handle}"))?
        .child
        .try_wait()
        .map_err(|e| SysError::from_io("process.wait_result", e))?
    {
        None => Ok(SysValue::Null),
        Some(status) => {
            let mut process = state
                .handles
                .remove(&handle)
                .ok_or_else(|| format!("process.wait_result: unknown handle {handle}"))?;
            let stdout = read_pipe(process.child.stdout.as_mut(), "process.wait_result")?;
            let stderr = read_pipe(process.child.stderr.as_mut(), "process.wait_result")?;
            completed_value(status, stdout, stderr)
        }
    }
}

fn proc_kill(state: &mut ProcState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "process.kill")?;
    let process = state
        .handles
        .get_mut(&handle)
        .ok_or_else(|| format!("process.kill: unknown handle {handle}"))?;
    process
        .child
        .kill()
        .map_err(|e| SysError::from_io("process.kill", e))?;
    Ok(SysValue::Null)
}

fn proc_write_stdin(state: &mut ProcState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "process.write_stdin")?;
    let text = args.require_str(1, "process.write_stdin")?;
    state.reject_elapsed_deadline(handle, "process.write_stdin")?;
    let process = state
        .handles
        .get_mut(&handle)
        .ok_or_else(|| format!("process.write_stdin: unknown handle {handle}"))?;
    let stdin = process
        .child
        .stdin
        .as_mut()
        .ok_or("process.write_stdin: stdin not captured")?;
    stdin
        .write_all(text.as_bytes())
        .map_err(|e| SysError::from_io("process.write_stdin", e))?;
    stdin
        .flush()
        .map_err(|e| SysError::from_io("process.write_stdin", e))?;
    Ok(SysValue::Null)
}

fn proc_close_stdin(state: &mut ProcState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "process.close_stdin")?;
    state.reject_elapsed_deadline(handle, "process.close_stdin")?;
    let process = state
        .handles
        .get_mut(&handle)
        .ok_or_else(|| format!("process.close_stdin: unknown handle {handle}"))?;
    process.child.stdin.take();
    Ok(SysValue::Null)
}

fn proc_read_stdout(state: &mut ProcState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "process.read_stdout")?;
    state
        .read_stdout(handle, "process.read_stdout")
        .map(SysValue::Str)
}

fn proc_read_stderr(state: &mut ProcState, args: SysArgs) -> SysResult {
    let handle = args.require_int(0, "process.read_stderr")?;
    state
        .read_stderr(handle, "process.read_stderr")
        .map(SysValue::Str)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

#[derive(Debug)]
struct ProcSpec {
    argv: Vec<String>,
    cwd: Option<String>,
    env: BTreeMap<String, String>,
    capture_stdout: Option<bool>,
    capture_stderr: Option<bool>,
    inherit_stdin: bool,
    inherit_stdout: bool,
    inherit_stderr: bool,
    timeout_ms: Option<u64>,
    limits: ProcessResourceLimits,
}

#[derive(Clone, Copy, Debug, Default)]
struct ProcessResourceLimits {
    cpu_seconds: Option<u64>,
    memory_bytes: Option<u64>,
    open_files: Option<u64>,
}

fn parse_spec(spec: crate::ffi_value::SysMap) -> Result<ProcSpec, SysError> {
    let argv_list = match spec.0.get("argv") {
        Some(SysValue::List(v)) => v.clone(),
        Some(_) => return Err("process: argv must be a list".into()),
        None => return Err("process: missing argv".into()),
    };
    if argv_list.is_empty() {
        return Err("process: argv must be non-empty".into());
    }
    let argv: Result<Vec<String>, SysError> = argv_list
        .into_iter()
        .map(|v| match v {
            SysValue::Str(s) => Ok(s),
            _ => Err("process: argv elements must be strings".into()),
        })
        .collect();
    let argv = argv?;

    let cwd = match spec.0.get("cwd") {
        Some(SysValue::Str(s)) => Some(s.clone()),
        Some(SysValue::Null) | None => None,
        Some(_) => return Err("process: cwd must be a string".into()),
    };

    let env = match spec.0.get("env") {
        Some(SysValue::Map(m)) => {
            let mut map = BTreeMap::new();
            for (k, v) in m {
                match v {
                    SysValue::Str(s) => {
                        map.insert(k.clone(), s.clone());
                    }
                    _ => return Err("process: env values must be strings".into()),
                }
            }
            map
        }
        Some(SysValue::Null) | None => BTreeMap::new(),
        Some(_) => return Err("process: env must be a map".into()),
    };

    let limits = process_resource_limits(&spec)?;
    validate_process_resource_limits_supported(&limits)?;

    Ok(ProcSpec {
        argv,
        cwd,
        env,
        capture_stdout: optional_bool_field(&spec, "capture_stdout")?,
        capture_stderr: optional_bool_field(&spec, "capture_stderr")?,
        inherit_stdin: optional_bool(&spec, "inherit_stdin")?,
        inherit_stdout: optional_bool(&spec, "inherit_stdout")?,
        inherit_stderr: optional_bool(&spec, "inherit_stderr")?,
        timeout_ms: optional_timeout_ms(&spec)?,
        limits,
    })
}

fn optional_bool(spec: &crate::ffi_value::SysMap, key: &str) -> Result<bool, SysError> {
    match spec.0.get(key) {
        Some(SysValue::Bool(value)) => Ok(*value),
        Some(SysValue::Null) | None => Ok(false),
        Some(_) => Err(SysError::invalid_argument(format!(
            "process: {key} must be a bool"
        ))),
    }
}

fn optional_timeout_ms(spec: &crate::ffi_value::SysMap) -> Result<Option<u64>, SysError> {
    match spec.0.get("timeout_ms") {
        Some(SysValue::Int(value)) if *value >= 0 => Ok(Some(*value as u64)),
        Some(SysValue::Null) | None => Ok(None),
        Some(SysValue::Int(_)) => Err("process: timeout_ms must be a non-negative int".into()),
        Some(_) => Err("process: timeout_ms must be an int".into()),
    }
}

fn optional_bool_field(
    spec: &crate::ffi_value::SysMap,
    key: &str,
) -> Result<Option<bool>, SysError> {
    match spec.0.get(key) {
        Some(SysValue::Bool(value)) => Ok(Some(*value)),
        Some(SysValue::Null) | None => Ok(None),
        Some(_) => Err(SysError::invalid_argument(format!(
            "process: {key} must be a bool"
        ))),
    }
}

fn build_command(spec: &ProcSpec) -> Command {
    let mut cmd = Command::new(&spec.argv[0]);
    cmd.args(spec.argv.iter().skip(1));
    if let Some(cwd) = &spec.cwd {
        cmd.current_dir(cwd);
    }
    cmd.envs(spec.env.iter());
    configure_process_resource_limits(&mut cmd, spec.limits);
    cmd
}

fn process_resource_limits(
    spec: &crate::ffi_value::SysMap,
) -> Result<ProcessResourceLimits, SysError> {
    let Some(value) = spec.0.get("limits") else {
        return Ok(ProcessResourceLimits::default());
    };
    let SysValue::Map(limits) = value else {
        if matches!(value, SysValue::Null) {
            return Ok(ProcessResourceLimits::default());
        }
        return Err("process: limits must be a map".into());
    };
    Ok(ProcessResourceLimits {
        cpu_seconds: optional_positive_limit(limits, "cpu_seconds")?,
        memory_bytes: optional_positive_limit(limits, "memory_bytes")?,
        open_files: optional_positive_limit(limits, "open_files")?,
    })
}

fn optional_positive_limit(
    limits: &HashMap<String, SysValue>,
    key: &str,
) -> Result<Option<u64>, SysError> {
    match limits.get(key) {
        Some(SysValue::Int(value)) if *value > 0 => u64::try_from(*value).map(Some).map_err(|_| {
            SysError::invalid_argument(format!("process: limits.{key} exceeds range"))
        }),
        Some(SysValue::Int(_)) => Err(SysError::invalid_argument(format!(
            "process: limits.{key} must be a positive int"
        ))),
        Some(SysValue::Null) | None => Ok(None),
        Some(_) => Err(SysError::invalid_argument(format!(
            "process: limits.{key} must be an int"
        ))),
    }
}

#[cfg(unix)]
fn validate_process_resource_limits_supported(
    _limits: &ProcessResourceLimits,
) -> Result<(), SysError> {
    Ok(())
}

#[cfg(not(unix))]
fn validate_process_resource_limits_supported(
    limits: &ProcessResourceLimits,
) -> Result<(), SysError> {
    if limits.cpu_seconds.is_some() || limits.memory_bytes.is_some() || limits.open_files.is_some()
    {
        return Err("process: resource limits are not supported on this platform".into());
    }
    Ok(())
}

fn configure_process_resource_limits(command: &mut Command, limits: ProcessResourceLimits) {
    #[cfg(unix)]
    if limits.cpu_seconds.is_some() || limits.memory_bytes.is_some() || limits.open_files.is_some()
    {
        // SAFETY: `pre_exec` closure runs in the child process after `fork()` before `exec()`;
        // `apply_process_resource_limits` only calls async-signal-safe libc functions.
        unsafe {
            command.pre_exec(move || apply_process_resource_limits(limits));
        }
    }
    #[cfg(not(unix))]
    let _ = (command, limits);
}

#[cfg(unix)]
fn apply_process_resource_limits(limits: ProcessResourceLimits) -> std::io::Result<()> {
    if let Some(value) = limits.cpu_seconds {
        set_process_resource_limit(libc::RLIMIT_CPU, value, "cpu_seconds")?;
    }
    if let Some(value) = limits.memory_bytes {
        set_process_resource_limit(libc::RLIMIT_AS, value, "memory_bytes")?;
    }
    if let Some(value) = limits.open_files {
        set_process_resource_limit(libc::RLIMIT_NOFILE, value, "open_files")?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_process_resource_limit(
    resource: libc::__rlimit_resource_t,
    value: u64,
    label: &str,
) -> std::io::Result<()> {
    let limit = libc::rlim_t::try_from(value).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("process: limits.{label} exceeds platform range"),
        )
    })?;
    let mut current = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: `resource` is a valid `RLIMIT_*` constant; `current` is a local `rlimit` that
    // `getrlimit` will initialize.
    if unsafe { libc::getrlimit(resource, &mut current) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    if limit > current.rlim_max {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("process: limits.{label} exceeds current hard limit"),
        ));
    }
    let next = libc::rlimit {
        rlim_cur: limit,
        rlim_max: current.rlim_max,
    };
    // SAFETY: `resource` is a valid `RLIMIT_*` constant; `next` has `rlim_cur <= rlim_max`.
    if unsafe { libc::setrlimit(resource, &next) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

fn configure_stdio(
    cmd: &mut Command,
    spec: &ProcSpec,
    default_capture_stdout: bool,
    default_capture_stderr: bool,
    pipe_stdin_when_not_inherited: bool,
) {
    cmd.stdin(if spec.inherit_stdin {
        Stdio::inherit()
    } else if pipe_stdin_when_not_inherited {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(
        if spec.inherit_stdout || !spec.capture_stdout.unwrap_or(default_capture_stdout) {
            Stdio::inherit()
        } else {
            Stdio::piped()
        },
    );
    cmd.stderr(
        if spec.inherit_stderr || !spec.capture_stderr.unwrap_or(default_capture_stderr) {
            Stdio::inherit()
        } else {
            Stdio::piped()
        },
    );
}

fn process_timeout_deadline(
    timeout_ms: Option<u64>,
    ctx: &str,
) -> Result<Option<Instant>, SysError> {
    timeout_ms
        .map(|timeout_ms| {
            Instant::now()
                .checked_add(Duration::from_millis(timeout_ms))
                .ok_or_else(|| {
                    SysError::invalid_argument(format!("{ctx}: timeout_ms is too large"))
                })
        })
        .transpose()
}

fn wait_child_value_until(
    mut child: Child,
    ctx: &str,
    timeout_ms: Option<u64>,
    deadline: Option<Instant>,
) -> SysResult {
    if let Some(deadline) = deadline {
        loop {
            if let Some(status) = child.try_wait().map_err(|e| SysError::from_io(ctx, e))? {
                let stdout = read_pipe(child.stdout.as_mut(), ctx)?;
                let stderr = read_pipe(child.stderr.as_mut(), ctx)?;
                return completed_value(status, stdout, stderr);
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(SysError::timed_out(format!(
                    "{ctx}: process timed out after {} ms",
                    timeout_ms.unwrap_or(0)
                )));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    let output = child
        .wait_with_output()
        .map_err(|e| SysError::from_io(ctx, e))?;
    let stdout = decode_process_output(output.stdout, ctx, "stdout")?;
    let stderr = decode_process_output(output.stderr, ctx, "stderr")?;
    completed_value(output.status, stdout, stderr)
}

fn process_timeout_error_if_elapsed(
    timeout_ms: Option<u64>,
    deadline: Option<Instant>,
    ctx: &str,
) -> Option<SysError> {
    deadline.and_then(|deadline| {
        (Instant::now() >= deadline).then(|| {
            SysError::timed_out(format!(
                "{ctx}: process timed out after {} ms",
                timeout_ms.unwrap_or(0)
            ))
        })
    })
}

enum ProcPipeReadError {
    Runtime(SysError),
    Timeout(SysError),
}

fn process_timeout_pipe_error_if_elapsed(
    timeout_ms: Option<u64>,
    deadline: Option<Instant>,
    ctx: &str,
) -> Option<ProcPipeReadError> {
    process_timeout_error_if_elapsed(timeout_ms, deadline, ctx).map(ProcPipeReadError::Timeout)
}

#[cfg(unix)]
fn read_live_pipe_until<T: Read + AsRawFd>(
    pipe: &mut T,
    timeout_ms: Option<u64>,
    deadline: Option<Instant>,
    ctx: &str,
    stream: &str,
) -> Result<String, ProcPipeReadError> {
    if deadline.is_none() {
        let mut text = String::new();
        pipe.read_to_string(&mut text)
            .map_err(|e| ProcPipeReadError::Runtime(SysError::from_io(ctx, e)))?;
        return Ok(text);
    }

    let fd = pipe.as_raw_fd();
    // SAFETY: `fd` is a valid open file descriptor from `pipe.as_raw_fd()`.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(ProcPipeReadError::Runtime(SysError::from_io(
            ctx,
            std::io::Error::last_os_error(),
        )));
    }
    let changed_flags = flags & libc::O_NONBLOCK == 0;
    // SAFETY: `fd` is a valid file descriptor; we set O_NONBLOCK for deadline-aware pipe reads.
    if changed_flags && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(ProcPipeReadError::Runtime(SysError::from_io(
            ctx,
            std::io::Error::last_os_error(),
        )));
    }

    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        if let Some(error) = process_timeout_pipe_error_if_elapsed(timeout_ms, deadline, ctx) {
            // Restore O_NONBLOCK before returning on timeout.
            if changed_flags {
                // SAFETY: restoring original fd flags on timeout; `fd` is still valid.
                let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
            }
            return Err(error);
        }
        match pipe.read(&mut buffer) {
            Ok(0) => {
                if changed_flags {
                    // SAFETY: restoring original fd flags after pipe EOF; `fd` is still valid.
                    let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
                }
                return decode_process_output(bytes, ctx, stream)
                    .map_err(ProcPipeReadError::Runtime);
            }
            Ok(n) => bytes.extend_from_slice(&buffer[..n]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => {
                if changed_flags {
                    // SAFETY: restoring original fd flags on read error; `fd` is still valid.
                    let _ = unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
                }
                return Err(ProcPipeReadError::Runtime(SysError::from_io(ctx, error)));
            }
        }
    }
}

#[cfg(not(unix))]
fn read_live_pipe_until<T: Read>(
    pipe: &mut T,
    timeout_ms: Option<u64>,
    deadline: Option<Instant>,
    ctx: &str,
    stream: &str,
) -> Result<String, ProcPipeReadError> {
    // On non-Unix there is no portable non-blocking pipe API, so individual
    // read() calls may block briefly.  The deadline is checked between calls;
    // a process that writes very slowly may exceed the deadline by at most one
    // read() syscall duration.
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    loop {
        if let Some(error) = process_timeout_pipe_error_if_elapsed(timeout_ms, deadline, ctx) {
            return Err(error);
        }
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => bytes.extend_from_slice(&buffer[..n]),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(ProcPipeReadError::Runtime(SysError::from_io(ctx, e))),
        }
    }
    decode_process_output(bytes, ctx, stream).map_err(ProcPipeReadError::Runtime)
}

fn decode_process_output(bytes: Vec<u8>, ctx: &str, stream: &str) -> Result<String, SysError> {
    String::from_utf8(bytes).map_err(|error| {
        SysError::invalid_argument(format!("{ctx}: {stream} is not valid UTF-8: {error}"))
    })
}

fn completed_value(status: ExitStatus, stdout: String, stderr: String) -> SysResult {
    let code = status.code().unwrap_or_else(|| {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            -status.signal().unwrap_or(1)
        }
        #[cfg(not(unix))]
        {
            1
        }
    });
    let signal = {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            status
                .signal()
                .map(|s| SysValue::Int(s as i64))
                .unwrap_or(SysValue::Null)
        }
        #[cfg(not(unix))]
        {
            SysValue::Null
        }
    };
    let mut m = HashMap::new();
    m.insert("status".into(), SysValue::Int(code as i64));
    m.insert("success".into(), SysValue::Bool(status.success()));
    m.insert("stdout".into(), SysValue::Str(stdout));
    m.insert("stderr".into(), SysValue::Str(stderr));
    m.insert("signal".into(), signal);
    Ok(SysValue::Map(m))
}

fn read_pipe(pipe: Option<&mut impl Read>, ctx: &str) -> Result<String, SysError> {
    let Some(p) = pipe else {
        return Ok(String::new());
    };
    let mut text = String::new();
    p.read_to_string(&mut text)
        .map_err(|e| SysError::from_io(ctx, e))?;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(
        entries: impl IntoIterator<Item = (&'static str, SysValue)>,
    ) -> crate::ffi_value::SysMap {
        crate::ffi_value::SysMap(
            entries
                .into_iter()
                .map(|(key, value)| (key.to_string(), value))
                .collect(),
        )
    }

    #[test]
    fn parse_spec_rejects_malformed_optional_fields() {
        for (field, value, expected) in [
            ("cwd", SysValue::Bool(true), "cwd must be a string"),
            ("env", SysValue::List(Vec::new()), "env must be a map"),
            (
                "capture_stdout",
                SysValue::Str("yes".into()),
                "capture_stdout must be a bool",
            ),
            (
                "inherit_stdin",
                SysValue::Int(1),
                "inherit_stdin must be a bool",
            ),
            (
                "timeout_ms",
                SysValue::Str("10".into()),
                "timeout_ms must be an int",
            ),
            (
                "timeout_ms",
                SysValue::Int(-1),
                "timeout_ms must be a non-negative int",
            ),
            (
                "limits",
                SysValue::Str("tight".into()),
                "limits must be a map",
            ),
        ] {
            let error = match parse_spec(spec([
                ("argv", SysValue::List(vec![SysValue::Str("echo".into())])),
                (field, value),
            ])) {
                Ok(_) => panic!("expected malformed process spec to fail"),
                Err(error) => error,
            };
            assert!(error.contains(expected), "got {error:?}");
        }
    }

    #[test]
    fn parse_spec_accepts_absent_or_null_optional_fields() {
        let parsed = parse_spec(spec([
            ("argv", SysValue::List(vec![SysValue::Str("echo".into())])),
            ("cwd", SysValue::Null),
            ("env", SysValue::Null),
            ("capture_stdout", SysValue::Null),
        ]))
        .unwrap();

        assert_eq!(parsed.cwd, None);
        assert!(parsed.env.is_empty());
        assert_eq!(parsed.capture_stdout, None);
        assert_eq!(parsed.timeout_ms, None);
        assert_eq!(parsed.limits.cpu_seconds, None);
        assert_eq!(parsed.limits.memory_bytes, None);
        assert_eq!(parsed.limits.open_files, None);
    }

    #[test]
    fn parse_spec_accepts_process_resource_limits() {
        let parsed = parse_spec(spec([
            ("argv", SysValue::List(vec![SysValue::Str("echo".into())])),
            (
                "limits",
                SysValue::Map(HashMap::from([
                    ("cpu_seconds".into(), SysValue::Int(2)),
                    ("memory_bytes".into(), SysValue::Int(1024 * 1024)),
                    ("open_files".into(), SysValue::Int(32)),
                ])),
            ),
        ]))
        .unwrap();

        assert_eq!(parsed.limits.cpu_seconds, Some(2));
        assert_eq!(parsed.limits.memory_bytes, Some(1024 * 1024));
        assert_eq!(parsed.limits.open_files, Some(32));
    }

    #[test]
    fn parse_spec_rejects_malformed_process_resource_limits() {
        for (key, value, expected) in [
            ("cpu_seconds", SysValue::Int(0), "must be a positive int"),
            ("memory_bytes", SysValue::Int(-1), "must be a positive int"),
            ("open_files", SysValue::Str("many".into()), "must be an int"),
        ] {
            let error = parse_spec(spec([
                ("argv", SysValue::List(vec![SysValue::Str("echo".into())])),
                (
                    "limits",
                    SysValue::Map(HashMap::from([(key.into(), value)])),
                ),
            ]))
            .unwrap_err();

            assert!(error.contains(expected), "got {error:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn args_reject_non_utf8_arguments() {
        use std::os::unix::ffi::OsStringExt;

        let error = process_args_value([OsString::from_vec(b"bad-\xFF".to_vec())]).unwrap_err();

        assert!(error.contains("process.args: argument is not valid UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn run_preserves_default_capture_and_honors_explicit_capture_flags() {
        let mut state = ProcState::new();
        let captured = invoke(
            &mut state,
            "run",
            SysArgs(vec![SysValue::Map(HashMap::from([(
                "argv".into(),
                SysValue::List(vec![
                    SysValue::Str("/bin/sh".into()),
                    SysValue::Str("-c".into()),
                    SysValue::Str("printf out; printf err >&2".into()),
                ]),
            )]))]),
        )
        .unwrap();
        let SysValue::Map(captured) = captured else {
            panic!("expected completed process map");
        };
        assert_eq!(captured.get("stdout"), Some(&SysValue::Str("out".into())));
        assert_eq!(captured.get("stderr"), Some(&SysValue::Str("err".into())));

        let not_captured = invoke(
            &mut state,
            "run",
            SysArgs(vec![SysValue::Map(HashMap::from([
                (
                    "argv".into(),
                    SysValue::List(vec![
                        SysValue::Str("/bin/sh".into()),
                        SysValue::Str("-c".into()),
                        SysValue::Str("printf out >/dev/null; printf err >/dev/null".into()),
                    ]),
                ),
                ("capture_stdout".into(), SysValue::Bool(false)),
                ("capture_stderr".into(), SysValue::Bool(false)),
            ]))]),
        )
        .unwrap();
        let SysValue::Map(not_captured) = not_captured else {
            panic!("expected completed process map");
        };
        assert_eq!(
            not_captured.get("stdout"),
            Some(&SysValue::Str(String::new()))
        );
        assert_eq!(
            not_captured.get("stderr"),
            Some(&SysValue::Str(String::new()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn run_rejects_invalid_utf8_output() {
        let mut state = ProcState::new();
        let error = invoke(
            &mut state,
            "run",
            SysArgs(vec![SysValue::Map(HashMap::from([(
                "argv".into(),
                SysValue::List(vec![
                    SysValue::Str("/bin/sh".into()),
                    SysValue::Str("-c".into()),
                    SysValue::Str("printf '\\377'".into()),
                ]),
            )]))]),
        )
        .unwrap_err();

        assert!(error.contains("process.run: stdout is not valid UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn run_enforces_timeout_ms() {
        let mut state = ProcState::new();
        let error = invoke(
            &mut state,
            "run",
            SysArgs(vec![SysValue::Map(HashMap::from([
                (
                    "argv".into(),
                    SysValue::List(vec![
                        SysValue::Str("/bin/sh".into()),
                        SysValue::Str("-c".into()),
                        SysValue::Str("sleep 1".into()),
                    ]),
                ),
                ("timeout_ms".into(), SysValue::Int(1)),
            ]))]),
        )
        .unwrap_err();

        assert!(error.contains("process.run: process timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn wait_result_enforces_spawn_timeout_deadline() {
        let mut state = ProcState::new();
        let handle = invoke(
            &mut state,
            "spawn",
            SysArgs(vec![SysValue::Map(HashMap::from([
                (
                    "argv".into(),
                    SysValue::List(vec![
                        SysValue::Str("/bin/sh".into()),
                        SysValue::Str("-c".into()),
                        SysValue::Str("exec sleep 1".into()),
                    ]),
                ),
                ("timeout_ms".into(), SysValue::Int(1)),
            ]))]),
        )
        .unwrap();
        let SysValue::Int(handle) = handle else {
            panic!("expected process handle");
        };
        std::thread::sleep(Duration::from_millis(20));

        let error = invoke(
            &mut state,
            "wait_result",
            SysArgs(vec![SysValue::Int(handle)]),
        )
        .unwrap_err();

        assert!(error.contains("process.wait_result: process timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn read_stdout_enforces_spawn_timeout_deadline() {
        let mut state = ProcState::new();
        let handle = invoke(
            &mut state,
            "spawn",
            SysArgs(vec![SysValue::Map(HashMap::from([
                (
                    "argv".into(),
                    SysValue::List(vec![
                        SysValue::Str("/bin/sh".into()),
                        SysValue::Str("-c".into()),
                        SysValue::Str("exec sleep 1".into()),
                    ]),
                ),
                ("capture_stdout".into(), SysValue::Bool(true)),
                ("timeout_ms".into(), SysValue::Int(1)),
            ]))]),
        )
        .unwrap();
        let SysValue::Int(handle) = handle else {
            panic!("expected process handle");
        };
        let error = invoke(
            &mut state,
            "read_stdout",
            SysArgs(vec![SysValue::Int(handle)]),
        )
        .unwrap_err();

        assert!(error.contains("process.read_stdout: process timed out"));
        let error = invoke(&mut state, "kill", SysArgs(vec![SysValue::Int(handle)])).unwrap_err();
        assert!(error.contains("unknown handle"));
    }

    #[cfg(unix)]
    #[test]
    fn wait_rejects_invalid_utf8_output() {
        let mut state = ProcState::new();
        let handle = invoke(
            &mut state,
            "spawn",
            SysArgs(vec![SysValue::Map(HashMap::from([
                (
                    "argv".into(),
                    SysValue::List(vec![
                        SysValue::Str("/bin/sh".into()),
                        SysValue::Str("-c".into()),
                        SysValue::Str("printf '\\377'".into()),
                    ]),
                ),
                ("capture_stdout".into(), SysValue::Bool(true)),
            ]))]),
        )
        .unwrap();
        let SysValue::Int(handle) = handle else {
            panic!("expected process handle");
        };

        let error = invoke(&mut state, "wait", SysArgs(vec![SysValue::Int(handle)])).unwrap_err();

        assert!(error.contains("process.wait: stdout is not valid UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn wait_enforces_spawn_timeout_ms() {
        let mut state = ProcState::new();
        let handle = invoke(
            &mut state,
            "spawn",
            SysArgs(vec![SysValue::Map(HashMap::from([
                (
                    "argv".into(),
                    SysValue::List(vec![
                        SysValue::Str("/bin/sh".into()),
                        SysValue::Str("-c".into()),
                        SysValue::Str("exec sleep 1".into()),
                    ]),
                ),
                ("timeout_ms".into(), SysValue::Int(1)),
            ]))]),
        )
        .unwrap();
        let SysValue::Int(handle) = handle else {
            panic!("expected process handle");
        };

        let error = invoke(&mut state, "wait", SysArgs(vec![SysValue::Int(handle)])).unwrap_err();

        assert!(error.contains("process.wait: process timed out"));
    }

    #[cfg(unix)]
    #[test]
    fn drop_reaps_live_children() {
        let mut state = ProcState::new();
        let handle = invoke(
            &mut state,
            "spawn",
            SysArgs(vec![SysValue::Map(HashMap::from([(
                "argv".into(),
                SysValue::List(vec![
                    SysValue::Str("/bin/sh".into()),
                    SysValue::Str("-c".into()),
                    SysValue::Str("exec sleep 30".into()),
                ]),
            )]))]),
        )
        .unwrap();
        let SysValue::Int(handle) = handle else {
            panic!("expected process handle");
        };
        let pid = state.handles[&handle].child.id() as i32;

        // The child is alive while the session owns it (signal 0 only probes
        // existence, it sends nothing).
        // SAFETY: `pid` is a child of this process; signal 0 has no effect.
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            0,
            "child {pid} should be alive"
        );

        // Dropping the session must kill *and* reap the child: a merely-killed
        // child lingers as a zombie (kill(pid, 0) == 0) until waited, so once
        // `kill` reports ESRCH we know `Drop` waited it.
        drop(state);
        let mut reaped = false;
        for _ in 0..200 {
            // SAFETY: same as above.
            if unsafe { libc::kill(pid, 0) } != 0 {
                reaped = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(reaped, "child {pid} was not reaped after ProcState drop");
    }
}
