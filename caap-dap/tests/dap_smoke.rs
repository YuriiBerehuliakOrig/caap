//! End-to-end smoke test: drive the real `caap-dap` binary over stdio with a
//! scripted DAP session against a tiny bootstrap file, asserting the
//! initialize → stopped(entry) → continue → terminated handshake.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn write_msg(stdin: &mut ChildStdin, value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len()).unwrap();
    stdin.write_all(&body).unwrap();
    stdin.flush().unwrap();
}

fn read_msg(reader: &mut impl BufRead) -> Option<Value> {
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("Content-Length:") {
            content_length = rest.trim().parse().ok();
        }
    }
    let len = content_length?;
    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).ok()?;
    serde_json::from_slice(&buf).ok()
}

struct Dap {
    child: Child,
    stdin: ChildStdin,
    seq: i64,
}

impl Dap {
    fn request(&mut self, command: &str, arguments: Value) {
        let seq = self.seq;
        self.seq += 1;
        write_msg(
            &mut self.stdin,
            &json!({ "seq": seq, "type": "request", "command": command, "arguments": arguments }),
        );
    }
}

#[test]
fn dap_entry_continue_terminate() {
    // Tiny bootstrap: no stdlib needed, three source lines.
    let dir = std::env::temp_dir().join(format!("caap-dap-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bootstrap = dir.join("mini.caap");
    std::fs::write(&bootstrap, "(do\n  (bind ((x 1)) x)\n  (bind ((y 2)) y))\n").unwrap();
    let bootstrap_str = std::fs::canonicalize(&bootstrap)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_caap-dap"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn caap-dap");
    let stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut dap = Dap {
        child,
        stdin,
        seq: 1,
    };

    dap.request(
        "initialize",
        json!({ "adapterID": "caap", "linesStartAt1": true }),
    );
    dap.request(
        "launch",
        json!({ "bootstrap": bootstrap_str, "stopOnEntry": true }),
    );
    dap.request("configurationDone", json!({}));

    let mut saw_initialized = false;
    let mut saw_stopped = false;
    let mut saw_terminated = false;
    let mut continued = false;
    let deadline = Instant::now() + Duration::from_secs(30);

    while Instant::now() < deadline {
        let Some(msg) = read_msg(&mut reader) else {
            break;
        };
        let ty = msg.get("type").and_then(Value::as_str).unwrap_or("");
        let kind = msg
            .get("event")
            .or_else(|| msg.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("");
        match (ty, kind) {
            ("event", "initialized") => saw_initialized = true,
            ("event", "stopped") => {
                saw_stopped = true;
                if !continued {
                    continued = true;
                    dap.request("continue", json!({ "threadId": 1 }));
                }
            }
            ("event", "terminated") => {
                saw_terminated = true;
                break;
            }
            _ => {}
        }
    }

    dap.request("disconnect", json!({}));
    let _ = dap.child.wait();
    std::fs::remove_dir_all(&dir).ok();

    assert!(saw_initialized, "missing `initialized` event");
    assert!(saw_stopped, "missing `stopped` (entry) event");
    assert!(saw_terminated, "missing `terminated` event");
}

/// Source-mode debugging against the real `stdlib` bootstrap: the run command
/// is resolved through the `caap.session.commands` capability map, the user
/// program is evaluated under the hook, and stepping lands on the user's own
/// file (proving surface spans survive the loader's read→expand→eval and that
/// the stepping focus pins to the program). Ends in a clean termination.
#[test]
fn dap_source_mode_steps_user_program_against_stdlib() {
    let stdlib_bootstrap =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../stdlib/bootstrap.caap");
    let bootstrap_str = std::fs::canonicalize(&stdlib_bootstrap)
        .expect("stdlib/bootstrap.caap exists")
        .to_str()
        .unwrap()
        .to_string();

    let dir = std::env::temp_dir().join(format!("caap-dap-src-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let program = dir.join("prog.caap");
    // A value-shaped program (no `(module …)` directive): kernel forms only, so
    // the loader's expansion pass is a no-op and `load` evaluates it directly.
    std::fs::write(&program, "(do\n  (bind ((x 1)) x)\n  (bind ((y 2)) y))\n").unwrap();
    let program_str = std::fs::canonicalize(&program)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_caap-dap"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn caap-dap");
    let stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut dap = Dap {
        child,
        stdin,
        seq: 1,
    };

    dap.request(
        "initialize",
        json!({ "adapterID": "caap", "linesStartAt1": true }),
    );
    dap.request(
        "launch",
        json!({
            "bootstrap": bootstrap_str,
            "program": program_str,
            "stopOnEntry": true,
        }),
    );
    dap.request("configurationDone", json!({}));

    let mut saw_stopped = false;
    let mut saw_terminated = false;
    let mut requested_stack = false;
    let mut frame_path: Option<String> = None;
    // The stdlib bootstrap load is heavy; allow generous headroom.
    let deadline = Instant::now() + Duration::from_secs(90);

    while Instant::now() < deadline {
        let Some(msg) = read_msg(&mut reader) else {
            break;
        };
        let ty = msg.get("type").and_then(Value::as_str).unwrap_or("");
        let kind = msg
            .get("event")
            .or_else(|| msg.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("");
        match (ty, kind) {
            ("event", "stopped") => {
                saw_stopped = true;
                if !requested_stack {
                    requested_stack = true;
                    dap.request("stackTrace", json!({ "threadId": 1 }));
                } else {
                    dap.request("continue", json!({ "threadId": 1 }));
                }
            }
            ("response", "stackTrace") => {
                frame_path = msg
                    .get("body")
                    .and_then(|b| b.get("stackFrames"))
                    .and_then(Value::as_array)
                    .and_then(|frames| frames.first())
                    .and_then(|f| f.get("source"))
                    .and_then(|s| s.get("path"))
                    .and_then(Value::as_str)
                    .map(str::to_string);
                dap.request("continue", json!({ "threadId": 1 }));
            }
            ("event", "terminated") => {
                saw_terminated = true;
                break;
            }
            _ => {}
        }
    }

    dap.request("disconnect", json!({}));
    let _ = dap.child.wait();
    std::fs::remove_dir_all(&dir).ok();

    assert!(saw_stopped, "missing entry `stopped` in source mode");
    assert!(
        saw_terminated,
        "source-mode run never terminated (run command unresolved?)"
    );
    assert_eq!(
        frame_path.as_deref(),
        Some(program_str.as_str()),
        "stepping should land in the user program file, not stdlib"
    );
}

/// `restart` re-launches the worker in-place: after the first entry stop a
/// restart yields a *second* entry stop (the superseded worker's terminated is
/// swallowed via generation gating), and only after continuing does the session
/// terminate.
#[test]
fn dap_restart_relaunches_worker() {
    let dir = std::env::temp_dir().join(format!("caap-dap-restart-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let bootstrap = dir.join("mini.caap");
    std::fs::write(&bootstrap, "(do\n  (bind ((x 1)) x)\n  (bind ((y 2)) y))\n").unwrap();
    let bootstrap_str = std::fs::canonicalize(&bootstrap)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let mut child = Command::new(env!("CARGO_BIN_EXE_caap-dap"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn caap-dap");
    let stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut dap = Dap {
        child,
        stdin,
        seq: 1,
    };

    dap.request(
        "initialize",
        json!({ "adapterID": "caap", "linesStartAt1": true }),
    );
    dap.request(
        "launch",
        json!({ "bootstrap": bootstrap_str, "stopOnEntry": true }),
    );
    dap.request("configurationDone", json!({}));

    let mut stopped_count = 0;
    let mut terminated_after = None;
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let Some(msg) = read_msg(&mut reader) else {
            break;
        };
        let ty = msg.get("type").and_then(Value::as_str).unwrap_or("");
        let kind = msg
            .get("event")
            .or_else(|| msg.get("command"))
            .and_then(Value::as_str)
            .unwrap_or("");
        match (ty, kind) {
            ("event", "stopped") => {
                stopped_count += 1;
                if stopped_count == 1 {
                    dap.request("restart", json!({}));
                } else {
                    dap.request("continue", json!({ "threadId": 1 }));
                }
            }
            ("event", "terminated") => {
                terminated_after = Some(stopped_count);
                break;
            }
            _ => {}
        }
    }

    dap.request("disconnect", json!({}));
    let _ = dap.child.wait();
    std::fs::remove_dir_all(&dir).ok();

    assert!(
        stopped_count >= 2,
        "restart should produce a second entry stop, saw {stopped_count}"
    );
    assert_eq!(
        terminated_after,
        Some(stopped_count),
        "terminated must come only after the post-restart run was continued"
    );
}
