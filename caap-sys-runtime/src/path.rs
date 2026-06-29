use std::path::{Component, Path, PathBuf};

use crate::ffi_value::{SysArgs, SysError, SysResult, SysValue};

pub fn invoke(name: &str, args: SysArgs) -> SysResult {
    match name {
        "join" => {
            let mut p = std::path::PathBuf::new();
            for arg in args.iter() {
                match arg {
                    SysValue::Str(s) => p.push(s.as_str()),
                    _ => return Err("path.join: all arguments must be strings".into()),
                }
            }
            Ok(SysValue::Str(path_to_string(&p, "path.join")?))
        }
        "basename" => {
            let s = args.require_str(0, "path.basename")?;
            let path = Path::new(s.as_str());
            let name = path
                .file_name()
                .ok_or_else(|| format!("path.basename: path has no final component: {s:?}"))?;
            let name = path_component_to_string(name, "path.basename")?;
            Ok(SysValue::Str(name))
        }
        "dirname" => {
            let s = args.require_str(0, "path.dirname")?;
            let path = Path::new(s.as_str());
            let parent = match path.parent() {
                Some(parent) if parent.as_os_str().is_empty() => ".".to_string(),
                Some(parent) => path_to_string(parent, "path.dirname")?,
                None if path.has_root() => path_to_string(path, "path.dirname")?,
                None => {
                    return Err(SysError::invalid_argument(format!(
                        "path.dirname: path has no parent: {s:?}"
                    )))
                }
            };
            Ok(SysValue::Str(parent))
        }
        "extension" => {
            let s = args.require_str(0, "path.extension")?;
            match Path::new(s.as_str()).extension() {
                Some(ext) => Ok(SysValue::Str(path_component_to_string(
                    ext,
                    "path.extension",
                )?)),
                None => Ok(SysValue::Null),
            }
        }
        "stem" => {
            let s = args.require_str(0, "path.stem")?;
            let stem = Path::new(s.as_str())
                .file_stem()
                .ok_or_else(|| format!("path.stem: path has no final component: {s:?}"))?;
            Ok(SysValue::Str(path_component_to_string(stem, "path.stem")?))
        }
        "with_extension" => {
            let s = args.require_str(0, "path.with_extension")?;
            let ext = args.require_str(1, "path.with_extension")?;
            let result = Path::new(s.as_str()).with_extension(ext.as_str());
            Ok(SysValue::Str(path_to_string(
                &result,
                "path.with_extension",
            )?))
        }
        "is_absolute" => {
            let s = args.require_str(0, "path.is_absolute")?;
            Ok(SysValue::Bool(Path::new(s.as_str()).is_absolute()))
        }
        "normalize" => {
            let s = args.require_str(0, "path.normalize")?;
            Ok(SysValue::Str(normalize_lexically(s.as_str())))
        }
        "split" => {
            let s = args.require_str(0, "path.split")?;
            let mut parts = Vec::new();
            for component in Path::new(s.as_str()).components() {
                parts.push(SysValue::Str(path_component_to_string(
                    component.as_os_str(),
                    "path.split",
                )?));
            }
            Ok(SysValue::List(parts))
        }
        "strip_prefix" => {
            let s = args.require_str(0, "path.strip_prefix")?;
            let prefix = args.require_str(1, "path.strip_prefix")?;
            match Path::new(s.as_str()).strip_prefix(prefix.as_str()) {
                Ok(rest) => Ok(SysValue::Str(path_to_string(rest, "path.strip_prefix")?)),
                Err(_) => Ok(SysValue::Null),
            }
        }
        _ => Err(format!("path: unknown export '{name}'").into()),
    }
}

/// Lexical normalization: collapse `.` and `..` components without touching the
/// filesystem. Purely textual and platform-aware via `std::path::Component`.
fn normalize_lexically(path: &str) -> String {
    let mut normalized = PathBuf::new();
    let mut had_component = false;
    for component in Path::new(path).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop a real segment; otherwise preserve `..` (e.g. relative paths
                // that ascend above their start, or after a root where it is a no-op).
                if matches!(
                    normalized.components().next_back(),
                    Some(Component::Normal(_))
                ) {
                    normalized.pop();
                } else if !normalized.has_root() {
                    normalized.push("..");
                }
                had_component = true;
            }
            other => {
                normalized.push(other.as_os_str());
                had_component = true;
            }
        }
    }
    if !had_component || normalized.as_os_str().is_empty() {
        return ".".to_string();
    }
    // Paths are UTF-8 by contract; components came from a &str so this is lossless.
    normalized.to_string_lossy().into_owned()
}

fn path_component_to_string(component: &std::ffi::OsStr, ctx: &str) -> Result<String, String> {
    component
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{ctx}: path component is not valid UTF-8"))
}

fn path_to_string(path: &Path, ctx: &str) -> Result<String, String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{ctx}: path is not valid UTF-8"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, path: &str) -> SysResult {
        invoke(name, SysArgs(vec![SysValue::Str(path.to_string())]))
    }

    #[test]
    fn join_uses_explicit_utf8_path_boundary() {
        assert_eq!(
            invoke(
                "join",
                SysArgs(vec![
                    SysValue::Str("tmp".to_string()),
                    SysValue::Str("demo.caap".to_string()),
                ]),
            )
            .unwrap(),
            SysValue::Str("tmp/demo.caap".to_string())
        );
    }

    #[test]
    fn basename_rejects_paths_without_final_component() {
        assert_eq!(
            call("basename", "/tmp/demo.caap").unwrap(),
            SysValue::Str("demo.caap".to_string())
        );
        assert!(call("basename", "/")
            .unwrap_err()
            .contains("path has no final component"));
        assert!(call("basename", "")
            .unwrap_err()
            .contains("path has no final component"));
    }

    fn call2(name: &str, a: &str, b: &str) -> SysResult {
        invoke(
            name,
            SysArgs(vec![
                SysValue::Str(a.to_string()),
                SysValue::Str(b.to_string()),
            ]),
        )
    }

    #[test]
    fn extension_and_stem_split_final_component() {
        assert_eq!(
            call("extension", "/tmp/demo.caap").unwrap(),
            SysValue::Str("caap".to_string())
        );
        assert_eq!(call("extension", "/tmp/demo").unwrap(), SysValue::Null);
        assert_eq!(
            call("stem", "/tmp/demo.caap").unwrap(),
            SysValue::Str("demo".to_string())
        );
    }

    #[test]
    fn with_extension_replaces_suffix() {
        assert_eq!(
            call2("with_extension", "/tmp/demo.caap", "txt").unwrap(),
            SysValue::Str("/tmp/demo.txt".to_string())
        );
    }

    #[test]
    fn is_absolute_reports_root() {
        assert_eq!(call("is_absolute", "/tmp").unwrap(), SysValue::Bool(true));
        assert_eq!(call("is_absolute", "tmp/x").unwrap(), SysValue::Bool(false));
    }

    #[test]
    fn normalize_collapses_dot_and_dotdot_lexically() {
        assert_eq!(
            call("normalize", "/a/./b/../c").unwrap(),
            SysValue::Str("/a/c".to_string())
        );
        assert_eq!(
            call("normalize", "a/b/../../c").unwrap(),
            SysValue::Str("c".to_string())
        );
        assert_eq!(
            call("normalize", "./").unwrap(),
            SysValue::Str(".".to_string())
        );
        // Ascending above a relative start is preserved (no FS access).
        assert_eq!(
            call("normalize", "../x").unwrap(),
            SysValue::Str("../x".to_string())
        );
    }

    #[test]
    fn split_yields_components() {
        let SysValue::List(parts) = call("split", "/tmp/demo.caap").unwrap() else {
            panic!("expected list");
        };
        assert_eq!(
            parts,
            vec![
                SysValue::Str("/".to_string()),
                SysValue::Str("tmp".to_string()),
                SysValue::Str("demo.caap".to_string()),
            ]
        );
    }

    #[test]
    fn strip_prefix_returns_relative_or_null() {
        assert_eq!(
            call2("strip_prefix", "/a/b/c", "/a").unwrap(),
            SysValue::Str("b/c".to_string())
        );
        assert_eq!(call2("strip_prefix", "/a/b", "/x").unwrap(), SysValue::Null);
    }

    #[test]
    fn dirname_uses_explicit_lexical_parent_semantics() {
        assert_eq!(
            call("dirname", "/tmp/demo.caap").unwrap(),
            SysValue::Str("/tmp".to_string())
        );
        assert_eq!(
            call("dirname", "demo.caap").unwrap(),
            SysValue::Str(".".to_string())
        );
        assert_eq!(
            call("dirname", "/").unwrap(),
            SysValue::Str("/".to_string())
        );
        assert!(call("dirname", "")
            .unwrap_err()
            .contains("path has no parent"));
    }
}
