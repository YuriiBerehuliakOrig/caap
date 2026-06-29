//! Small constructors for outgoing DAP response/event JSON envelopes.

use serde_json::{json, Value};

/// Build a `response` envelope.
pub fn response(seq: i64, request_seq: i64, command: &str, success: bool, body: Value) -> Value {
    json!({
        "seq": seq,
        "type": "response",
        "request_seq": request_seq,
        "success": success,
        "command": command,
        "body": body,
    })
}

/// Build an `event` envelope.
pub fn event(seq: i64, event: &str, body: Value) -> Value {
    json!({
        "seq": seq,
        "type": "event",
        "event": event,
        "body": body,
    })
}
