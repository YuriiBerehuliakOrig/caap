use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::Path;

use crate::ffi_value::{SysArgs, SysError, SysResult, SysValue};

pub fn invoke(name: &str, args: SysArgs) -> SysResult {
    match name {
        "env_get" => {
            let key = args.require_str(0, "os.env_get")?;
            match std::env::var(key.as_str()) {
                Ok(value) => Ok(SysValue::Str(value)),
                Err(std::env::VarError::NotPresent) => Ok(SysValue::Null),
                Err(error) => Err(SysError::invalid_argument(format!("os.env_get: {error}"))),
            }
        }
        "env_has" => {
            let key = args.require_str(0, "os.env_has")?;
            Ok(SysValue::Bool(std::env::var_os(key.as_str()).is_some()))
        }
        "env_keys" => {
            let mut keys = std::env::vars_os()
                .map(|(key, _)| os_str_to_string(&key, "os.env-keys key").map(SysValue::Str))
                .collect::<Result<Vec<_>, _>>()?;
            keys.sort_by_key(|value| match value {
                SysValue::Str(value) => value.clone(),
                _ => String::new(),
            });
            Ok(SysValue::List(keys))
        }
        "env_vars" => {
            let mut map = HashMap::new();
            for (key, value) in std::env::vars_os() {
                let key = os_str_to_string(&key, "os.env-vars key")?;
                let value = os_str_to_string(&value, "os.env-vars value")?;
                map.insert(key, SysValue::Str(value));
            }
            Ok(SysValue::Map(map))
        }
        "getcwd" => {
            let cwd = std::env::current_dir().map_err(|e| SysError::from_io("os.getcwd", e))?;
            Ok(SysValue::Str(path_to_string(&cwd, "os.getcwd")?))
        }
        "current_exe" => {
            let exe =
                std::env::current_exe().map_err(|e| SysError::from_io("os.current_exe", e))?;
            Ok(SysValue::Str(path_to_string(&exe, "os.current_exe")?))
        }
        "temp_dir" => {
            let dir = std::env::temp_dir();
            Ok(SysValue::Str(path_to_string(&dir, "os.temp_dir")?))
        }
        "hostname" => hostname(),
        "platform" => Ok(SysValue::Str(std::env::consts::OS.to_string())),
        "arch" => Ok(SysValue::Str(std::env::consts::ARCH.to_string())),
        "family" => Ok(SysValue::Str(std::env::consts::FAMILY.to_string())),
        "available_parallelism" => {
            let n = std::thread::available_parallelism()
                .map_err(|e| SysError::from_io("os.available_parallelism", e))?;
            i64::try_from(n.get()).map(SysValue::Int).map_err(|_| {
                SysError::invalid_argument(
                    "os.available_parallelism: value exceeds CAAP SYS int range",
                )
            })
        }
        "set_current_dir" => {
            let dir = args.require_str(0, "os.set_current_dir")?;
            std::env::set_current_dir(&dir)
                .map_err(|e| SysError::from_io("os.set_current_dir", e))?;
            Ok(SysValue::Null)
        }
        "set_env" => {
            let key = args.require_str(0, "os.set_env")?;
            let value = args.require_str(1, "os.set_env")?;
            if key.is_empty() || key.contains('=') || key.contains('\0') {
                return Err(SysError::invalid_argument(
                    "os.set_env: name must be non-empty and contain no '=' or NUL",
                ));
            }
            if value.contains('\0') {
                return Err(SysError::invalid_argument(
                    "os.set_env: value must contain no NUL",
                ));
            }
            // Process-global mutation; the interpreter/CLI is single-threaded.
            std::env::set_var(&key, &value);
            Ok(SysValue::Null)
        }
        "remove_env" => {
            let key = args.require_str(0, "os.remove_env")?;
            if key.is_empty() || key.contains('=') || key.contains('\0') {
                return Err(SysError::invalid_argument(
                    "os.remove_env: name must be non-empty and contain no '=' or NUL",
                ));
            }
            std::env::remove_var(&key);
            Ok(SysValue::Null)
        }
        _ => Err(format!("os: unknown export '{name}'").into()),
    }
}

/// The system hostname, via `gethostname(2)`. `std` exposes no portable hostname
/// API, so this reads it through libc on unix and is unsupported elsewhere.
#[cfg(unix)]
fn hostname() -> SysResult {
    // HOST_NAME_MAX is 64 on Linux; 256 leaves ample room and a guaranteed NUL.
    let mut buffer = vec![0u8; 256];
    // SAFETY: `buffer` is valid for `buffer.len()` bytes; `gethostname` writes at
    // most that many and (on success, with room to spare) NUL-terminates.
    let rc = unsafe { libc::gethostname(buffer.as_mut_ptr() as *mut libc::c_char, buffer.len()) };
    if rc != 0 {
        return Err(SysError::from_io(
            "os.hostname",
            std::io::Error::last_os_error(),
        ));
    }
    let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
    buffer.truncate(end);
    let name = String::from_utf8(buffer)
        .map_err(|e| SysError::invalid_argument(format!("os.hostname: {e}")))?;
    Ok(SysValue::Str(name))
}

#[cfg(not(unix))]
fn hostname() -> SysResult {
    Err(SysError::unsupported(
        "os.hostname: not supported on this platform",
    ))
}

fn path_to_string(path: &Path, ctx: &str) -> Result<String, SysError> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| SysError::invalid_argument(format!("{ctx}: path is not valid UTF-8")))
}

fn os_str_to_string(value: &OsStr, ctx: &str) -> Result<String, SysError> {
    value
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| SysError::invalid_argument(format!("{ctx}: value is not valid UTF-8")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_path_exports_use_explicit_utf8_boundary() {
        for export in ["getcwd", "current_exe", "temp_dir"] {
            let value = invoke(export, SysArgs(Vec::new())).unwrap();
            let SysValue::Str(path) = value else {
                panic!("expected os.{export} string path");
            };
            assert!(!path.is_empty());
        }
    }

    #[test]
    fn os_platform_and_arch_use_std_env_constants() {
        assert_eq!(
            invoke("platform", SysArgs(Vec::new())).unwrap(),
            SysValue::Str(std::env::consts::OS.to_string())
        );
        assert_eq!(
            invoke("arch", SysArgs(Vec::new())).unwrap(),
            SysValue::Str(std::env::consts::ARCH.to_string())
        );
    }

    #[test]
    fn family_and_parallelism_report_portable_values() {
        let SysValue::Str(family) = invoke("family", SysArgs(Vec::new())).unwrap() else {
            panic!("expected family string");
        };
        assert!(family == "unix" || family == "windows" || family == "wasm" || !family.is_empty());
        let SysValue::Int(n) = invoke("available_parallelism", SysArgs(Vec::new())).unwrap() else {
            panic!("expected parallelism int");
        };
        assert!(n >= 1);
    }

    #[test]
    fn set_and_remove_env_round_trip() {
        let key = format!("CAAP_TEST_SET_ENV_{}", std::process::id());
        invoke(
            "set_env",
            SysArgs(vec![
                SysValue::Str(key.clone()),
                SysValue::Str("on".to_string()),
            ]),
        )
        .unwrap();
        assert_eq!(
            invoke("env_get", SysArgs(vec![SysValue::Str(key.clone())])).unwrap(),
            SysValue::Str("on".to_string())
        );
        invoke("remove_env", SysArgs(vec![SysValue::Str(key.clone())])).unwrap();
        assert_eq!(
            invoke("env_get", SysArgs(vec![SysValue::Str(key)])).unwrap(),
            SysValue::Null
        );
    }

    #[test]
    fn set_env_rejects_malformed_names() {
        let error = invoke(
            "set_env",
            SysArgs(vec![
                SysValue::Str("BAD=NAME".to_string()),
                SysValue::Str("x".to_string()),
            ]),
        )
        .unwrap_err();
        assert!(error.contains("must be non-empty and contain no"));
    }

    #[test]
    fn set_current_dir_round_trips_through_temp() {
        let original = std::env::current_dir().unwrap();
        let temp = std::env::temp_dir();
        invoke(
            "set_current_dir",
            SysArgs(vec![SysValue::Str(temp.to_str().unwrap().to_string())]),
        )
        .unwrap();
        // Restore immediately to avoid disturbing other tests.
        std::env::set_current_dir(&original).unwrap();
    }

    #[test]
    fn env_get_returns_null_only_for_missing_values() {
        let missing = format!("CAAP_TEST_MISSING_ENV_{}", std::process::id());
        std::env::remove_var(&missing);
        assert_eq!(
            invoke("env_get", SysArgs(vec![SysValue::Str(missing)])).unwrap(),
            SysValue::Null
        );
    }

    #[cfg(unix)]
    #[test]
    fn os_string_projection_rejects_non_utf8_values() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let invalid = OsString::from_vec(b"bad-\xFF".to_vec());
        let error = os_str_to_string(&invalid, "os.test").unwrap_err();
        assert!(error.contains("not valid UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn hostname_returns_a_non_empty_name() {
        let SysValue::Str(name) = invoke("hostname", SysArgs(Vec::new())).unwrap() else {
            panic!("expected hostname string");
        };
        assert!(!name.is_empty());
        // gethostname must not leak embedded NULs or trailing padding.
        assert!(!name.contains('\0'));
        assert_eq!(name, name.trim());
    }
}
