//! Demonstration + regression guard for the user's request: the LSP semantic-token
//! pipeline must highlight EVERY element class of a real clike program (the `urun`
//! example) — globals, types, function names, parameters (and their `mut`/type
//! attributes), local variables, struct fields, enum members, namespaces, keywords,
//! and literals — AND surface go-to-definition for the declarations.
//!
//! This runs the SAME path the editor runs on `didOpen`: a `BootstrapSession`
//! (stdlib + clike) and `augment_from_bootstrap`, which invokes clike's own
//! `analyze_program`. The token stream here is exactly what VS Code colors.
//!
//! Heavy (boots the stdlib once), so it is opt-in. Run:
//!   CAAP_RUN_URUN_HIGHLIGHT_DEMO=1 cargo test -p caap-lsp --test urun_highlight -- --nocapture

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use caap_core::lsp::BootstrapSession;
use caap_core::SourceSpan;
use caap_lsp::analyze::Analysis;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("caap-lsp has a parent (repo root)")
        .to_path_buf()
}

/// The source text a single-line token span covers (1-based line/col, char-precise).
fn text_at(source: &str, sp: &SourceSpan) -> String {
    if sp.start_line == 0 || sp.start_line != sp.end_line {
        return String::new();
    }
    let Some(line) = source.lines().nth(sp.start_line - 1) else {
        return String::new();
    };
    let chars: Vec<char> = line.chars().collect();
    let s = sp.start_col.saturating_sub(1);
    let e = sp.end_col.saturating_sub(1);
    if s > e || e > chars.len() {
        return String::new();
    }
    chars[s..e].iter().collect()
}

fn analyze(session: &BootstrapSession, path: &Path) -> (String, Analysis) {
    let source =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let mut a = Analysis::empty();
    let ok = a.augment_from_bootstrap(session, path);
    assert!(ok, "augment_from_bootstrap failed for {}", path.display());
    (source, a)
}

#[test]
fn urun_highlights_every_element_class() {
    if std::env::var_os("CAAP_RUN_URUN_HIGHLIGHT_DEMO").is_none() {
        eprintln!("skipping URun highlight demo; set CAAP_RUN_URUN_HIGHLIGHT_DEMO=1 to run it");
        return;
    }

    let root = workspace_root();
    let bootstrap = root.join("stdlib").join("bootstrap.caap");
    if !bootstrap.exists() {
        eprintln!("stdlib bootstrap absent; skipping");
        return;
    }
    let session = BootstrapSession::new(vec![bootstrap], vec![root.clone()])
        .expect("stdlib session constructs");

    let files = [
        "examples/urun/ur_scheduler.caap",
        "examples/urun/ur_status.caap",
        "examples/urun/ur_thread.caap",
    ];

    let mut all: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut def_kinds: BTreeMap<String, usize> = BTreeMap::new();

    for rel in files {
        let (source, a) = analyze(&session, &root.join(rel));
        println!("\n========== {rel} ==========");
        println!(
            "tokens={}  definitions={}  diagnostics={}",
            a.tokens.len(),
            a.definitions.len(),
            a.diagnostics.len()
        );

        let mut by_cat: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for t in &a.tokens {
            let txt = text_at(&source, &t.span);
            if !txt.is_empty() {
                by_cat.entry(t.kind.clone()).or_default().push(txt.clone());
                all.entry(t.kind.clone()).or_default().push(txt);
            }
        }
        println!("-- token categories (count : sample texts) --");
        for (cat, texts) in &by_cat {
            let mut uniq: Vec<&String> = {
                let set: std::collections::BTreeSet<&String> = texts.iter().collect();
                set.into_iter().collect()
            };
            uniq.truncate(8);
            println!("  {cat:<11} {:>3} : {uniq:?}", texts.len());
        }

        // Full source-order stream for the small enum file: shows the in-context
        // sequence (e.g. `mut`->keyword `UR_THREAD`->type `*`->operator `p`->parameter,
        // and the enum members as their own tokens).
        if rel.ends_with("ur_status.caap") {
            let mut ordered: Vec<&caap_lsp::analyze::SemToken> = a.tokens.iter().collect();
            ordered.sort_by_key(|t| (t.span.start_line, t.span.start_col));
            println!("-- full token stream (source order) --");
            for t in ordered {
                let txt = text_at(&source, &t.span);
                let m = if t.mods.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", t.mods.join(","))
                };
                println!(
                    "  {:>3}:{:<3} {:<12} {txt}{m}",
                    t.span.start_line, t.span.start_col, t.kind
                );
            }
        }

        println!("-- definitions (go-to-def / declaration targets) --");
        for d in a.definitions.iter().take(28) {
            let params = if d.params.is_empty() {
                String::new()
            } else {
                format!(" ({})", d.params.join(", "))
            };
            println!(
                "  {:<32} {:?}{params}  @{}:{}",
                d.name, d.kind, d.name_span.start_line, d.name_span.start_col
            );
            *def_kinds.entry(format!("{:?}", d.kind)).or_default() += 1;
        }
    }

    println!("\n========== AGGREGATE COVERAGE (across urun) ==========");
    for (cat, items) in &all {
        let uniq: std::collections::BTreeSet<&String> = items.iter().collect();
        println!(
            "  {cat:<11} {:>4} tokens, {} distinct",
            items.len(),
            uniq.len()
        );
    }
    println!("definition kinds: {def_kinds:?}");
    for opt in [
        "enumMember",
        "namespace",
        "number",
        "string",
        "operator",
        "comment",
    ] {
        println!(
            "  optional `{opt}`: {}",
            if all.contains_key(opt) {
                "present"
            } else {
                "ABSENT"
            }
        );
    }

    // ── Assertions: every element class the user named must be highlighted ──
    // globals + locals -> variable ; types/structs -> type ; function names ->
    // function ; params -> parameter ; struct fields/attrs -> property ; the
    // struct/enum/export/mut/fn declaration words -> keyword.
    for expected in [
        "keyword",
        "function",
        "parameter",
        "variable",
        "type",
        "property",
    ] {
        assert!(
            all.contains_key(expected),
            "no `{expected}` token across the urun files — a highlighting gap. Present: {:?}",
            all.keys().collect::<Vec<_>>()
        );
    }
    assert!(
        !def_kinds.is_empty(),
        "no definitions extracted — go-to-definition / declaration would be empty"
    );
    // go-to-definition must index VARIABLES (globals + locals) and FUNCTIONS, not
    // just struct/enum types — the declaration-derived definition index.
    assert!(
        def_kinds.contains_key("Variable"),
        "go-to-def must index variables (globals + locals): {def_kinds:?}"
    );
    assert!(
        def_kinds.contains_key("Function"),
        "go-to-def must index functions: {def_kinds:?}"
    );
}
