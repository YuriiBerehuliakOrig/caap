//! Binary data builtins.
//!
//! `RuntimeValue::Bytes` is an opaque immutable blob produced by the binary sys
//! exports (`fs.read-bytes`, `net.read-bytes`, …). These builtins let CAAP code
//! construct, inspect, and convert it without a platform-specific dependency.
//!
//! Covers: bytes-length, bytes-from-list, bytes-to-list, string->bytes,
//! bytes->string.

use std::rc::Rc;

use crate::values::{eval_err, EvalSignal, RuntimeValue};

fn require_bytes<'a>(v: &'a RuntimeValue, ctx: &str) -> Result<&'a Rc<[u8]>, EvalSignal> {
    match v {
        RuntimeValue::Bytes(b) => Ok(b),
        other => Err(eval_err(format!("{ctx}: expected bytes, got {other}"))),
    }
}

fn bytes_length(args: Vec<RuntimeValue>) -> Result<RuntimeValue, EvalSignal> {
    let bytes = require_bytes(&args[0], "bytes_length")?;
    i64::try_from(bytes.len())
        .map(RuntimeValue::Int)
        .map_err(|_| eval_err("bytes_length: length exceeds int range"))
}

fn bytes_from_list(args: Vec<RuntimeValue>) -> Result<RuntimeValue, EvalSignal> {
    let items = match &args[0] {
        RuntimeValue::List(items) => items.borrow(),
        other => {
            return Err(eval_err(format!(
                "bytes_from_list: expected list, got {other}"
            )))
        }
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items.iter() {
        let RuntimeValue::Int(value) = item else {
            return Err(eval_err(
                "bytes_from_list: elements must be ints in 0..=255",
            ));
        };
        let byte = u8::try_from(*value)
            .map_err(|_| eval_err("bytes_from_list: elements must be ints in 0..=255"))?;
        out.push(byte);
    }
    Ok(RuntimeValue::Bytes(out.into()))
}

fn bytes_to_list(args: Vec<RuntimeValue>) -> Result<RuntimeValue, EvalSignal> {
    let bytes = require_bytes(&args[0], "bytes_to_list")?;
    let items: Vec<RuntimeValue> = bytes.iter().map(|b| RuntimeValue::Int(*b as i64)).collect();
    Ok(RuntimeValue::List(Rc::new(std::cell::RefCell::new(items))))
}

fn string_to_bytes(args: Vec<RuntimeValue>) -> Result<RuntimeValue, EvalSignal> {
    let RuntimeValue::Str(s) = &args[0] else {
        return Err(eval_err(format!(
            "string->bytes: expected string, got {}",
            args[0]
        )));
    };
    Ok(RuntimeValue::Bytes(s.as_bytes().into()))
}

fn bytes_to_string(args: Vec<RuntimeValue>) -> Result<RuntimeValue, EvalSignal> {
    let bytes = require_bytes(&args[0], "bytes->string")?;
    let text = std::str::from_utf8(bytes)
        .map_err(|error| eval_err(format!("bytes->string: invalid UTF-8: {error}")))?;
    Ok(RuntimeValue::Str(text.into()))
}

pub fn register(ev: &mut crate::eval::Evaluator) {
    use crate::values::BuiltinMetadata;
    ev.register_eager(
        "bytes_length",
        1,
        Some(1),
        BuiltinMetadata::eager_runtime().with_signature(&["bytes"], "int"),
        bytes_length,
    );
    ev.register_eager(
        "bytes_from_list",
        1,
        Some(1),
        BuiltinMetadata::eager_runtime().with_signature(&["list"], "bytes"),
        bytes_from_list,
    );
    ev.register_eager(
        "bytes_to_list",
        1,
        Some(1),
        BuiltinMetadata::eager_runtime().with_signature(&["bytes"], "list"),
        bytes_to_list,
    );
    ev.register_eager(
        "string->bytes",
        1,
        Some(1),
        BuiltinMetadata::eager_runtime().with_signature(&["string"], "bytes"),
        string_to_bytes,
    );
    ev.register_eager(
        "bytes->string",
        1,
        Some(1),
        BuiltinMetadata::eager_runtime().with_signature(&["bytes"], "string"),
        bytes_to_string,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(ints: &[i64]) -> RuntimeValue {
        RuntimeValue::List(Rc::new(std::cell::RefCell::new(
            ints.iter().map(|n| RuntimeValue::Int(*n)).collect(),
        )))
    }

    #[test]
    fn list_and_bytes_round_trip() {
        let bytes = bytes_from_list(vec![list(&[0, 127, 255])]).unwrap();
        assert_eq!(bytes, RuntimeValue::Bytes(vec![0, 127, 255].into()));
        assert_eq!(
            bytes_length(vec![bytes.clone()]).unwrap(),
            RuntimeValue::Int(3)
        );
        // List equality is by identity, so compare contents explicitly.
        let RuntimeValue::List(items) = bytes_to_list(vec![bytes]).unwrap() else {
            panic!("expected list");
        };
        let items = items.borrow();
        assert_eq!(
            items.as_slice(),
            &[
                RuntimeValue::Int(0),
                RuntimeValue::Int(127),
                RuntimeValue::Int(255)
            ]
        );
    }

    #[test]
    fn bytes_from_list_rejects_out_of_range() {
        let error = bytes_from_list(vec![list(&[256])]).unwrap_err();
        assert!(error.to_string().contains("0..=255"));
    }

    #[test]
    fn string_bytes_round_trip_and_invalid_utf8_errors() {
        let bytes = string_to_bytes(vec![RuntimeValue::Str("héllo".into())]).unwrap();
        assert_eq!(
            bytes_to_string(vec![bytes]).unwrap(),
            RuntimeValue::Str("héllo".into())
        );

        let invalid = RuntimeValue::Bytes(vec![0xff, 0xfe].into());
        let error = bytes_to_string(vec![invalid]).unwrap_err();
        assert!(error.to_string().contains("invalid UTF-8"));
    }
}
