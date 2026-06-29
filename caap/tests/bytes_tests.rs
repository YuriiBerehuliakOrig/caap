//! The `bytes` family is a deliberate **conversion-only bridge**: sys host
//! services traffic in binary (`SysValue::Bytes`), so the language must be able
//! to convert and measure — slicing/searching binary data in-language is out of
//! scope until a consumer exists (see docs/builtins.md). These are the first
//! tests the family ever had.

use caap_core::frontend::eval_source;
use caap_core::RuntimeValue;

fn ok(src: &str) -> RuntimeValue {
    eval_source(src).expect("eval")
}

#[test]
fn string_roundtrips_through_bytes() {
    assert_eq!(
        ok(r#"(bytes->string (string->bytes "héllo"))"#),
        RuntimeValue::Str("héllo".into())
    );
}

#[test]
fn bytes_length_counts_utf8_bytes() {
    assert_eq!(
        ok(r#"(bytes_length (string->bytes "héllo"))"#),
        RuntimeValue::Int(6)
    );
}

#[test]
fn list_roundtrip_preserves_bytes() {
    assert_eq!(
        ok("(bytes->string (bytes_from_list (bytes_to_list (string->bytes \"ok\"))))"),
        RuntimeValue::Str("ok".into())
    );
    assert_eq!(
        ok("(get (bytes_to_list (bytes_from_list (list_of 104 105))) 0 0)"),
        RuntimeValue::Int(104)
    );
}

#[test]
fn bytes_from_list_rejects_out_of_range() {
    assert!(eval_source("(bytes_from_list (list_of 256))").is_err());
    assert!(eval_source("(bytes_from_list (list_of -1))").is_err());
}

#[test]
fn invalid_utf8_decoding_is_an_error() {
    assert!(eval_source("(bytes->string (bytes_from_list (list_of 255 254)))").is_err());
}
