use std::sync::OnceLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::ffi_value::{SysArgs, SysError, SysResult, SysValue};

/// Monotonic anchor captured on first use. `Instant` has no portable absolute
/// epoch, so `monotonic-ns` reports nanoseconds elapsed since this anchor —
/// suitable for measuring durations, never going backwards across clock changes.
static MONOTONIC_ANCHOR: OnceLock<Instant> = OnceLock::new();

pub fn invoke(name: &str, args: SysArgs) -> SysResult {
    match name {
        "now_unix_ns" => unix_time_value(SystemTime::now(), TimeUnit::Nanoseconds, "now_unix_ns"),
        "unix_millis" => unix_time_value(SystemTime::now(), TimeUnit::Milliseconds, "unix_millis"),
        "unix_seconds" => unix_time_value(SystemTime::now(), TimeUnit::Seconds, "unix_seconds"),
        "monotonic_ns" => {
            let anchor = MONOTONIC_ANCHOR.get_or_init(Instant::now);
            let nanos = anchor.elapsed().as_nanos();
            let value = i64::try_from(nanos).map_err(|_| {
                "time.monotonic_ns: elapsed time exceeds CAAP SYS int range".to_string()
            })?;
            Ok(SysValue::Int(value))
        }
        "sleep_ms" => {
            let ms = args.require_int(0, "time.sleep_ms")?;
            if ms < 0 {
                return Err(SysError::invalid_argument(
                    "time.sleep_ms: milliseconds must be non-negative",
                ));
            }
            std::thread::sleep(Duration::from_millis(ms as u64));
            Ok(SysValue::Null)
        }
        _ => Err(format!("time: unknown export '{name}'").into()),
    }
}

enum TimeUnit {
    Nanoseconds,
    Milliseconds,
    Seconds,
}

fn unix_time_value(now: SystemTime, unit: TimeUnit, export: &str) -> SysResult {
    let duration = now
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("time.{export}: system clock is before UNIX epoch: {error}"))?;
    let value = match unit {
        TimeUnit::Nanoseconds => duration.as_nanos(),
        TimeUnit::Milliseconds => duration.as_millis(),
        TimeUnit::Seconds => duration.as_secs() as u128,
    };
    let value = i64::try_from(value).map_err(|_| {
        SysError::invalid_argument(format!(
            "time.{export}: UNIX timestamp exceeds CAAP SYS int range"
        ))
    })?;
    Ok(SysValue::Int(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn time_exports_return_positive_unix_timestamps() {
        let SysValue::Int(ns) = invoke("now_unix_ns", SysArgs(Vec::new())).unwrap() else {
            panic!("expected integer nanosecond timestamp");
        };
        let SysValue::Int(ms) = invoke("unix_millis", SysArgs(Vec::new())).unwrap() else {
            panic!("expected integer millisecond timestamp");
        };

        assert!(ns > 0);
        assert!(ms > 0);
    }

    #[test]
    fn monotonic_is_nondecreasing_and_sleep_advances_it() {
        let SysValue::Int(t0) = invoke("monotonic_ns", SysArgs(Vec::new())).unwrap() else {
            panic!("expected int");
        };
        invoke("sleep_ms", SysArgs(vec![SysValue::Int(2)])).unwrap();
        let SysValue::Int(t1) = invoke("monotonic_ns", SysArgs(Vec::new())).unwrap() else {
            panic!("expected int");
        };
        assert!(t1 >= t0, "monotonic clock went backwards: {t0} -> {t1}");
    }

    #[test]
    fn sleep_ms_rejects_negative() {
        let error = invoke("sleep_ms", SysArgs(vec![SysValue::Int(-1)])).unwrap_err();
        assert!(error.contains("must be non-negative"));
    }

    #[test]
    fn unix_seconds_is_positive() {
        let SysValue::Int(s) = invoke("unix_seconds", SysArgs(Vec::new())).unwrap() else {
            panic!("expected int seconds");
        };
        assert!(s > 0);
    }

    #[test]
    fn unix_time_value_rejects_times_before_epoch() {
        let before_epoch = UNIX_EPOCH - Duration::from_nanos(1);
        let error =
            unix_time_value(before_epoch, TimeUnit::Nanoseconds, "now_unix_ns").unwrap_err();
        assert!(error.contains("system clock is before UNIX epoch"));
    }

    #[test]
    fn unix_time_value_rejects_values_outside_sys_int_range() {
        let too_large = UNIX_EPOCH + Duration::from_nanos((i64::MAX as u64) + 1);
        let error = unix_time_value(too_large, TimeUnit::Nanoseconds, "now_unix_ns").unwrap_err();
        assert!(error.contains("exceeds CAAP SYS int range"));
    }
}
