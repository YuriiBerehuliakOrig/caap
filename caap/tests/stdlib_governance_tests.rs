//! Mechanical governance lints over the stdlib tree (audit "Quick Wins").
//!
//! Two independent checks that a coordinator can rely on as a regression gate:
//!   (A) the in-language tier/layering checker (`stdlib.semantics.passes.tiers`
//!       `check_file`) reports ZERO findings across every stdlib module — the
//!       tower must not invert. The checker already whitelists the documented
//!       intentional upward edges, so a clean tree yields nothing.
//!   (B) a pure-Rust lint pinning the set of files allowed to call
//!       `ctfe_compiler_load_surface_file_template` directly — a new call site
//!       outside the documented allowlist is a layering-bypass regression.
use caap_core::RuntimeValue;

mod common;

use common::{eval_ok, stdlib_path, with_stdlib_root};

/// The tier directories enumerated by the corpus check. A module may import only
/// its own tier or a lower one; `tiers.caap` enforces this. We sweep every
/// `.caap` under these dirs — EXCLUDING `stdlib/lib/tests` (the in-language test
/// corpus / fixtures, which are not library modules and may bend the rules).
const TIER_DIRS: &[&str] = &[
    "lib",
    "boot",
    "syntax",
    "semantics",
    "frontend",
    "backend",
    "storage",
    "sys",
    "bare",
];

/// Repo-relative path of a stdlib file, e.g. `stdlib/frontend/surface.caap`.
fn repo_relative(path: &std::path::Path) -> String {
    let stdlib_root = std::path::PathBuf::from(stdlib_path(""));
    // `stdlib_path("")` yields `.../stdlib/`; its parent is the repo root used
    // for the documented `stdlib/...` spelling.
    let repo_root = stdlib_root
        .parent()
        .expect("stdlib has a parent (repo root)");
    path.strip_prefix(repo_root)
        .map(|p| p.display().to_string().replace('\\', "/"))
        .unwrap_or_else(|_| path.display().to_string())
}

/// Walk `stdlib/<dir>` for every `*.caap`, skipping the `lib/tests` corpus.
fn collect_tier_caap_files() -> Vec<std::path::PathBuf> {
    let tests_dir = std::path::PathBuf::from(stdlib_path("lib/tests"));
    let mut found = Vec::new();
    for dir in TIER_DIRS {
        let root = std::path::PathBuf::from(stdlib_path(dir));
        if !root.exists() {
            continue;
        }
        let mut stack = vec![root];
        while let Some(d) = stack.pop() {
            // Exclude the in-language test/fixture corpus wholesale.
            if d == tests_dir {
                continue;
            }
            for entry in std::fs::read_dir(&d).expect("read stdlib tier dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().and_then(|e| e.to_str()) == Some("caap") {
                    found.push(path);
                }
            }
        }
    }
    found.sort();
    found
}

/// (A) The tier checker must report ZERO layering findings across the whole
/// stdlib corpus. Marked `acceptance` because it bootstraps the stdlib and runs
/// the in-language `check_file` per module.
///
/// On a violation this does NOT pass vacuously: the CAAP body annotates every
/// finding with its file and returns the flat list, and the assertion prints the
/// exact finding set before failing — so real upward edges surface for the
/// coordinator. `tiers.caap` already whitelists the legitimate exceptions
/// (`stdlib.semantics.passes.derive -> types`, `stdlib.analyze -> lib`), so a
/// clean tree is empty.
#[test]
#[ignore = "acceptance: tiers check over all stdlib modules"]
fn stdlib_tiers_no_violations_in_corpus() {
    let files = collect_tier_caap_files();
    assert!(
        files.len() >= 50,
        "expected the stdlib corpus to be enumerated; found only {} files",
        files.len()
    );

    // Build a CAAP list literal of (repo_relative, absolute) path pairs. The body
    // runs `check_file` per absolute path and, for each non-empty finding,
    // prepends the repo-relative path so the Rust side can report a clear set.
    let mut pairs = String::new();
    for path in &files {
        let abs = path.display().to_string();
        let rel = repo_relative(path);
        pairs.push_str(&format!("(list_of {rel:?} {abs:?})\n"));
    }

    // NB: the body is RAW kernel code (`eval_ok` evaluates without the expander),
    // so it uses kernel iteration builtins (`sequence_each`) rather than the
    // stdlib `for` macro. Each finding is prefixed with its repo-relative file.
    let body = format!(
        "(bind ((tiers (load_module \"stdlib.semantics.passes.tiers\"))
                (check_file (get tiers \"check_file\" null))
                (paths (list_of {pairs}))
                (out (list_of)))
           (do
             (sequence_each paths
               (lambda (pair)
                 (bind ((rel (get pair 0 null)) (abs (get pair 1 null)))
                   (sequence_each (check_file abs)
                     (lambda (finding)
                       (append out (string_concat_many rel \": \" finding)))))))
             out))"
    );

    let v = eval_ok("stdlib_tiers_corpus", &with_stdlib_root(&body));
    let RuntimeValue::List(items) = v else {
        panic!("expected a list of findings, got {v:?}")
    };
    let findings: Vec<String> = items
        .borrow()
        .iter()
        .map(|f| match f {
            RuntimeValue::Str(s) => s.to_string(),
            other => format!("{other:?}"),
        })
        .collect();
    assert!(
        findings.is_empty(),
        "tier/layering violations found in the stdlib corpus ({} finding(s)):\n{}\n\
         Each line is `<file>: tiers: <loc>: <message>`. Either fix the upward \
         import, or — if intentional — add it to the `tier_exceptions` list in \
         stdlib/semantics/passes/tiers.caap.",
        findings.len(),
        findings.join("\n")
    );
}

/// (B) The DOCUMENTED set of stdlib files permitted to call
/// `ctfe_compiler_load_surface_file_template` directly. This builtin reads and
/// parses a source file as a surface template — a privileged loader primitive.
/// These files (and only these) form the surface/loader machinery; any new call
/// site elsewhere is a layering-bypass regression that should route through the
/// loader instead.
const SURFACE_TEMPLATE_LOADER_ALLOWLIST: &[&str] = &[
    "stdlib/bootstrap.caap",
    "stdlib/boot/reader.caap",
    "stdlib/boot/resolve.caap",
    "stdlib/boot/loader.caap",
    "stdlib/boot/analyze.caap",
    // The ONE sanctioned raw-form reader (audit #12): tiers / imports / bare_gate
    // are directive-level static-analysis passes that must read a module's RAW
    // top-level forms (directives intact, which the loader otherwise consumes).
    // They used to each open `check_file` with the surface-template builtin; that
    // identical three-line read now lives ONCE here (read_leading_forms), so those
    // three passes no longer call the builtin directly and have left this list.
    "stdlib/semantics/passes/source_read.caap",
    "stdlib/frontend/surface.caap",
    // clike's grammar registration (the surface-template load) lives in the
    // clike/grammar.caap leaf after the clike facade split (lowering byte-identical).
    "stdlib/frontend/clike/grammar.caap",
    "stdlib/backend/prep.caap",
    "stdlib/backend/driver.caap",
    "stdlib/backend/driver_wasm.caap",
];

/// The literal whose call sites the allowlist pins.
const SURFACE_TEMPLATE_BUILTIN: &str = "ctfe_compiler_load_surface_file_template";

/// Recursively collect every `*.caap` under `stdlib/` (the whole tree — this
/// lint is not tier-scoped; a bypass anywhere is a regression).
fn collect_all_stdlib_caap_files() -> Vec<std::path::PathBuf> {
    let root = std::path::PathBuf::from(stdlib_path(""));
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read stdlib dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("caap") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// (B) Pure-Rust lint (no bootstrap): the set of files that directly call
/// `ctfe_compiler_load_surface_file_template` must EQUAL the documented
/// allowlist exactly — no unsanctioned new call sites, and the allowlist carries
/// no stale entries.
#[test]
fn stdlib_no_unsanctioned_surface_template_loads() {
    let mut actual: Vec<String> = collect_all_stdlib_caap_files()
        .into_iter()
        .filter(|path| {
            std::fs::read_to_string(path)
                .map(|src| src.contains(SURFACE_TEMPLATE_BUILTIN))
                .unwrap_or(false)
        })
        .map(|path| repo_relative(&path))
        .collect();
    actual.sort();
    actual.dedup();

    let mut expected: Vec<String> = SURFACE_TEMPLATE_LOADER_ALLOWLIST
        .iter()
        .map(|s| s.to_string())
        .collect();
    expected.sort();

    let unexpected: Vec<&String> = actual.iter().filter(|p| !expected.contains(p)).collect();
    let missing: Vec<&String> = expected.iter().filter(|p| !actual.contains(p)).collect();

    assert!(
        unexpected.is_empty() && missing.is_empty(),
        "the set of files calling `{SURFACE_TEMPLATE_BUILTIN}` drifted from the allowlist.\n\
         UNEXPECTED (new call sites NOT on the allowlist — a layering-bypass regression):\n  {}\n\
         MISSING (allowlist entries that no longer call it — remove them from the allowlist):\n  {}\n\
         If a new call site is intentional, add its `stdlib/...` path to \
         SURFACE_TEMPLATE_LOADER_ALLOWLIST in this file; otherwise route the load \
         through the loader instead of calling the builtin directly.",
        if unexpected.is_empty() {
            "(none)".to_string()
        } else {
            unexpected
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        },
        if missing.is_empty() {
            "(none)".to_string()
        } else {
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  ")
        }
    );
}

/// Clike's grammar must stay a lexer/token-stream grammar. Language words and
/// concrete operator spellings belong in semantic registries/lowerers, not in the
/// grammar regex.
#[test]
fn stdlib_clike_grammar_stays_lexer_only() {
    let path = std::path::PathBuf::from(stdlib_path("frontend/clike/grammar.caap"));
    let src = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let start = src
        .find("(bind clike_grammar")
        .expect("clike grammar binding exists");
    let end = src
        .find("(bind clike_host")
        .expect("clike host binding follows grammar");
    let grammar = &src[start..end];

    let banned_words = [
        "mut", "const", "volatile", "extern", "export", "u32", "i32", "struct", "enum",
    ];
    let word_hits: Vec<&str> = banned_words
        .iter()
        .copied()
        .filter(|tok| contains_whole_token(grammar, tok))
        .collect();

    let old_operator_fragments = [
        "<<=|", ">>=|", "->|", "=>|", "==|", "!=|", "<=|", ">=|", "<<|", ">>|",
        "&&|", "\\|\\||", "\\+=|", "-=|", "\\*=|", "\\/=|", "%=|", "&=|",
        "\\|=|", "\\^=|", "\\+\\+|", "--|",
    ];
    let op_hits: Vec<&str> = old_operator_fragments
        .iter()
        .copied()
        .filter(|frag| grammar.contains(frag))
        .collect();

    let old_literal_fragments = ["hexlit", "binlit", "0[xX]", "0[bB]", "[0-9a-fA-F]", "[01]+"];
    let literal_hits: Vec<&str> = old_literal_fragments
        .iter()
        .copied()
        .filter(|frag| grammar.contains(frag))
        .collect();

    assert!(
        word_hits.is_empty() && op_hits.is_empty() && literal_hits.is_empty(),
        "clike grammar drifted away from lexer-only policy.\n\
         Language/type/metadata words in grammar: {}\n\
         Old explicit operator-regex fragments in grammar: {}\n\
         Old radix-specific literal fragments in grammar: {}\n\
         Keep these decisions in clike semantic registries/lowerers instead.",
        if word_hits.is_empty() {
            "(none)".to_string()
        } else {
            word_hits.join(", ")
        },
        if op_hits.is_empty() {
            "(none)".to_string()
        } else {
            op_hits.join(", ")
        },
        if literal_hits.is_empty() {
            "(none)".to_string()
        } else {
            literal_hits.join(", ")
        }
    );
    assert!(
        grammar.contains("[-+*\\\\/%<>=&|^!~]+|[,;]"),
        "clike operator rule should remain a generic operator-character run"
    );
}

// --- (D) module identity: (module …) must match the file's path ---------------
//
// Every production `.caap` under `stdlib/` (EXCLUDING `stdlib/lib/tests/`, the
// in-language test/fixture corpus) must declare a top-of-file `(module …)` whose
// dotted name equals its path under `stdlib/` (slashes -> dots, minus `.caap`) —
// e.g. `stdlib/sys/fs.caap` -> `(module stdlib.sys.fs)`. Two documented exception
// classes are allowlisted:
//   (1) RAW_BOOTSTRAP_NO_MODULE — files run via `ctfe_compiler_execute_bootstrap_file`
//       / `run_expanded` BEFORE the `module` form is a bound name; they cannot carry
//       a `(module …)` form (it would raise `unknown name: module`) and set their
//       registry identity via `ctfe_compiler_register_value`.
//   (2) SHORT_NAME_MODULES — boot modules loaded THROUGH the loader (so they DO
//       carry `(module …)`) that intentionally keep a non-path-canonical short name
//       pinned by external consumers (the tier-exception list / the session-command
//       surface).
// See the "Ідентичність модуля" section of stdlib/CONVENTIONS.md.

/// (1) Raw bootstrap scripts that legitimately carry NO `(module …)` form. Their
/// registry identity is set explicitly via `ctfe_compiler_register_value`; adding
/// a `(module …)` form would raise `unknown name: module` (they run before that
/// form is bound). Each file carries a `MODULE IDENTITY:` banner explaining this.
const RAW_BOOTSTRAP_NO_MODULE: &[&str] = &[
    "stdlib/bootstrap.caap",
    "stdlib/boot/expander.caap",
    "stdlib/boot/forms.caap",
    "stdlib/boot/check.caap",
    "stdlib/boot/gate.caap",
    "stdlib/boot/loader.caap",
    "stdlib/boot/namespace.caap",
    "stdlib/boot/reader.caap",
    "stdlib/boot/resolve.caap",
    "stdlib/boot/unit_build.caap",
    "stdlib/boot/native_emit.caap",
    "stdlib/boot/pe.caap",
    "stdlib/boot/peval.caap",
    "stdlib/boot/sys_grants.caap",
    "stdlib/boot/opt_O1.caap",
    "stdlib/boot/opt_O2.caap",
    "stdlib/boot/opt_O3.caap",
];

/// (2) Boot modules that ARE loader-loaded (so they carry `(module …)`) but keep a
/// short, non-path-canonical name on purpose. `(repo-relative path, declared name)`.
/// `stdlib.analyze` is pinned by the tier-exception list in
/// `stdlib/semantics/passes/tiers.caap`; the trio is the established
/// session-command identity (`commands.caap` self-locates via
/// `(module_path "stdlib.commands")`). Renaming would break those consumers.
const SHORT_NAME_MODULES: &[(&str, &str)] = &[
    ("stdlib/boot/analyze.caap", "stdlib.analyze"),
    ("stdlib/boot/commands.caap", "stdlib.commands"),
    ("stdlib/boot/run.caap", "stdlib.run"),
];

/// The path-canonical module name for a stdlib `.caap` file: its repo-relative
/// path with `stdlib/` stripped, slashes -> dots, and the `.caap` suffix dropped.
/// `stdlib/sys/fs.caap` -> `stdlib.sys.fs`.
fn path_canonical_module_name(rel_path: &str) -> Option<String> {
    let inner = rel_path.strip_prefix("stdlib/")?.strip_suffix(".caap")?;
    Some(format!("stdlib.{}", inner.replace('/', ".")))
}

/// The dotted name of the file's top-of-file `(module …)` directive, or `None` if
/// the first real form is not `(module …)` (a body form before any `(module …)`
/// means the file has none). Whitespace-tolerant to match the loader's own reader:
/// the directive may use any whitespace between `(`, `module`, and the name, and
/// may span lines — so `(module\tx)` and `(module\n  x)` are recognised just like
/// `(module x)`. Comment (`;`-to-end-of-line) and blank lines are skipped first.
/// A present-but-empty form (`(module )`) yields `Some("")`, which the caller
/// reports as a malformed directive rather than a missing one.
fn declared_module_name(src: &str) -> Option<String> {
    // Strip `;`-comments, then concatenate the remaining text so a directive that
    // spans lines is tokenized as one form (the reader does not care about lines).
    let mut code = String::new();
    for raw in src.lines() {
        let without_comment = match raw.find(';') {
            Some(i) => &raw[..i],
            None => raw,
        };
        code.push_str(without_comment);
        code.push('\n');
    }
    let code = code.trim_start();
    // The first form must open `(`, then the token `module`, then a name token.
    let rest = code.strip_prefix('(')?;
    let rest = rest.trim_start();
    let after_kw = rest.strip_prefix("module")?;
    // `module` must be a whole token — the next char is whitespace or `)`, never a
    // name char (so `(module-foo …)` is NOT read as the `module` directive).
    match after_kw.chars().next() {
        Some(c) if c.is_whitespace() || c == ')' => {}
        _ => return None,
    }
    let name: String = after_kw
        .trim_start()
        .chars()
        .take_while(|c| *c != ')' && !c.is_whitespace())
        .collect();
    Some(name)
}

/// (D) Module identity must not drift from the file tree: every production stdlib
/// `.caap` (outside `lib/tests`) declares a path-canonical `(module …)`, or is on a
/// documented allowlist (raw-bootstrap files with no `(module …)`, or intentional
/// short-named boot modules). Pure-Rust (no bootstrap): a token scan of each file.
///
/// On drift this does NOT pass vacuously: it accumulates every offending file with
/// the precise reason (wrong/missing/unexpected `(module …)`) and prints them all.
#[test]
fn stdlib_module_identity_check() {
    let tests_dir = std::path::PathBuf::from(stdlib_path("lib/tests"));
    let raw_bootstrap: std::collections::BTreeSet<&str> =
        RAW_BOOTSTRAP_NO_MODULE.iter().copied().collect();
    let short_names: std::collections::BTreeMap<&str, &str> =
        SHORT_NAME_MODULES.iter().copied().collect();

    let mut problems: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for path in collect_all_stdlib_caap_files() {
        // Exclude the in-language test/fixture corpus wholesale.
        if path.starts_with(&tests_dir) {
            continue;
        }
        let rel = repo_relative(&path);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let declared = declared_module_name(&src);
        checked += 1;

        if raw_bootstrap.contains(rel.as_str()) {
            // Must carry NO `(module …)` form (it would fail to evaluate).
            if let Some(name) = declared {
                problems.push(format!(
                    "{rel}: on RAW_BOOTSTRAP_NO_MODULE allowlist but declares `(module {name})` \
                     — a raw bootstrap script cannot carry a `(module …)` form (it runs before \
                     `module` is bound). Remove the form, or remove the file from the allowlist."
                ));
            }
            continue;
        }

        let canonical = match path_canonical_module_name(&rel) {
            Some(c) => c,
            None => {
                problems.push(format!(
                    "{rel}: cannot derive a canonical module name from the path"
                ));
                continue;
            }
        };

        match declared {
            None => problems.push(format!(
                "{rel}: no top-of-file `(module …)` — expected `(module {canonical})` \
                 (or add the file to a documented allowlist in this test)"
            )),
            Some(name) if name == canonical => {} // OK: path-canonical
            Some(name) => {
                // A mismatch is allowed only if it is the documented short name.
                match short_names.get(rel.as_str()) {
                    Some(expected) if *expected == name => {} // OK: allowlisted short name
                    Some(expected) => problems.push(format!(
                        "{rel}: declares `(module {name})` but its SHORT_NAME_MODULES allowlist \
                         entry is `{expected}` — update one to match"
                    )),
                    None => problems.push(format!(
                        "{rel}: declares `(module {name})` but the path-canonical name is \
                         `{canonical}`. Rename the module to match its path, or — if the short \
                         name is intentional and pinned by an external consumer — add \
                         (\"{rel}\", \"{name}\") to SHORT_NAME_MODULES with a documented reason."
                    )),
                }
            }
        }
    }

    assert!(
        checked >= 100,
        "expected the stdlib corpus to be enumerated for module-identity; checked only {checked}"
    );
    assert!(
        problems.is_empty(),
        "module-identity drift in the stdlib tree ({} file(s)):\n  {}\n\
         Policy: every production `.caap` under stdlib/ (outside lib/tests) declares a \
         path-canonical `(module …)`; exceptions live on RAW_BOOTSTRAP_NO_MODULE \
         (no `(module …)` form) or SHORT_NAME_MODULES (intentional short name). See the \
         \"Ідентичність модуля\" section of stdlib/CONVENTIONS.md.",
        problems.len(),
        problems.join("\n  ")
    );
}

// --- (C) docs/stdlib-architecture.md catalog drift ----------------------------
//
// The "Public Module Catalog" tables in `docs/stdlib-architecture.md` are the
// hand-written contract for every public stdlib module: each row pins a module's
// dotted name, its source file, and the names it documents under "API". This lint
// pins those two facts mechanically — the source file MUST exist where the row's
// link points, and every documented API name MUST actually appear (as a whole
// token) in that source file. A row that drifts (renamed/removed file, renamed/
// dropped export) is a docs regression that this test surfaces precisely.
//
// Pure-Rust (no bootstrap): the catalog is plain Markdown and the export check is
// a token-presence scan of the source. Why token-presence over "must appear in an
// `(export …)` form"? The catalog spans modules with heterogeneous export
// mechanisms — most `lib/*` modules use `(export …)`, boot modules like
// `boot/expander.caap` register a value map with `"name"` string keys and have NO
// `(export …)` form at all, and re-export facades use `(re_export …)`. A scan for
// the whole-token name covers all three without false drift, while still catching
// a renamed or deleted export (the old token vanishes from the file).

/// The catalog section header; parsing starts here.
const CATALOG_SECTION_HEADER: &str = "## Public Module Catalog";
/// The section that immediately follows the catalog; parsing stops here.
const CATALOG_SECTION_END: &str = "## Dependency Shape";

/// Absolute path to `docs/stdlib-architecture.md`.
fn docs_architecture_path() -> std::path::PathBuf {
    let stdlib_root = std::path::PathBuf::from(stdlib_path(""));
    let repo_root = stdlib_root
        .parent()
        .expect("stdlib has a parent (repo root)");
    repo_root.join("docs/stdlib-architecture.md")
}

/// True if `ch` can appear inside a CAAP identifier / export name. Names may carry
/// trailing `?`/`!` (predicates / mutators) and embedded `-` (e.g. surface heads);
/// this set defines the token boundary used to test whole-name presence.
fn is_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '_' | '?' | '!' | '-')
}

/// Whether `name` occurs in `src` as a WHOLE token — not as a substring of a
/// longer identifier. `set_bit` must not match inside `set_bits8`, but a real
/// standalone `set_bit` token (in an `(export …)` form, a `"set_bit"` map key, or
/// a definition) does match. Both neighbours of the match must be non-name chars.
fn contains_whole_token(src: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut search_from = 0;
    while let Some(off) = src[search_from..].find(name) {
        let start = search_from + off;
        let end = start + name.len();
        let before_ok = src[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !is_name_char(c));
        let after_ok = src[end..].chars().next().is_none_or(|c| !is_name_char(c));
        if before_ok && after_ok {
            return true;
        }
        // Advance past this occurrence's first byte to find later matches.
        search_from = start + 1;
    }
    false
}

/// One parsed catalog row that names a real module: its dotted name, the
/// repo-relative source path from the row's link, and the documented export tokens.
struct CatalogRow {
    module: String,
    rel_path: String,
    exports: Vec<String>,
}

/// Pull the backticked tokens out of an API cell, e.g. `` `a`, `b` `` -> [a, b].
fn backtick_tokens(cell: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = cell;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        if let Some(close) = after.find('`') {
            let tok = &after[..close];
            if !tok.is_empty() {
                out.push(tok.to_string());
            }
            rest = &after[close + 1..];
        } else {
            break;
        }
    }
    out
}

/// Parse the "Public Module Catalog" tables. Rows whose first cell is not a single
/// `` `stdlib.x` `` / `` `sys.x` `` dotted name (header/separator rows, and the
/// `form registry entries` / `registry values` / `bootstrap scripts` rows) are
/// skipped, as are PROSE / RE-EXPORT API cells (which list re-exported names that
/// live in OTHER files). Everything else becomes a [`CatalogRow`].
fn parse_catalog(doc: &str) -> Vec<CatalogRow> {
    let start = doc
        .find(CATALOG_SECTION_HEADER)
        .expect("docs/stdlib-architecture.md has a Public Module Catalog section");
    let after = &doc[start..];
    let end = after.find(CATALOG_SECTION_END).unwrap_or(after.len());
    let section = &after[..end];

    let mut rows = Vec::new();
    for line in section.lines() {
        let line = line.trim();
        if !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim())
            .collect();
        if cells.len() < 4 {
            continue;
        }
        let (mod_cell, file_cell, api_cell) = (cells[0], cells[1], cells[3]);

        // Header / separator rows.
        if mod_cell == "Module"
            || mod_cell == "Surface"
            || (!mod_cell.is_empty() && mod_cell.chars().all(|c| matches!(c, '-' | ':' | ' ')))
        {
            continue;
        }

        // Module cell must be exactly one `backticked` dotted name under stdlib./sys.
        // Name chars mirror `is_name_char` plus the `.` segment separator, so a
        // future module like `stdlib.x.foo-bar` is checked rather than silently
        // skipped as a non-module row.
        let Some(dotted) = mod_cell
            .strip_prefix('`')
            .and_then(|s| s.strip_suffix('`'))
            .filter(|s| {
                (s.starts_with("stdlib.") || s.starts_with("sys."))
                    && s.chars().all(|c| c == '.' || is_name_char(c))
            })
        else {
            // `form registry entries`, `registry values`, `bootstrap scripts`, etc.
            continue;
        };

        // Extract the source path from the row's link: `](../stdlib/<path>.caap)`.
        let Some(rel_path) = file_cell
            .find("](../")
            .map(|i| &file_cell[i + "](../".len()..])
            .and_then(|s| s.split(')').next())
            .filter(|s| s.starts_with("stdlib/") && s.ends_with(".caap"))
            .map(|s| s.to_string())
        else {
            // Multi-file rows or rows without a single `.caap` link.
            continue;
        };

        // Prose / re-export API cells list names that live in OTHER files; their
        // tokens are not this file's exports, so skip the export check for them.
        let api_lower = api_cell.to_ascii_lowercase();
        let exports = if api_lower.starts_with("re-exports") || api_lower.contains("etc.") {
            Vec::new()
        } else {
            backtick_tokens(api_cell)
        };

        rows.push(CatalogRow {
            module: dotted.to_string(),
            rel_path,
            exports,
        });
    }
    rows
}

/// (C) `docs/stdlib-architecture.md`'s catalog must not drift from the tree: every
/// module row's source file EXISTS, and every documented API name is PRESENT (as a
/// whole token) in that file. Marked `acceptance` only to group it with the other
/// stdlib-corpus gates — it runs no bootstrap and is fast.
///
/// On drift this does NOT pass vacuously: it accumulates every missing file and
/// every missing export across the whole catalog and prints them with the owning
/// module and path before failing, so the exact rows to fix are obvious.
#[test]
#[ignore = "acceptance: docs catalog matches source"]
fn stdlib_docs_catalog_matches_source() {
    let doc_path = docs_architecture_path();
    let doc = std::fs::read_to_string(&doc_path)
        .unwrap_or_else(|e| panic!("read {}: {e}", doc_path.display()));
    let rows = parse_catalog(&doc);

    // Sanity: the catalog must actually have been parsed. If this trips, the
    // section markers or table format changed and the parser silently matched
    // nothing — that itself is drift worth surfacing rather than passing blank.
    assert!(
        rows.len() >= 100,
        "parsed only {} catalog module rows from {} — the catalog format likely \
         changed (section markers `{CATALOG_SECTION_HEADER}` / `{CATALOG_SECTION_END}`, \
         the `| `stdlib.x` | [file](../stdlib/…) | … | API | deps |` row shape, or the \
         link spelling). Update parse_catalog to match.",
        rows.len(),
        doc_path.display()
    );

    let repo_root = std::path::PathBuf::from(stdlib_path(""))
        .parent()
        .expect("stdlib has a parent (repo root)")
        .to_path_buf();

    let mut missing_files: Vec<String> = Vec::new();
    let mut missing_exports: Vec<String> = Vec::new();

    for row in &rows {
        let abs = repo_root.join(&row.rel_path);
        let src = match std::fs::read_to_string(&abs) {
            Ok(s) => s,
            Err(_) => {
                missing_files.push(format!("{} -> {} (no such file)", row.module, row.rel_path));
                continue;
            }
        };
        for ex in &row.exports {
            if !contains_whole_token(&src, ex) {
                missing_exports.push(format!(
                    "{}: documented export `{}` is absent from {}",
                    row.module, ex, row.rel_path
                ));
            }
        }
    }

    assert!(
        missing_files.is_empty() && missing_exports.is_empty(),
        "docs/stdlib-architecture.md Public Module Catalog drifted from the stdlib tree \
         ({} missing file(s), {} missing export(s)).\n\
         MISSING FILES (a row's `.caap` link points nowhere — fix the path or remove the row):\n  {}\n\
         MISSING EXPORTS (a name in the row's API column is not present in its source — the \
         export was renamed/removed, or the catalog is stale; update whichever is wrong):\n  {}",
        missing_files.len(),
        missing_exports.len(),
        if missing_files.is_empty() {
            "(none)".to_string()
        } else {
            missing_files.join("\n  ")
        },
        if missing_exports.is_empty() {
            "(none)".to_string()
        } else {
            missing_exports.join("\n  ")
        }
    );
}

/// (D) The compiler-registry key ABI (audit #13). Boot/codegen modules find each
/// other through STRING KEYS in the compiler registry. `stdlib/boot/registry_
/// contract.caap` is the ONE formal description of those keys; this drift-guard
/// pins it: every registry key LITERAL passed to a `ctfe_compiler_{lookup,
/// register,bind}_value` call in production stdlib code must be documented in that
/// contract. A new undocumented key is a hidden-DI regression — add it to
/// registry_contract.caap (and the ABI doc) or route the dependency explicitly.
///
/// Only LITERAL keys are checked (a key passed as a variable — e.g. the bootstrap
/// manifest's `(get entry "key")` — is dynamic and out of scope). The contract
/// module itself is excluded (it defines the keys).
fn registry_key_literals(src: &str) -> Vec<String> {
    const CALLS: &[&str] = &[
        "ctfe_compiler_lookup_value",
        "ctfe_compiler_register_value",
        "ctfe_compiler_bind_value",
    ];
    let mut keys = Vec::new();
    for call in CALLS {
        let mut from = 0;
        while let Some(rel) = src[from..].find(call) {
            let start = from + rel + call.len();
            // the key is the first string literal after the call head; cap the
            // window so we never run past the call's own argument list.
            let window = &src[start..usize::min(start + 80, src.len())];
            if let Some(q1) = window.find('"') {
                if let Some(q2) = window[q1 + 1..].find('"') {
                    let key = &window[q1 + 1..q1 + 1 + q2];
                    // A literal ending in `.` is a key PREFIX built up with
                    // string_concat (e.g. "stdlib.sys.grant." + op) — a dynamic key
                    // namespace, not a fixed key, so it is out of scope here.
                    if (key.starts_with("stdlib.") || key.starts_with("caap."))
                        && !key.ends_with('.')
                    {
                        keys.push(key.to_string());
                    }
                }
            }
            from = start;
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

// --- (E) import dependency graph: hub fan-in budget (audit #18 + #20) ----------
//
// The stdlib dependency graph has a few strong HUBS — modules with high fan-in
// (many dependents). A small API change in a hub has a wide blast radius, so the
// hub-stability policy (docs/mechanisms/stdlib-hub-stability.md) gives them a
// heavier stability contract. This lint turns that policy into enforcement:
//
//   #20 hub fan-in CEILING — each stability-critical hub's importer count must
//       stay ≤ a documented ceiling (current + a little headroom). A new importer
//       that blows a hub's blast-radius budget fails the build deliberately, so
//       the widening is a conscious choice (bump the ceiling + record the reason)
//       rather than an accident.
//   #18 dep-graph drift artifact — each hub's importer count is ALSO pinned to an
//       EXACT number, so ANY drift (up OR down) surfaces. This is the checked
//       artifact that complements the in-language tiers hard gate: the tiers pass
//       proves the tower does not invert; this proves the hubs' fan-in is exactly
//       what the policy doc claims.
//
// Pure-Rust (no bootstrap): a token scan of each production `.caap`'s top-of-file
// import directives. An import is a `(use <m> …)`, `(import <m> …)`, or
// `(re_export <m> …)` form whose `<m>` is a dotted module name. The graph maps a
// module to the SET of files that import it (a file importing a hub twice counts
// once). `lib/tests/` (the in-language test/fixture corpus) is excluded — those
// are not library modules and their imports are not part of the production graph.

/// The four stability-critical hubs and their fan-in CEILINGS (audit #20). Each
/// ceiling is the current importer count plus a small headroom (`+3`): a few new
/// dependents on a 40-ish-fan-in hub are unremarkable, but a sudden jump past the
/// budget is a blast-radius signal worth a deliberate review. Raise alongside
/// `HUB_FANIN_EXACT` and the policy doc when a hub legitimately grows.
const HUB_FANIN_CEILINGS: &[(&str, usize)] = &[
    ("stdlib.lib.collections.sequence", 50),  // current 47
    ("stdlib.syntax.ast", 63),                // current 61
    ("stdlib.semantics.passes.registry", 40), // current 37 (+5 urun static-safety passes)
    ("stdlib.syntax.ir", 27),                 // current 26
];

/// The four hubs' EXACT current fan-in (audit #18). Pinned so ANY drift — a new
/// importer OR a removed one — surfaces as a failing test rather than silently
/// shifting the graph. When you intentionally add/remove a hub dependent, update
/// the matching number here (and, if it changes the order of magnitude, the
/// approximate figure in docs/mechanisms/stdlib-hub-stability.md).
const HUB_FANIN_EXACT: &[(&str, usize)] = &[
    ("stdlib.lib.collections.sequence", 47),
    ("stdlib.syntax.ast", 61),
    ("stdlib.semantics.passes.registry", 39),
    ("stdlib.syntax.ir", 26),
];

/// The import directive heads that pull in (and thus depend on) a module. All
/// three LOAD the named module for the importing file, so all three count toward
/// fan-in: `use` and `import` are the obvious dependencies, and `re_export`
/// additionally re-publishes — but it still loads its module, so it is an edge too.
const IMPORT_DIRECTIVE_HEADS: &[&str] = &["use", "import", "re_export"];

/// Parse the dotted module names a single `.caap` source imports via
/// `(use …)` / `(import …)` / `(re_export …)` directives. Returns the SET of
/// imported modules for this file (deduplicated). `;`-comments are stripped first
/// (so a directive mentioned in prose — e.g. `bare_gate.caap`'s `(re_export …)`
/// examples — is not a phantom edge), mirroring `declared_module_name`'s reader.
///
/// A directive is `(` + the head token (`use`/`import`/`re_export`) followed by
/// whitespace + a dotted name token. The head must be a WHOLE token (the char
/// after it is whitespace), so `(used …)` or `(import_foo …)` never match. Only
/// dotted names (containing a `.`) are kept — a bare single-segment token is a
/// macro parameter or a non-module form, never a stdlib module.
fn imported_modules(src: &str) -> std::collections::BTreeSet<String> {
    // Strip `;`-comments line by line (the directive never spans a comment).
    let mut code = String::new();
    for raw in src.lines() {
        let without_comment = match raw.find(';') {
            Some(i) => &raw[..i],
            None => raw,
        };
        code.push_str(without_comment);
        code.push('\n');
    }

    let mut modules = std::collections::BTreeSet::new();
    for head in IMPORT_DIRECTIVE_HEADS {
        // Match `(` immediately followed by the head token, then verify the token
        // boundary and read the module name that follows.
        let needle = format!("({head}");
        let mut from = 0;
        while let Some(rel) = code[from..].find(&needle) {
            let after_head = from + rel + needle.len();
            from = after_head; // advance regardless of whether this match is a hit
                               // The head must be a whole token: the next char is whitespace (the
                               // usual `(use stdlib.x …`), never a name char (so `(used …`,
                               // `(import_all …` do not match).
            let after = &code[after_head..];
            match after.chars().next() {
                Some(c) if c.is_whitespace() => {}
                _ => continue,
            }
            // The next token is the module name: skip whitespace, then take a run
            // of name/`.` chars. Only a DOTTED name is a real module dependency.
            let name: String = after
                .trim_start()
                .chars()
                .take_while(|c| is_name_char(*c) || *c == '.')
                .collect();
            if name.contains('.') {
                modules.insert(name);
            }
        }
    }
    modules
}

/// Build the stdlib import dependency graph: module -> set of repo-relative files
/// importing it. Scans every production `.caap` (EXCLUDING `lib/tests/`).
fn build_import_graph() -> std::collections::BTreeMap<String, std::collections::BTreeSet<String>> {
    let tests_dir = std::path::PathBuf::from(stdlib_path("lib/tests"));
    let mut graph: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();
    for path in collect_all_stdlib_caap_files() {
        if path.starts_with(&tests_dir) {
            continue;
        }
        let rel = repo_relative(&path);
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        for module in imported_modules(&src) {
            graph.entry(module).or_default().insert(rel.clone());
        }
    }
    graph
}

/// (E) The stability-critical hubs' fan-in is bounded (#20) AND pinned (#18). Both
/// the ceiling and the exact-pin assertions read from one scan of the import graph.
/// Pure-Rust (no bootstrap), so it runs by default — not `#[ignore]`.
///
/// On a violation this does NOT pass vacuously: a missing hub (it stopped being
/// imported, or its name changed) trips a dedicated assertion, and the ceiling /
/// exact-pin messages name the hub, the count, the bound, and point at the
/// hub-stability change-checklist so the fix path is unambiguous.
#[test]
fn stdlib_hub_fanin_within_budget() {
    let graph = build_import_graph();

    // Sanity: the graph must actually have been built. If a refactor breaks the
    // directive scan it would silently find nothing — surface that rather than
    // pass blank. (Every hub below is imported many times over.)
    let edges: usize = graph.values().map(|s| s.len()).sum();
    assert!(
        edges >= 100,
        "the stdlib import graph looks empty ({edges} edges) — the directive scan \
         likely broke (the `(use|import|re_export) <dotted.name>` shape changed). \
         Fix `imported_modules`."
    );

    let fanin = |hub: &str| graph.get(hub).map(|s| s.len()).unwrap_or(0);

    // #20 — ceiling: each hub's fan-in must stay within its blast-radius budget.
    let mut over_ceiling: Vec<String> = Vec::new();
    for (hub, ceiling) in HUB_FANIN_CEILINGS {
        assert!(
            graph.contains_key(*hub),
            "stability-critical hub `{hub}` has ZERO importers in the scanned graph — \
             it was renamed/removed, or the import scan broke. Update HUB_FANIN_CEILINGS \
             / HUB_FANIN_EXACT (and docs/mechanisms/stdlib-hub-stability.md) to match."
        );
        let count = fanin(hub);
        if count > *ceiling {
            over_ceiling.push(format!("`{hub}`: {count} importers > ceiling {ceiling}"));
        }
    }
    assert!(
        over_ceiling.is_empty(),
        "stdlib hub fan-in exceeded its blast-radius budget (audit #20):\n  {}\n\
         A new dependent on a stability-critical hub widens its blast radius. If the \
         new importer is justified, RAISE the hub's ceiling in HUB_FANIN_CEILINGS \
         (and its exact pin in HUB_FANIN_EXACT) and follow the hub-edit change \
         checklist in docs/mechanisms/stdlib-hub-stability.md (changelog entry, full \
         stdlib test run, dependency-impact note). Otherwise route the new dependency \
         through a smaller module instead of widening the hub.",
        over_ceiling.join("\n  ")
    );

    // #18 — exact pin: any drift (up OR down) is a checked artifact change.
    let mut drift: Vec<String> = Vec::new();
    for (hub, expected) in HUB_FANIN_EXACT {
        let count = fanin(hub);
        if count != *expected {
            drift.push(format!("`{hub}`: {count} importers, pinned at {expected}"));
        }
    }
    assert!(
        drift.is_empty(),
        "stdlib hub fan-in drifted from its pinned value (audit #18):\n  {}\n\
         The import dependency graph changed shape at a stability-critical hub. This \
         is a checked artifact: when you intentionally add OR remove a hub dependent, \
         update the matching number in HUB_FANIN_EXACT (and the ceiling in \
         HUB_FANIN_CEILINGS / the approximate figure in \
         docs/mechanisms/stdlib-hub-stability.md if it shifts the order of magnitude).",
        drift.join("\n  ")
    );
}

#[test]
fn stdlib_registry_keys_documented() {
    let contract_path = stdlib_path("boot/registry_contract.caap");
    let contract =
        std::fs::read_to_string(&contract_path).expect("read stdlib/boot/registry_contract.caap");

    let mut undocumented: Vec<String> = Vec::new();
    for path in collect_all_stdlib_caap_files() {
        let rel = repo_relative(&path);
        // the contract module DEFINES the keys; the in-language test corpus may
        // reference internal keys freely.
        if rel.ends_with("boot/registry_contract.caap") || rel.contains("/lib/tests/") {
            continue;
        }
        let src = std::fs::read_to_string(&path).expect("read stdlib .caap");
        for key in registry_key_literals(&src) {
            // documented == the key string appears in the contract's registry_abi.
            let quoted = format!("\"{key}\"");
            if !contract.contains(&quoted) {
                undocumented.push(format!("{rel}: {key}"));
            }
        }
    }
    undocumented.sort();
    undocumented.dedup();

    assert!(
        undocumented.is_empty(),
        "registry key(s) used in stdlib code but NOT documented in \
         stdlib/boot/registry_contract.caap (hidden compiler-registry DI — add each \
         to the registry_abi table + the ABI doc, or route the dependency \
         explicitly):\n  {}",
        undocumented.join("\n  ")
    );
}

// --- (F) freeform Markdown docs: relative links resolve + no moved paths -------
//
// The hand-written stdlib docs (`stdlib/**/*.md` — area READMEs, CONVENTIONS,
// ROADMAP) link to sibling source files and each other with RELATIVE paths. The
// `docs/stdlib-architecture.md` catalog drift-test (C, above) only covers the
// public-module catalog tables in that ONE file; the freeform docs were drifting
// undetected after the big `stdlib/` reshuffle (`types/`/`kits/` -> `semantics/`,
// `backend/`, `frontend/`; `lib/syntax/`, `lib/passes/` -> top-level `syntax/`,
// `semantics/passes/`). This lint closes that gap with two pure-Rust checks:
//
//   (1) every RELATIVE markdown link `[text](path)` resolves to an existing file
//       (relative to the linking `.md`'s directory). External (`http(s)://`),
//       in-page anchors (`#…`), and absolute (`/…`) targets are out of scope.
//   (2) a SMALL, high-precision denylist of unambiguous MOVED path tokens
//       (`STALE_DOC_TOKENS`) appears in NO `.md`. The link resolver is the primary
//       gate; the token list catches the same moves in prose that is not a link
//       (table cells, code fences, headings) without the false positives a broad
//       token (`types/`, `"six"`) would bring.
//
// Pure-Rust (no bootstrap): plain file scanning, so it runs by default.

/// Unambiguous MOVED path tokens that must not appear in any `stdlib/**/*.md`.
/// Kept PRECISE on purpose: each one only ever names a directory that was
/// relocated by the stdlib reshuffle, so a hit is always real drift —
///   `kits/`        -> split into `backend/` (codegen) + `frontend/` (grammars)
///   `lib/syntax/`  -> top-level `syntax/`
///   `lib/passes/`  -> `semantics/passes/`
/// Ambiguous tokens (bare `types/`, the word `six`) are deliberately EXCLUDED —
/// they would false-positive on legitimate prose; the link resolver below is the
/// primary gate and the prose count was corrected by hand.
const STALE_DOC_TOKENS: &[&str] = &["kits/", "lib/syntax/", "lib/passes/"];

/// Recursively collect every `*.md` under `stdlib/`.
fn collect_all_stdlib_md_files() -> Vec<std::path::PathBuf> {
    let root = std::path::PathBuf::from(stdlib_path(""));
    let mut found = Vec::new();
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read stdlib dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Extract the RELATIVE link targets from one markdown line: the `path` of every
/// `[text](path)`, with external / anchor / absolute targets filtered out and any
/// `#anchor` / `?query` suffix stripped. Returns the cleaned, repo-relative-able
/// path strings (still relative to the linking file's directory).
///
/// A `](` only opens a link target when a `[` precedes the `]` earlier on the line
/// (the link-text bracket). This keeps a stray `](` in prose / code spans — these
/// are Ukrainian docs full of array-index spans like `a[i]` / `T[N][M]` — from
/// being mis-read as a link, e.g. `arr[i](x)` is NOT a link to `x`. All `[`/`]`/`(`
/// /`)` are ASCII, so the byte indices used for slicing are always char boundaries
/// (safe on the Cyrillic content).
fn relative_link_targets(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // A `](` opens a link target only if a `[` precedes this `]` (the link text).
        if bytes[i] == b']'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'('
            && line[..i].contains('[')
        {
            if let Some(close_rel) = line[i + 2..].find(')') {
                let raw = &line[i + 2..i + 2 + close_rel];
                // Strip a `#anchor` / `?query` suffix; the file part is what we test.
                let path = raw.split('#').next().unwrap_or(raw);
                let path = path.split('?').next().unwrap_or(path).trim();
                let is_external = path.starts_with("http://")
                    || path.starts_with("https://")
                    || path.starts_with("mailto:");
                let is_absolute = path.starts_with('/');
                if !path.is_empty() && !is_external && !is_absolute {
                    out.push(path.to_string());
                }
                i += 2 + close_rel + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// (F) Freeform stdlib Markdown docs do not drift: (1) every relative markdown
/// link resolves to a real file, and (2) no unambiguous moved-path token survives.
/// Pure-Rust (no bootstrap), so it runs by default — not `#[ignore]`.
///
/// On a violation this does NOT pass vacuously: it accumulates every broken link
/// with `file:line` + the unresolved target, and every stale-token hit with
/// `file:line` + the token, and prints the full set before failing.
#[test]
fn stdlib_markdown_links_and_stale_tokens() {
    let md_files = collect_all_stdlib_md_files();

    // Sanity: the docs corpus must actually have been enumerated. If this trips,
    // the walk broke (or the docs vanished) — surface it rather than pass blank.
    assert!(
        md_files.len() >= 10,
        "expected the stdlib docs corpus to be enumerated; found only {} `.md` file(s)",
        md_files.len()
    );

    let mut broken_links: Vec<String> = Vec::new();
    let mut stale_tokens: Vec<String> = Vec::new();
    let mut links_checked = 0usize;

    for md in &md_files {
        let rel = repo_relative(md);
        let dir = md.parent().expect("md file has a parent directory");
        let src =
            std::fs::read_to_string(md).unwrap_or_else(|e| panic!("read {}: {e}", md.display()));

        for (lineno, line) in src.lines().enumerate() {
            let line_1based = lineno + 1;

            // (1) relative link targets must resolve from the file's directory.
            for target in relative_link_targets(line) {
                links_checked += 1;
                let resolved = dir.join(&target);
                if !resolved.exists() {
                    broken_links.push(format!("{rel}:{line_1based}: [{target}] -> not found"));
                }
            }

            // (2) no unambiguous moved-path token may appear anywhere in the docs.
            for tok in STALE_DOC_TOKENS {
                if line.contains(tok) {
                    stale_tokens.push(format!("{rel}:{line_1based}: stale path token `{tok}`"));
                }
            }
        }
    }

    // Sanity: the link scan must have FOUND links (the docs are link-heavy). A zero
    // here means the extractor regressed and the gate would pass vacuously.
    assert!(
        links_checked >= 10,
        "the markdown link scan found only {links_checked} relative link(s) across {} docs — \
         `relative_link_targets` likely regressed (the `[text](path)` shape changed).",
        md_files.len()
    );

    assert!(
        broken_links.is_empty() && stale_tokens.is_empty(),
        "freeform stdlib Markdown docs drifted ({} broken link(s), {} stale token(s)).\n\
         BROKEN LINKS (a relative `[text](path)` points nowhere — fix the path or remove the link):\n  {}\n\
         STALE TOKENS (an unambiguous MOVED path survives — `kits/` -> `backend/`/`frontend/`, \
         `lib/syntax/` -> `syntax/`, `lib/passes/` -> `semantics/passes/`):\n  {}",
        broken_links.len(),
        stale_tokens.len(),
        if broken_links.is_empty() {
            "(none)".to_string()
        } else {
            broken_links.join("\n  ")
        },
        if stale_tokens.is_empty() {
            "(none)".to_string()
        } else {
            stale_tokens.join("\n  ")
        }
    );
}

// ── (#33) stale v1 nomenclature outside the stdlib `.md` gate ──────────────────
// The gate above only sweeps `stdlib/**/*.md`. The same MOVED-path tokens also
// survived in the freeform docs (`docs/`, `book/`), in `.caap` source comments,
// and in root files — invisible to a stdlib-only scan. This second gate closes
// that gap over the WHOLE published `.md` + `.caap` corpus.

/// Files that LEGITIMATELY record the old names (a deliberate historical record,
/// not drift) and so are exempt from the `.caap`/freeform-doc stale-token sweep.
const STALE_TOKEN_FILE_ALLOWLIST: &[&str] = &[
    "MIGRATION.md",                              // the v1->v2 migration log
    "docs/mechanisms/provider-pass-pipeline.md", // preserved historical-context note
    "KERNEL_REFERENCE.md", // names a deleted v1 file to say "That file is gone"
    "docs/design-partial-evaluation.md", // opens with an explicit "v1 refs below are intentional" note
];

/// A stale token counts only at a LEFT WORD BOUNDARY (the preceding char is not
/// `[A-Za-z0-9_]`), so the live `stdlib/syntax/` — which textually CONTAINS
/// `lib/syntax/` — and words like `toolkits/` are not false positives. All tokens
/// are ASCII, so byte indexing is safe even on the Cyrillic-heavy docs.
fn line_has_stale_token(line: &str, tok: &str) -> bool {
    let bytes = line.as_bytes();
    let tb = tok.as_bytes();
    if tb.is_empty() || bytes.len() < tb.len() {
        return false;
    }
    for i in 0..=bytes.len() - tb.len() {
        if &bytes[i..i + tb.len()] == tb {
            let boundary =
                i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
            if boundary {
                return true;
            }
        }
    }
    false
}

/// Recursively collect files with one of `exts` under `dir` (absolute, sorted).
fn collect_files_with_ext(dir: &std::path::Path, exts: &[&str]) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    if !dir.exists() {
        return found;
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).expect("read dir") {
            let path = entry.expect("dir entry").path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| exts.contains(&e))
            {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Every published `.md` + `.caap` source the stale-token gate covers: the freeform
/// docs (`docs/`, `book/`), the whole `stdlib/` tree, the `tools/` + `examples/`
/// programs, and root-level files. `.agents/` (an internal, separately-governed
/// surface) and `.rs` (where this gate's OWN denylist literal lives) are excluded.
fn collect_stale_scan_files() -> Vec<std::path::PathBuf> {
    let repo_root = std::path::PathBuf::from(stdlib_path(""))
        .parent()
        .expect("stdlib has a parent (repo root)")
        .to_path_buf();
    let mut files = Vec::new();
    for sub in ["docs", "book", "stdlib", "tools", "examples"] {
        files.extend(collect_files_with_ext(
            &repo_root.join(sub),
            &["md", "caap"],
        ));
    }
    if let Ok(rd) = std::fs::read_dir(&repo_root) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e == "md" || e == "caap")
            {
                files.push(p);
            }
        }
    }
    files.sort();
    files.dedup();
    files
}

/// (#33) No stale v1 path nomenclature survives in any published doc or `.caap`
/// source — the moved dirs `kits/` -> `backend/`+`frontend/`, `lib/syntax/` ->
/// `syntax/`, `lib/passes/` -> `semantics/passes/`. Closes the gap the
/// `stdlib/**/*.md`-only gate above left, so a public reader never meets a path
/// that the v1->v2 migration relocated. Pure-Rust, runs by default.
#[test]
fn no_stale_v1_nomenclature_in_docs_and_caap() {
    let files = collect_stale_scan_files();
    assert!(
        files.len() >= 50,
        "expected the docs+stdlib source corpus to be enumerated; found only {}",
        files.len()
    );
    let mut hits: Vec<String> = Vec::new();
    for f in &files {
        let rel = repo_relative(f);
        if STALE_TOKEN_FILE_ALLOWLIST.contains(&rel.as_str()) {
            continue;
        }
        let src =
            std::fs::read_to_string(f).unwrap_or_else(|e| panic!("read {}: {e}", f.display()));
        for (lineno, line) in src.lines().enumerate() {
            for tok in STALE_DOC_TOKENS {
                if line_has_stale_token(line, tok) {
                    hits.push(format!("{rel}:{}: stale path token `{tok}`", lineno + 1));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "stale v1 path nomenclature survives in {} place(s) — these dirs MOVED in the v1->v2 \
         migration (`kits/` -> `backend/`+`frontend/`, `lib/syntax/` -> `syntax/`, `lib/passes/` \
         -> `semantics/passes/`). Fix each, or if it is a deliberate historical record add the \
         file to STALE_TOKEN_FILE_ALLOWLIST:\n  {}",
        hits.len(),
        hits.join("\n  ")
    );
}
