//! Cryptographically-secure randomness, sourced from the OS CSPRNG.
//!
//! Entropy comes from `/dev/urandom` (the standard non-blocking CSPRNG on every
//! unix), read fresh per call. This keeps the module stateless and
//! dependency-free, matching `time`/`os`/`path`. Integer ranges are sampled with
//! rejection so the result is unbiased even when the span does not divide 2^64.

use std::io::Read;

use crate::ffi_value::{SysArgs, SysError, SysResult, SysValue};

pub fn invoke(name: &str, args: SysArgs) -> SysResult {
    match name {
        "bytes" => rand_bytes(args),
        "int" => rand_int(args),
        "float" => rand_float(),
        "bool" => rand_bool(),
        _ => Err(format!("rand: unknown export '{name}'").into()),
    }
}

fn rand_bytes(args: SysArgs) -> SysResult {
    let count = args.require_int(0, "rand.bytes")?;
    let count = usize::try_from(count)
        .map_err(|_| SysError::invalid_argument("rand.bytes: count must be a non-negative int"))?;
    let mut buffer = vec![0u8; count];
    fill_secure(&mut buffer)?;
    Ok(SysValue::Bytes(buffer))
}

fn rand_int(args: SysArgs) -> SysResult {
    let lo = args.require_int(0, "rand.int")?;
    let hi = args.require_int(1, "rand.int")?;
    if lo >= hi {
        return Err(SysError::invalid_argument(format!(
            "rand.int: lo ({lo}) must be < hi ({hi})"
        )));
    }
    // Span as u128 lands in 1..=2^64; the bounds are i64 so `hi - lo` can exceed
    // i64 (e.g. across the full range) and must be computed in a wider type.
    let span = (hi as i128 - lo as i128) as u128;
    let offset = if span > u64::MAX as u128 {
        // Full 2^64 range: every u64 maps uniformly, no rejection needed.
        rand_u64()? as u128
    } else {
        uniform_below(span as u64)? as u128
    };
    // lo + offset < hi <= i64::MAX and >= lo >= i64::MIN, so the result fits i64.
    Ok(SysValue::Int((lo as i128 + offset as i128) as i64))
}

fn rand_float() -> SysResult {
    // 53 random bits scaled into [0, 1) — the full f64 mantissa, so every
    // representable multiple of 2^-53 in the range is equally likely.
    let bits = rand_u64()? >> 11;
    Ok(SysValue::Float(bits as f64 / (1u64 << 53) as f64))
}

fn rand_bool() -> SysResult {
    let mut byte = [0u8; 1];
    fill_secure(&mut byte)?;
    Ok(SysValue::Bool(byte[0] & 1 == 1))
}

/// Draw an unbiased value in `[0, span)` (`span > 0`) by rejection sampling: a
/// plain `u64 % span` skews toward small values unless `span` divides 2^64, so
/// values landing in the final incomplete block are discarded and redrawn.
fn uniform_below(span: u64) -> Result<u64, SysError> {
    // `reject` is `2^64 mod span`: the size of the incomplete block at the top
    // of the u64 range. When it is zero, `span` divides 2^64 and every draw is
    // already unbiased.
    let reject = ((u64::MAX % span) + 1) % span;
    loop {
        let value = rand_u64()?;
        if reject == 0 || value <= u64::MAX - reject {
            return Ok(value % span);
        }
    }
}

fn rand_u64() -> Result<u64, SysError> {
    let mut bytes = [0u8; 8];
    fill_secure(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

/// Fill `buffer` with cryptographically-secure random bytes from the OS CSPRNG.
fn fill_secure(buffer: &mut [u8]) -> Result<(), SysError> {
    if buffer.is_empty() {
        return Ok(());
    }
    let mut source = std::fs::File::open("/dev/urandom").map_err(|error| {
        SysError::from_io("rand: cannot open system CSPRNG (/dev/urandom)", error)
    })?;
    source
        .read_exact(buffer)
        .map_err(|error| SysError::from_io("rand: reading from system CSPRNG failed", error))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: Vec<SysValue>) -> SysResult {
        invoke(name, SysArgs(args))
    }

    #[test]
    fn bytes_returns_requested_length_and_varies() {
        let SysValue::Bytes(a) = call("bytes", vec![SysValue::Int(32)]).unwrap() else {
            panic!("expected bytes");
        };
        let SysValue::Bytes(b) = call("bytes", vec![SysValue::Int(32)]).unwrap() else {
            panic!("expected bytes");
        };
        assert_eq!(a.len(), 32);
        assert_eq!(b.len(), 32);
        // Two 32-byte draws colliding is a ~2^-256 event.
        assert_ne!(a, b, "two independent draws must differ");
    }

    #[test]
    fn bytes_zero_is_empty_and_rejects_negative() {
        assert_eq!(
            call("bytes", vec![SysValue::Int(0)]).unwrap(),
            SysValue::Bytes(Vec::new())
        );
        let error = call("bytes", vec![SysValue::Int(-1)]).unwrap_err();
        assert!(error.contains("non-negative"), "got {error:?}");
    }

    #[test]
    fn int_stays_within_half_open_range() {
        for _ in 0..1000 {
            let SysValue::Int(value) =
                call("int", vec![SysValue::Int(-5), SysValue::Int(5)]).unwrap()
            else {
                panic!("expected int");
            };
            assert!((-5..5).contains(&value), "out of range: {value}");
        }
    }

    #[test]
    fn int_eventually_covers_the_whole_small_range() {
        let mut seen = [false; 3];
        for _ in 0..1000 {
            let SysValue::Int(value) =
                call("int", vec![SysValue::Int(0), SysValue::Int(3)]).unwrap()
            else {
                panic!("expected int");
            };
            seen[value as usize] = true;
        }
        assert!(seen.iter().all(|&hit| hit), "did not cover [0,3): {seen:?}");
    }

    #[test]
    fn int_handles_full_range_without_overflow() {
        for _ in 0..1000 {
            let SysValue::Int(_) = call(
                "int",
                vec![SysValue::Int(i64::MIN), SysValue::Int(i64::MAX)],
            )
            .unwrap() else {
                panic!("expected int");
            };
        }
    }

    #[test]
    fn int_rejects_empty_or_inverted_range() {
        let error = call("int", vec![SysValue::Int(5), SysValue::Int(5)]).unwrap_err();
        assert!(error.contains("must be <"), "got {error:?}");
        let error = call("int", vec![SysValue::Int(5), SysValue::Int(1)]).unwrap_err();
        assert!(error.contains("must be <"), "got {error:?}");
    }

    #[test]
    fn float_is_in_unit_interval() {
        for _ in 0..1000 {
            let SysValue::Float(value) = call("float", vec![]).unwrap() else {
                panic!("expected float");
            };
            assert!((0.0..1.0).contains(&value), "out of [0,1): {value}");
        }
    }

    #[test]
    fn bool_yields_both_values() {
        let mut seen_true = false;
        let mut seen_false = false;
        for _ in 0..1000 {
            match call("bool", vec![]).unwrap() {
                SysValue::Bool(true) => seen_true = true,
                SysValue::Bool(false) => seen_false = true,
                other => panic!("expected bool, got {other:?}"),
            }
        }
        assert!(seen_true && seen_false, "bool did not produce both values");
    }

    #[test]
    fn unknown_export_is_reported() {
        let error = invoke("nope", SysArgs(Vec::new())).unwrap_err();
        assert!(error.contains("unknown export"), "got {error:?}");
    }

    #[test]
    fn uniform_below_is_unbiased_enough_over_a_small_span() {
        // A coarse chi-square-free sanity check: counts for a span of 4 should
        // each land near n/4 over many draws.
        let mut counts = [0u32; 4];
        let draws = 20_000;
        for _ in 0..draws {
            counts[uniform_below(4).unwrap() as usize] += 1;
        }
        let expected = draws / 4;
        for (bucket, &count) in counts.iter().enumerate() {
            let delta = (count as i32 - expected as i32).unsigned_abs();
            assert!(
                delta < expected / 2,
                "bucket {bucket} skewed: {count} vs ~{expected}"
            );
        }
    }
}
