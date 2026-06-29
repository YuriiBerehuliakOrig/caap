//! Filesystem host policy.
//!
//! The filesystem *operations* live in `caap_sys_runtime::fs`; this module holds
//! only the caap-side sandbox policy: it normalizes a requested path to an
//! absolute, lexically-canonical form and confines it to the configured
//! read/write roots before the operation runs. See [`super::sys_policy`].

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::values::{eval_err, EvalSignal};

use super::fn_misc::path_to_string;
use super::HostSystemPolicy;

thread_local! {
    /// Memo of canonicalized policy roots, keyed by the lexically-normalized
    /// configured root. Policy roots are a small, fixed set reused across every
    /// filesystem operation, so canonicalizing each distinct root once (instead
    /// of on every check) avoids an `O(roots * ops)` flood of `canonicalize`
    /// syscalls. The `canonicalize` *result* is memoized so an unresolvable root
    /// keeps erroring exactly as the uncached path did. The request path is
    /// per-call and intentionally not memoized.
    static CANONICAL_ROOT_CACHE: RefCell<HashMap<PathBuf, Result<PathBuf, String>>> =
        RefCell::new(HashMap::new());
}

// ---------------------------------------------------------------------------
// Policy path helpers
// ---------------------------------------------------------------------------

/// Normalize, sandbox-check, and return a filesystem path argument.
///
/// `verb` is `"read"` or `"write"`; the path is normalized to an absolute,
/// lexically-canonical form and confined to the matching policy roots. The
/// returned string is the normalized path that the sys-runtime operation should
/// act on, so the interpreter and a compiled binary observe the same path.
pub(super) fn authorize_fs_path(
    policy: &HostSystemPolicy,
    raw: &str,
    verb: &str,
    context: &str,
) -> Result<String, EvalSignal> {
    if raw.is_empty() {
        return Err(eval_err("filesystem paths must be non-empty strings"));
    }
    let path = host_policy_path(raw, context)?;
    let roots = match verb {
        "read" => policy.fs.read_roots.as_ref(),
        "write" => policy.fs.write_roots.as_ref(),
        _ => {
            return Err(eval_err(format!(
                "{context}: unsupported filesystem policy verb"
            )))
        }
    };
    enforce_roots(&path, roots, verb, context)?;
    path_to_string(path)
}

fn host_policy_path(path: &str, context: &str) -> Result<PathBuf, EvalSignal> {
    let path = Path::new(path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| eval_err(format!("{context}: {error}")))?
            .join(path)
    };
    normalize_path_lexically(&absolute)
}

fn normalize_path_lexically(path: &Path) -> Result<PathBuf, EvalSignal> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    if normalized.as_os_str().is_empty() {
        return Err(eval_err("filesystem paths must be non-empty strings"));
    }
    Ok(normalized)
}

fn enforce_roots(
    path: &Path,
    roots: Option<&Vec<PathBuf>>,
    verb: &str,
    context: &str,
) -> Result<(), EvalSignal> {
    let Some(roots) = roots else {
        return Ok(());
    };
    if roots.is_empty() {
        return Err(eval_err(format!(
            "compile-time {verb} access is not allowed for {}",
            path.display()
        )));
    }
    let checked_path = canonical_policy_path(path, verb, context)?;
    for root in roots {
        let root = canonical_policy_root(root, context)?;
        if checked_path.starts_with(&root) {
            return Ok(());
        }
    }
    let rendered_roots = roots
        .iter()
        .map(|root| root.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(eval_err(format!(
        "{context}: compile-time {verb} access to {} is outside allowed roots: {rendered_roots}",
        path.display()
    )))
}

fn canonical_policy_root(root: &Path, context: &str) -> Result<PathBuf, EvalSignal> {
    let root = normalize_path_lexically(root)?;
    let cached = CANONICAL_ROOT_CACHE.with(|cache| {
        if let Some(result) = cache.borrow().get(&root) {
            return result.clone();
        }
        let result = std::fs::canonicalize(&root).map_err(|error| {
            format!(
                "filesystem policy root {} cannot be canonicalized: {error}",
                root.display()
            )
        });
        cache.borrow_mut().insert(root.clone(), result.clone());
        result
    });
    cached.map_err(|message| eval_err(format!("{context}: {message}")))
}

fn canonical_policy_path(path: &Path, verb: &str, context: &str) -> Result<PathBuf, EvalSignal> {
    if path.exists() {
        return std::fs::canonicalize(path).map_err(|error| {
            eval_err(format!(
                "{context}: compile-time {verb} path {} cannot be canonicalized: {error}",
                path.display()
            ))
        });
    }
    let parent = nearest_existing_parent(path).ok_or_else(|| {
        eval_err(format!(
            "{context}: compile-time {verb} path {} has no existing parent",
            path.display()
        ))
    })?;
    std::fs::canonicalize(parent).map_err(|error| {
        eval_err(format!(
            "{context}: compile-time {verb} parent {} cannot be canonicalized: {error}",
            parent.display()
        ))
    })
}

fn nearest_existing_parent(path: &Path) -> Option<&Path> {
    let mut current = path.parent();
    while let Some(parent) = current {
        if parent.exists() {
            return Some(parent);
        }
        current = parent.parent();
    }
    None
}
