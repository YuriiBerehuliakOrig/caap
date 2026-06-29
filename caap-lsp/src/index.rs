//! A lazily-built, workspace-wide index of top-level definitions, backing
//! `workspace/symbol`. Each `.caap` file under the workspace roots is parsed
//! with the base s-expr parser and its definitions collected — fast (no
//! evaluation). Grammar-extended files contribute only their parseable header.

use std::path::{Path, PathBuf};

use caap_core::SourceSpan;
use lsp_types::Range;

use crate::analyze::{Analysis, DefinitionKind};
use crate::symbols::occurrences;

pub struct IndexedSymbol {
    pub name: String,
    pub kind: DefinitionKind,
    pub path: PathBuf,
    pub name_span: SourceSpan,
}

/// Walk the workspace `.caap` files and collect every whole-token occurrence of
/// `word`, returning `(file, range)` pairs. `skip` (the file being edited) is
/// excluded so the caller can use its fresh in-memory text instead of disk.
/// Capped to keep references/rename bounded.
pub fn workspace_occurrences(
    roots: &[PathBuf],
    word: &str,
    skip: Option<&Path>,
) -> Vec<(PathBuf, Range)> {
    const LIMIT: usize = 5000;
    let mut out = Vec::new();
    let skip_canon = skip.and_then(|p| std::fs::canonicalize(p).ok());
    for root in roots {
        collect_occurrences(root, word, skip_canon.as_deref(), &mut out, 0, LIMIT);
    }
    out
}

fn collect_occurrences(
    dir: &Path,
    word: &str,
    skip: Option<&Path>,
    out: &mut Vec<(PathBuf, Range)>,
    depth: usize,
    limit: usize,
) {
    if depth > 32 || out.len() >= limit {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if out.len() >= limit {
            return;
        }
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                "target" | "node_modules" | ".git" | ".caap_build"
            ) {
                continue;
            }
            collect_occurrences(&path, word, skip, out, depth + 1, limit);
        } else if path.extension().and_then(|e| e.to_str()) == Some("caap") {
            if skip.is_some_and(|s| std::fs::canonicalize(&path).ok().as_deref() == Some(s)) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            for range in occurrences(&text, word) {
                out.push((path.clone(), range));
                if out.len() >= limit {
                    return;
                }
            }
        }
    }
}

/// Every `.caap` file under the workspace roots (skipping build/vcs dirs).
/// Backs the cross-file call-graph cache, which augments grammar-extended files
/// individually rather than re-walking them in one pass.
pub fn workspace_caap_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        collect_caap_files(root, &mut out, 0);
    }
    out
}

fn collect_caap_files(dir: &Path, out: &mut Vec<PathBuf>, depth: usize) {
    if depth > 32 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                "target" | "node_modules" | ".git" | ".caap_build"
            ) {
                continue;
            }
            collect_caap_files(&path, out, depth + 1);
        } else if path.extension().and_then(|e| e.to_str()) == Some("caap") {
            out.push(path);
        }
    }
}

/// Walk the workspace roots and collect every top-level definition.
pub fn build_workspace_index(roots: &[PathBuf]) -> Vec<IndexedSymbol> {
    let mut out = Vec::new();
    for root in roots {
        collect(root, &mut out, 0);
    }
    out
}

fn collect(dir: &Path, out: &mut Vec<IndexedSymbol>, depth: usize) {
    if depth > 32 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                "target" | "node_modules" | ".git" | ".caap_build"
            ) {
                continue;
            }
            collect(&path, out, depth + 1);
        } else if path.extension().and_then(|e| e.to_str()) == Some("caap") {
            index_file(&path, out);
        }
    }
}

fn index_file(path: &Path, out: &mut Vec<IndexedSymbol>) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Some(path_str) = path.to_str() else {
        return;
    };
    // Plain s-expr parse when possible; otherwise just the recoverable header.
    let analysis = Analysis::from_source(path_str, &text)
        .unwrap_or_else(|_| Analysis::from_leading_forms(path_str, &text));
    for def in analysis.definitions {
        out.push(IndexedSymbol {
            name: def.name,
            kind: def.kind,
            path: path.to_path_buf(),
            name_span: def.name_span,
        });
    }
}
