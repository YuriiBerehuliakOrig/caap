//! Guards `docs/stdlib-reference.md` against silent drift (audit
//! 2026-06-19-repository-stdlib-maintainability-analysis, finding #1).
//!
//! Two checks, both chosen to NOT over-flag — the reference is a CURATED
//! public-API map, so we deliberately do NOT require every export to be
//! documented (that would force ~150 internal/sys symbols into the doc):
//!   1. every intra-repo link in the reference resolves to a real file
//!      (this is what caught the stale `kits/README.md` link);
//!   2. every export SYMBOL the reference documents for a stdlib module is
//!      actually `(export …)`-ed by that module (this is what caught the stale
//!      `alias` row that listed `find`/`unify`/… long after they were dropped).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("workspace root")
        .to_path_buf()
}

fn collect_caap(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_dir() {
                collect_caap(&p, out);
            } else if p.extension().is_some_and(|e| e == "caap") {
                out.push(p);
            }
        }
    }
}

/// `(module NAME)` -> the union of every `(export sym …)` in that file.
fn module_exports(root: &Path) -> HashMap<String, HashSet<String>> {
    let mut files = Vec::new();
    collect_caap(&root.join("stdlib"), &mut files);
    let mut map: HashMap<String, HashSet<String>> = HashMap::new();
    for f in files {
        let src = std::fs::read_to_string(&f).unwrap_or_default();
        let Some(module) = src
            .split("(module ")
            .nth(1)
            .and_then(|t| t.split(|c: char| c == ')' || c.is_whitespace()).next())
            .map(str::to_string)
        else {
            continue;
        };
        let entry = map.entry(module).or_default();
        let mut rest = src.as_str();
        while let Some(idx) = rest.find("(export ") {
            rest = &rest[idx + "(export ".len()..];
            if let Some(end) = rest.find(')') {
                for sym in rest[..end].split_whitespace() {
                    entry.insert(sym.to_string());
                }
            }
        }
    }
    map
}

#[test]
fn stdlib_reference_links_resolve() {
    let root = repo_root();
    let doc =
        std::fs::read_to_string(root.join("docs/stdlib-reference.md")).expect("read reference");
    let mut broken = Vec::new();
    for after in doc.split("](").skip(1) {
        let link: String = after
            .chars()
            .take_while(|&c| c != ')' && c != '#')
            .collect();
        if !link.starts_with("..") {
            continue; // external (http) or in-page anchor — not our concern
        }
        if !root.join("docs").join(&link).exists() {
            broken.push(link);
        }
    }
    assert!(
        broken.is_empty(),
        "docs/stdlib-reference.md has broken intra-repo links: {broken:?}"
    );
}

#[test]
fn stdlib_reference_documents_only_real_exports() {
    let root = repo_root();
    let exports = module_exports(&root);
    let doc =
        std::fs::read_to_string(root.join("docs/stdlib-reference.md")).expect("read reference");
    let mut stale = Vec::new();
    for line in doc.lines() {
        let line = line.trim_start();
        // a module table row: | `stdlib.x.y` | `sym`, `sym`, … | …
        if !line.starts_with("| `stdlib.") {
            continue;
        }
        let cols: Vec<&str> = line.split('|').collect();
        if cols.len() < 3 {
            continue;
        }
        let module = cols[1].trim().trim_matches('`');
        let Some(actual) = exports.get(module) else {
            continue; // a module the source scan didn't see — skip rather than guess
        };
        // backtick tokens in the EXPORTS column only (col 2), never the Purpose column
        let parts: Vec<&str> = cols[2].split('`').collect();
        let mut k = 1;
        while k < parts.len() {
            let sym = parts[k].trim();
            // a real export name: no spaces / slashes / dots (those are prose or paths)
            let looks_like_symbol =
                !sym.is_empty() && !sym.contains(' ') && !sym.contains('/') && !sym.contains('.');
            if looks_like_symbol && !actual.contains(sym) {
                stale.push(format!(
                    "{module}: documents `{sym}` which it does not export"
                ));
            }
            k += 2;
        }
    }
    assert!(
        stale.is_empty(),
        "docs/stdlib-reference.md documents exports that no longer exist:\n{}",
        stale.join("\n")
    );
}
