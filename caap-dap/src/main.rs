//! CAAP compile-time (CTFE) debugger — Debug Adapter Protocol server.
//!
//! Speaks DAP over stdio on the main thread. On `launch` it spawns an evaluation
//! worker thread that installs a thread-local debug hook and drives the stdlib
//! bootstrap; pauses/snapshots flow back over channels. See `worker.rs` and
//! `controller.rs`.

use std::io::{self, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use caap_dap::dap_types::{event, response};
use caap_dap::protocol::{BreakpointSpec, DapEvent, DebugCommand};
use caap_dap::wire;
use caap_dap::worker::{spawn_worker, DebugTarget, LaunchArgs};

enum Inbound {
    Client(Value),
    /// A worker event tagged with the generation that produced it; events from a
    /// superseded worker (after `restart`) carry a stale generation and are
    /// dropped.
    Worker(u64, DapEvent),
    ClientEof,
}

struct State {
    seq: i64,
    cmd_tx: Option<Sender<DebugCommand>>,
    start_tx: Option<Sender<()>>,
    frames: Vec<caap_dap::protocol::FrameSnapshot>,
    should_exit: bool,
    /// Stored so `restart` can re-launch the worker (reloading stdlib + program
    /// from disk) without a fresh DAP session.
    launch_args: Option<LaunchArgs>,
    /// The live worker generation; bumped on each (re)launch. Shared with the
    /// stdout forwarder so its output is attributed to the current run.
    current_gen: Arc<AtomicU64>,
    /// The focus file (the user's program), to de-emphasize foreign stack frames.
    focus: Option<PathBuf>,
    /// Handles to spawned evaluation workers. A superseded worker (after
    /// `restart`) is told to disconnect and unwinds on its own; we keep its
    /// handle only to reap it once finished so the vector can't grow unbounded
    /// across many restarts. We never block-join here (a paused worker that has
    /// not yet seen the disconnect would deadlock the protocol loop).
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl State {
    fn new(current_gen: Arc<AtomicU64>) -> Self {
        Self {
            seq: 1,
            cmd_tx: None,
            start_tx: None,
            frames: Vec::new(),
            should_exit: false,
            launch_args: None,
            current_gen,
            focus: None,
            workers: Vec::new(),
        }
    }
    fn next_seq(&mut self) -> i64 {
        let s = self.seq;
        self.seq += 1;
        s
    }
}

fn main() -> io::Result<()> {
    eprintln!("caap-dap: starting");

    // Redirect the process stdout to a pipe so the debugged program's output
    // (e.g. runtime `io.println`) does not corrupt the DAP protocol, which we
    // keep writing to the *original* stdout (`dap_out`). Captured program
    // output is forwarded as DAP `output` events.
    let (dap_out, captured) = caap_dap::stdio_capture::redirect_stdout()?;

    let (inbound_tx, inbound_rx) = mpsc::channel::<Inbound>();
    let current_gen = Arc::new(AtomicU64::new(0));

    // Program-output forwarder: read captured stdout and emit `output` events,
    // tagged with whatever generation is current when the bytes arrive.
    {
        let tx = inbound_tx.clone();
        let gen = current_gen.clone();
        let mut captured = captured;
        std::thread::spawn(move || {
            use std::io::Read;
            let mut buf = [0u8; 4096];
            loop {
                match captured.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let text = String::from_utf8_lossy(&buf[..n]).to_string();
                        if tx
                            .send(Inbound::Worker(
                                gen.load(Ordering::Relaxed),
                                DapEvent::Output {
                                    category: "stdout".to_string(),
                                    text,
                                },
                            ))
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
        });
    }

    // stdin reader thread.
    {
        let tx = inbound_tx.clone();
        std::thread::spawn(move || {
            let stdin = io::stdin();
            let mut reader = BufReader::new(stdin.lock());
            loop {
                match wire::read_message(&mut reader) {
                    Ok(Some(msg)) => {
                        if tx.send(Inbound::Client(msg)).is_err() {
                            break;
                        }
                    }
                    _ => {
                        let _ = tx.send(Inbound::ClientEof);
                        break;
                    }
                }
            }
        });
    }

    let mut state = State::new(current_gen);
    // DAP protocol writes go to the saved original stdout, not the redirected
    // fd 1 (which now feeds the program-output forwarder).
    let mut out = dap_out;

    for inbound in inbound_rx {
        match inbound {
            Inbound::Client(msg) => handle_request(&mut out, &mut state, &inbound_tx, &msg),
            Inbound::Worker(gen, evt) => {
                // Drop events from a worker superseded by `restart`.
                if gen == state.current_gen.load(Ordering::Relaxed) {
                    handle_worker_event(&mut out, &mut state, evt);
                }
            }
            Inbound::ClientEof => {
                if let Some(tx) = &state.cmd_tx {
                    let _ = tx.send(DebugCommand::Disconnect);
                }
                break;
            }
        }
        if state.should_exit {
            break;
        }
    }
    eprintln!("caap-dap: stopped");
    Ok(())
}

fn send(out: &mut impl Write, state: &mut State, mut value: Value) {
    value["seq"] = json!(state.next_seq());
    let _ = wire::write_message(out, &value);
}

fn handle_request(
    out: &mut impl Write,
    state: &mut State,
    inbound_tx: &Sender<Inbound>,
    msg: &Value,
) {
    let command = msg.get("command").and_then(Value::as_str).unwrap_or("");
    let request_seq = msg.get("seq").and_then(Value::as_i64).unwrap_or(0);
    let args = msg.get("arguments").cloned().unwrap_or(Value::Null);

    match command {
        "initialize" => {
            let body = json!({
                "supportsConfigurationDoneRequest": true,
                "supportsTerminateRequest": true,
                "supportsConditionalBreakpoints": true,
                "supportsHitConditionalBreakpoints": true,
                "supportsLogPoints": true,
                "supportsEvaluateForHovers": true,
                "supportsFunctionBreakpoints": true,
                "supportsExceptionInfoRequest": true,
                "supportsSetVariable": true,
                "supportsDataBreakpoints": true,
                "supportsRestartRequest": true,
                "supportsCompletionsRequest": true,
                "exceptionBreakpointFilters": [
                    { "filter": "errors", "label": "CTFE errors", "default": false }
                ],
            });
            send(out, state, response(0, request_seq, command, true, body));
            send(out, state, event(0, "initialized", json!({})));
        }
        "launch" => {
            launch(state, inbound_tx, &args);
            send(
                out,
                state,
                response(0, request_seq, command, true, json!({})),
            );
        }
        "setBreakpoints" => {
            let body = set_breakpoints(state, &args);
            send(out, state, response(0, request_seq, command, true, body));
        }
        "setExceptionBreakpoints" => {
            let filters = args
                .get("filters")
                .and_then(Value::as_array)
                .map(|a| a.iter().any(|f| f.as_str() == Some("errors")))
                .unwrap_or(false);
            send_command(state, DebugCommand::SetExceptionBreak(filters));
            send(
                out,
                state,
                response(0, request_seq, command, true, json!({})),
            );
        }
        "setFunctionBreakpoints" => {
            let names: Vec<String> = args
                .get("breakpoints")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|b| b.get("name").and_then(Value::as_str).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let verified: Vec<Value> = names.iter().map(|_| json!({ "verified": true })).collect();
            send_command(state, DebugCommand::SetFunctionBreakpoints(names));
            send(
                out,
                state,
                response(
                    0,
                    request_seq,
                    command,
                    true,
                    json!({ "breakpoints": verified }),
                ),
            );
        }
        "exceptionInfo" => {
            let message = request_exception_info(state).unwrap_or_default();
            let body = json!({
                "exceptionId": "ctfe.error",
                "breakMode": "always",
                "description": message.clone(),
                "details": { "message": message },
            });
            send(out, state, response(0, request_seq, command, true, body));
        }
        "configurationDone" => {
            if let Some(tx) = state.start_tx.take() {
                let _ = tx.send(());
            }
            send(
                out,
                state,
                response(0, request_seq, command, true, json!({})),
            );
        }
        "threads" => {
            let body = json!({ "threads": [{ "id": 1, "name": "ctfe" }] });
            send(out, state, response(0, request_seq, command, true, body));
        }
        "stackTrace" => {
            let focus = state.focus.clone();
            let frames: Vec<Value> = state
                .frames
                .iter()
                .map(|f| {
                    // Frames outside the user's program (stdlib/host internals) are
                    // dimmed so the user's own call chain stands out.
                    let in_focus = match (&focus, &f.file) {
                        (Some(focus), Some(p)) => same_file(focus, p),
                        (None, _) => true,
                        _ => false,
                    };
                    let source = f.file.as_ref().map(|p| {
                        let name = std::path::Path::new(p)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(p);
                        json!({
                            "name": name,
                            "path": p,
                            "presentationHint": if in_focus { "normal" } else { "deemphasize" },
                        })
                    });
                    json!({
                        "id": f.id,
                        "name": f.name,
                        "line": f.line,
                        "column": f.col.max(1),
                        "source": source,
                        "presentationHint": if in_focus { "normal" } else { "subtle" },
                    })
                })
                .collect();
            let total = frames.len();
            let body = json!({ "stackFrames": frames, "totalFrames": total });
            send(out, state, response(0, request_seq, command, true, body));
        }
        "scopes" => {
            let frame_id = args.get("frameId").and_then(Value::as_i64).unwrap_or(0);
            let scope_ref = request_scope_reference(state, frame_id);
            let body = json!({
                "scopes": [{
                    "name": "Locals",
                    "variablesReference": scope_ref,
                    "expensive": false,
                }]
            });
            send(out, state, response(0, request_seq, command, true, body));
        }
        "variables" => {
            let var_ref = args
                .get("variablesReference")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let start = args.get("start").and_then(Value::as_u64).unwrap_or(0) as usize;
            let count = args.get("count").and_then(Value::as_u64).unwrap_or(0) as usize;
            let vars = fetch_variables(state, var_ref, start, count);
            let mapped: Vec<Value> = vars
                .into_iter()
                .map(|v| {
                    let mut obj = json!({
                        "name": v.name,
                        "value": v.value,
                        "type": v.kind,
                        "variablesReference": v.variables_reference,
                    });
                    if v.indexed_variables > 0 {
                        obj["indexedVariables"] = json!(v.indexed_variables);
                    }
                    if v.named_variables > 0 {
                        obj["namedVariables"] = json!(v.named_variables);
                    }
                    obj
                })
                .collect();
            send(
                out,
                state,
                response(
                    0,
                    request_seq,
                    command,
                    true,
                    json!({ "variables": mapped }),
                ),
            );
        }
        "evaluate" => {
            let expression = args
                .get("expression")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let frame_id = args.get("frameId").and_then(Value::as_i64).unwrap_or(0);
            let reply = request_evaluate(state, frame_id, expression);
            let body = json!({
                "result": reply.result,
                "variablesReference": reply.variables_reference,
            });
            send(
                out,
                state,
                response(0, request_seq, command, reply.success, body),
            );
        }
        "setVariable" => {
            let reference = args
                .get("variablesReference")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let value = args
                .get("value")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let reply = request_set_variable(state, reference, name, value);
            let body = json!({
                "value": reply.result,
                "variablesReference": reply.variables_reference,
            });
            send(
                out,
                state,
                response(0, request_seq, command, reply.success, body),
            );
        }
        "dataBreakpointInfo" => {
            let reference = args
                .get("variablesReference")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            let frame_id = args.get("frameId").and_then(Value::as_i64);
            let name = args
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let body = match request_data_breakpoint_info(state, reference, frame_id, name.clone())
            {
                Some(data_id) => json!({
                    "dataId": data_id,
                    "description": format!("{name} changes"),
                    "accessTypes": ["write"],
                    "canPersist": false,
                }),
                None => json!({ "dataId": Value::Null, "description": "not watchable" }),
            };
            send(out, state, response(0, request_seq, command, true, body));
        }
        "setDataBreakpoints" => {
            let ids: Vec<String> = args
                .get("breakpoints")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|b| b.get("dataId").and_then(Value::as_str).map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let verified: Vec<Value> = ids.iter().map(|_| json!({ "verified": true })).collect();
            send_command(state, DebugCommand::SetDataBreakpoints(ids));
            send(
                out,
                state,
                response(
                    0,
                    request_seq,
                    command,
                    true,
                    json!({ "breakpoints": verified }),
                ),
            );
        }
        "restart" => {
            restart(state, inbound_tx);
            send(
                out,
                state,
                response(0, request_seq, command, true, json!({})),
            );
        }
        "completions" => {
            let frame_id = args.get("frameId").and_then(Value::as_i64).unwrap_or(0);
            let targets: Vec<Value> = request_completions(state, frame_id)
                .into_iter()
                .map(|name| json!({ "label": name, "type": "variable" }))
                .collect();
            send(
                out,
                state,
                response(0, request_seq, command, true, json!({ "targets": targets })),
            );
        }
        "continue" => {
            send_command(state, DebugCommand::Continue);
            send(
                out,
                state,
                response(
                    0,
                    request_seq,
                    command,
                    true,
                    json!({ "allThreadsContinued": true }),
                ),
            );
        }
        "next" => {
            send_command(state, DebugCommand::Next);
            send(
                out,
                state,
                response(0, request_seq, command, true, json!({})),
            );
        }
        "stepIn" => {
            send_command(state, DebugCommand::StepIn);
            send(
                out,
                state,
                response(0, request_seq, command, true, json!({})),
            );
        }
        "stepOut" => {
            send_command(state, DebugCommand::StepOut);
            send(
                out,
                state,
                response(0, request_seq, command, true, json!({})),
            );
        }
        "disconnect" | "terminate" => {
            send_command(state, DebugCommand::Disconnect);
            send(
                out,
                state,
                response(0, request_seq, command, true, json!({})),
            );
            state.should_exit = true;
        }
        _ => {
            // Be lenient: acknowledge unknown requests.
            send(
                out,
                state,
                response(0, request_seq, command, true, json!({})),
            );
        }
    }
}

fn launch(state: &mut State, inbound_tx: &Sender<Inbound>, args: &Value) {
    let bootstrap = args
        .get("bootstrap")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .unwrap_or_default();
    // Canonicalize the program/root up front so the paths that reach the
    // evaluator (and thus the source spans it stamps on every node) match the
    // canonical forms used for breakpoints (`set_breakpoints`) and the stepping
    // focus (`DebugController::new`). Without this, a `program` given with a
    // symlink or `..` would stamp non-canonical span paths that never compare
    // equal to the canonical breakpoint/focus keys, so breakpoints would
    // silently fail to bind and focus stepping would never pause.
    let canonical = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    let program = args
        .get("program")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .map(canonical);
    let root = args
        .get("root")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .map(canonical);
    let entry = args
        .get("entry")
        .and_then(Value::as_str)
        .map(str::to_string);
    let stop_on_entry = args
        .get("stopOnEntry")
        .and_then(Value::as_bool)
        .unwrap_or(true);

    // Decide what to debug: a module from a root, a single source file, or the
    // stdlib bootstrap itself.
    let target = if let Some(entry) = entry {
        DebugTarget::Root {
            root: root.clone().unwrap_or_else(|| PathBuf::from(".")),
            entry,
        }
    } else if let Some(path) = program.clone() {
        DebugTarget::Source { path }
    } else {
        DebugTarget::Bootstrap
    };

    let mut read_roots = Vec::new();
    if let Some(root) = &root {
        read_roots.push(root.clone());
    }
    if let Some(parent) = program.as_ref().and_then(|p| p.parent()) {
        let parent = parent.to_path_buf();
        if !read_roots.contains(&parent) {
            read_roots.push(parent);
        }
    }

    // Stepping focuses on the program file when given, else the (single-file)
    // source program.
    let focus = program.clone().or_else(|| match &target {
        DebugTarget::Source { path } => Some(path.clone()),
        _ => None,
    });

    let launch_args = LaunchArgs {
        bootstrap,
        read_roots,
        target,
        focus: focus.clone(),
        stop_on_entry,
    };
    // `focus` is already canonical (derived from the canonicalized program).
    state.focus = focus;
    state.launch_args = Some(launch_args.clone());
    start_worker(state, inbound_tx, launch_args, false);
}

/// Spawn the evaluation worker for `args`, wiring its command/event channels and
/// a generation-tagged event forwarder. When `auto_start` is set (a `restart`,
/// where the client won't re-send `configurationDone`) the run is released
/// immediately; otherwise the start signal waits for `configurationDone`.
fn start_worker(
    state: &mut State,
    inbound_tx: &Sender<Inbound>,
    args: LaunchArgs,
    auto_start: bool,
) {
    let (cmd_tx, cmd_rx) = mpsc::channel::<DebugCommand>();
    let (start_tx, start_rx) = mpsc::channel::<()>();
    let (event_tx, event_rx) = mpsc::channel::<DapEvent>();

    // This worker owns the next generation; its events (and any stdout produced
    // while it runs) are now the current ones.
    let gen = state.current_gen.fetch_add(1, Ordering::Relaxed) + 1;
    state.cmd_tx = Some(cmd_tx);
    if auto_start {
        let _ = start_tx.send(());
    } else {
        state.start_tx = Some(start_tx);
    }

    // Reap any workers that have already finished, then track the new one.
    state.workers.retain(|h| !h.is_finished());
    match spawn_worker(args, event_tx, cmd_rx, start_rx) {
        Ok(handle) => state.workers.push(handle),
        Err(error) => {
            let _ = inbound_tx.send(Inbound::Worker(
                gen,
                DapEvent::Terminated {
                    error: Some(format!("failed to spawn evaluator thread: {error}")),
                },
            ));
            return;
        }
    }

    let forward_tx = inbound_tx.clone();
    std::thread::spawn(move || {
        while let Ok(evt) = event_rx.recv() {
            if forward_tx.send(Inbound::Worker(gen, evt)).is_err() {
                break;
            }
        }
    });
}

/// Tear down the live worker and re-launch from the stored args, reloading the
/// stdlib bootstrap and program source from disk (a "hot reload").
fn restart(state: &mut State, inbound_tx: &Sender<Inbound>) {
    if let Some(tx) = &state.cmd_tx {
        let _ = tx.send(DebugCommand::Disconnect);
    }
    state.frames.clear();
    if let Some(args) = state.launch_args.clone() {
        start_worker(state, inbound_tx, args, true);
    }
}

fn set_breakpoints(state: &mut State, args: &Value) -> Value {
    let path = args
        .get("source")
        .and_then(|s| s.get("path"))
        .and_then(Value::as_str);
    let requested = args
        .get("breakpoints")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let canonical = path.and_then(|p| std::fs::canonicalize(p).ok());
    let specs: Vec<BreakpointSpec> = requested
        .iter()
        .filter_map(|b| {
            let line = b.get("line").and_then(Value::as_u64)? as usize;
            Some(BreakpointSpec {
                line,
                condition: b
                    .get("condition")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                hit_condition: b
                    .get("hitCondition")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                log_message: b
                    .get("logMessage")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect();

    let verified = canonical.is_some();
    if let (Some(file), Some(tx)) = (canonical, &state.cmd_tx) {
        let _ = tx.send(DebugCommand::SetBreakpoints {
            file,
            breakpoints: specs,
        });
    }

    let results: Vec<Value> = requested
        .iter()
        .map(|b| {
            json!({
                "verified": verified,
                "line": b.get("line").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect();
    json!({ "breakpoints": results })
}

/// Whether two paths denote the same file, comparing canonical forms when
/// available and falling back to a string match.
fn same_file(a: &std::path::Path, b: &str) -> bool {
    let b = PathBuf::from(b);
    match (std::fs::canonicalize(a), std::fs::canonicalize(&b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

fn send_command(state: &State, cmd: DebugCommand) {
    if let Some(tx) = &state.cmd_tx {
        let _ = tx.send(cmd);
    }
}

fn request_exception_info(state: &State) -> Option<String> {
    let tx = state.cmd_tx.as_ref()?;
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .send(DebugCommand::ExceptionInfo { reply: reply_tx })
        .is_err()
    {
        return None;
    }
    reply_rx.recv_timeout(Duration::from_secs(3)).ok().flatten()
}

fn request_scope_reference(state: &State, frame_id: i64) -> i64 {
    let Some(tx) = &state.cmd_tx else {
        return 0;
    };
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .send(DebugCommand::Scopes {
            frame_id,
            reply: reply_tx,
        })
        .is_err()
    {
        return 0;
    }
    reply_rx.recv_timeout(Duration::from_secs(3)).unwrap_or(0)
}

fn fetch_variables(
    state: &State,
    reference: i64,
    start: usize,
    count: usize,
) -> Vec<caap_dap::protocol::VarSnapshot> {
    let Some(tx) = &state.cmd_tx else {
        return Vec::new();
    };
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .send(DebugCommand::Variables {
            reference,
            start,
            count,
            reply: reply_tx,
        })
        .is_err()
    {
        return Vec::new();
    }
    reply_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap_or_default()
}

fn request_evaluate(
    state: &State,
    frame_id: i64,
    expression: String,
) -> caap_dap::protocol::EvalReply {
    let fail = || caap_dap::protocol::EvalReply {
        result: String::new(),
        variables_reference: 0,
        success: false,
    };
    let Some(tx) = &state.cmd_tx else {
        return fail();
    };
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .send(DebugCommand::Evaluate {
            expression,
            frame_id,
            reply: reply_tx,
        })
        .is_err()
    {
        return fail();
    }
    reply_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| fail())
}

fn request_set_variable(
    state: &State,
    reference: i64,
    name: String,
    value: String,
) -> caap_dap::protocol::EvalReply {
    let fail = || caap_dap::protocol::EvalReply {
        result: "no session".to_string(),
        variables_reference: 0,
        success: false,
    };
    let Some(tx) = &state.cmd_tx else {
        return fail();
    };
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .send(DebugCommand::SetVariable {
            reference,
            name,
            value,
            reply: reply_tx,
        })
        .is_err()
    {
        return fail();
    }
    reply_rx
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| fail())
}

fn request_completions(state: &State, frame_id: i64) -> Vec<String> {
    let Some(tx) = &state.cmd_tx else {
        return Vec::new();
    };
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .send(DebugCommand::Completions {
            frame_id,
            reply: reply_tx,
        })
        .is_err()
    {
        return Vec::new();
    }
    reply_rx
        .recv_timeout(Duration::from_secs(3))
        .unwrap_or_default()
}

fn request_data_breakpoint_info(
    state: &State,
    reference: i64,
    frame_id: Option<i64>,
    name: String,
) -> Option<String> {
    let tx = state.cmd_tx.as_ref()?;
    let (reply_tx, reply_rx) = mpsc::channel();
    if tx
        .send(DebugCommand::DataBreakpointInfo {
            reference,
            frame_id,
            name,
            reply: reply_tx,
        })
        .is_err()
    {
        return None;
    }
    reply_rx.recv_timeout(Duration::from_secs(3)).ok().flatten()
}

fn handle_worker_event(out: &mut impl Write, state: &mut State, evt: DapEvent) {
    match evt {
        DapEvent::Stopped { reason, frames, .. } => {
            state.frames = frames;
            let body = json!({
                "reason": reason.as_dap(),
                "threadId": 1,
                "allThreadsStopped": true,
            });
            send(out, state, event(0, "stopped", body));
        }
        DapEvent::Output { category, text } => {
            let body = json!({ "category": category, "output": text });
            send(out, state, event(0, "output", body));
        }
        DapEvent::Terminated { error } => {
            if let Some(message) = error {
                let body = json!({ "category": "stderr", "output": format!("{message}\n") });
                send(out, state, event(0, "output", body));
            }
            send(out, state, event(0, "terminated", json!({})));
            send(out, state, event(0, "exited", json!({ "exitCode": 0 })));
        }
    }
}
