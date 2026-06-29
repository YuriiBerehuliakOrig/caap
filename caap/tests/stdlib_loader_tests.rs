//! Module loader scenarios: in-language test discovery, malformed directives,
//! lib loading, failed-load retry, and project manifests.
use caap_core::RuntimeValue;

mod common;

use common::{
    corpus_path, eval_err_msg, eval_err_msg_fs, eval_ok, eval_ok_fs, stdlib_path, with_stdlib_root,
};

/// The three metadata fields parsed from a test file's leading comment header.
/// Each is a comment-only line of the form `;; @area: <value>` (likewise `@kind`
/// / `@slow`) placed near the top of every `test_*.caap`. They are pure metadata
/// (CAAP comments) — see `stdlib/lib/tests/README.md`.
#[derive(Default)]
struct TestHeader {
    area: Option<String>,
    kind: Option<String>,
    slow: Option<String>,
}

/// Parse the `@area` / `@kind` / `@slow` metadata header from a `test_*.caap`
/// file with PURE Rust (no CAAP eval): scan the leading comment block and extract
/// the three `;; @key: value` lines. We scan the first 30 lines because a file's
/// description comment can run long (the deepest header in the corpus today sits
/// at line 25), and stop at the first non-comment line — the header always
/// precedes the first executable `(use …)` form. Values are trimmed; the caller
/// matches them case-insensitively.
fn parse_test_header(path: &str) -> TestHeader {
    let mut header = TestHeader::default();
    let Ok(contents) = std::fs::read_to_string(path) else {
        return header;
    };
    for line in contents.lines().take(30) {
        let trimmed = line.trim_start();
        // The header block is a run of comment lines; the first non-comment,
        // non-blank line ends it (and the header is always above the code).
        if !trimmed.is_empty() && !trimmed.starts_with(';') {
            break;
        }
        let Some(rest) = trimmed.strip_prefix(";; @") else {
            continue;
        };
        let Some((key, value)) = rest.split_once(':') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            "area" if header.area.is_none() => header.area = Some(value),
            "kind" if header.kind.is_none() => header.kind = Some(value),
            "slow" if header.slow.is_none() => header.slow = Some(value),
            _ => {}
        }
    }
    header
}

/// The in-language test RUNNER: discover every `test_*.caap` under lib/ and
/// examples/ (NOT fixtures/) and run each by loading it. Adding a new test file
/// requires zero Rust changes. A failing assertion throws and names the file.
///
/// METADATA SELECTOR: each test file carries a `;; @area:` / `;; @kind:` /
/// `;; @slow:` comment header (see `stdlib/lib/tests/README.md`). Setting any of
/// `CAAP_TEST_AREA`, `CAAP_TEST_KIND`, `CAAP_TEST_SLOW` runs only the files whose
/// header matches ALL set filters (exact, case-insensitive). With NO filter env
/// var set, the FULL corpus runs exactly as before (same file set, same order) —
/// the selector is strictly additive on the default path.
#[test]
#[ignore = "acceptance: runs the full in-language stdlib test corpus"]
fn stdlib_run_all_in_language_tests() {
    let mut found = Vec::new();
    for dir in [stdlib_path("lib"), corpus_path("examples")] {
        let root = std::path::PathBuf::from(dir);
        let mut stack = vec![root];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).expect("read stdlib dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("test_") && n.ends_with(".caap"))
                {
                    found.push(path.display().to_string());
                }
            }
        }
    }
    assert!(
        found.len() >= 4,
        "expected the known in-language test files; found {found:?}"
    );

    // Enforce the metadata convention: every discovered test file MUST carry a
    // parseable `@area` header (the one field present on the whole corpus). A new
    // `test_*.caap` without one fails here, so the headers cannot silently rot.
    // (`@kind`/`@slow` are documented too, but only `@area` is hard-required so
    // the gate stays robust to a future file that omits the optional pair.)
    let headers: Vec<(String, TestHeader)> = found
        .iter()
        .map(|p| (p.clone(), parse_test_header(p)))
        .collect();
    let missing: Vec<&str> = headers
        .iter()
        .filter(|(_, h)| h.area.is_none())
        .map(|(p, _)| p.as_str())
        .collect();
    assert!(
        missing.is_empty(),
        "every test_*.caap must declare a `;; @area:` metadata header \
         (see stdlib/lib/tests/README.md); missing in: {missing:?}"
    );

    // The metadata-driven SELECTOR. Each env var, when set, narrows the run to
    // files whose corresponding header value matches (exact, case-insensitive).
    // Filters AND together; an unset filter is ignored. No env var set => the
    // full corpus (filtering below is a no-op, preserving today's behavior).
    let want_area = std::env::var("CAAP_TEST_AREA").ok();
    let want_kind = std::env::var("CAAP_TEST_KIND").ok();
    let want_slow = std::env::var("CAAP_TEST_SLOW").ok();
    let filtering = want_area.is_some() || want_kind.is_some() || want_slow.is_some();
    let matches = |want: &Option<String>, have: &Option<String>| match want {
        None => true,
        Some(w) => have
            .as_ref()
            .is_some_and(|h| h.eq_ignore_ascii_case(w.trim())),
    };

    let total = headers.len();
    let selected: Vec<String> = headers
        .into_iter()
        .filter(|(_, h)| {
            matches(&want_area, &h.area)
                && matches(&want_kind, &h.kind)
                && matches(&want_slow, &h.slow)
        })
        .map(|(p, _)| p)
        .collect();

    // Be transparent about a filtered run: log what was selected vs skipped so a
    // subset run never looks like a silently truncated full run.
    if filtering {
        eprintln!(
            "stdlib_run_all_in_language_tests: metadata filter active \
             (CAAP_TEST_AREA={want_area:?} CAAP_TEST_KIND={want_kind:?} \
             CAAP_TEST_SLOW={want_slow:?}) — selected {selected} of {total} test files",
            selected = selected.len(),
        );
        assert!(
            !selected.is_empty(),
            "metadata filter matched 0 of {total} test files — no test would run; \
             check CAAP_TEST_AREA/KIND/SLOW against the headers in stdlib/lib/tests/"
        );
    }

    for path in selected {
        eval_ok(&path, &with_stdlib_root(&format!("(load {path:?})")));
    }
}

/// A failing assertion reports the label AND expected/actual values.
#[test]
fn stdlib_in_language_test_reports_failure_with_values() {
    let fail = corpus_path("fixtures/test_fail.caap");
    let msg = eval_err_msg(
        "stdlib_test_fail",
        &with_stdlib_root(&format!("(load {fail:?})")),
    );
    assert!(msg.contains("FAIL: one equals two"), "msg: {msg}");
    assert!(
        msg.contains("expected 2") && msg.contains("got 1"),
        "carries expected/actual values: {msg}"
    );
}

/// Malformed directives fail with a LOCATED usage error (not a cryptic deep
/// failure): `(import mod)` without an alias names the file, line, and usage.
#[test]
fn stdlib_malformed_directive_reports_location_and_usage() {
    let bad = corpus_path("fixtures/bad_import_shape.caap");
    let msg = eval_err_msg(
        "stdlib_bad_directive",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("malformed (import"),
        "names the directive: {msg}"
    );
    assert!(msg.contains("(import mod alias)"), "shows usage: {msg}");
    assert!(
        msg.contains("bad_import_shape.caap:"),
        "has file:line location: {msg}"
    );
}

/// A FAILED load no longer poisons the module: catch the error, fix the
/// world (here: register the missing transform), reload the SAME module in
/// the SAME session — the loader adopts and repopulates the stale
/// placeholder instead of handing back an empty map.
#[test]
fn stdlib_failed_load_can_be_retried_after_fixing() {
    let lower = corpus_path("fixtures/lower_plus.caap");
    let user = corpus_path("fixtures/uses_plus.caap");
    let v = eval_ok(
        "load_retry",
        &with_stdlib_root(&format!(
            "(bind ((first (try (load {user:?}) (catch e \"failed\"))))
               (do
                 (load {lower:?})
                 (bind ((m (load {user:?})))
                   (list_of first ((get m \"f\" null))))))"
        )),
    );
    let RuntimeValue::List(items) = v else {
        panic!("expected list, got {v:?}")
    };
    let items = items.borrow();
    assert_eq!(
        items[0],
        RuntimeValue::Str("failed".into()),
        "first attempt fails"
    );
    assert_eq!(items[1], RuntimeValue::Int(42), "retry rebuilds and works");
}

/// A project dependency cycle is a hard error with the chain in the message.
#[test]
fn stdlib_project_cycle_rejected() {
    let manifest = corpus_path("fixtures/proj/cyc_a/project.caap");
    let msg = eval_err_msg(
        "project_cycle",
        &with_stdlib_root(&format!(
            "(bind ((pj (load_module \"stdlib.lib.project\")))
               ((get pj \"load_project\" null) {manifest:?}))"
        )),
    );
    assert!(msg.contains("project dependency cycle"), "msg: {msg}");
    assert!(
        msg.contains("cyc_a") && msg.contains("cyc_b"),
        "chain: {msg}"
    );
}

/// A manifest without a name fails with the manifest path in the message.
#[test]
fn stdlib_project_manifest_requires_name() {
    let manifest = corpus_path("fixtures/proj/noname/project.caap");
    let msg = eval_err_msg(
        "project_noname",
        &with_stdlib_root(&format!(
            "(bind ((pj (load_module \"stdlib.lib.project\")))
               ((get pj \"load_project\" null) {manifest:?}))"
        )),
    );
    assert!(
        msg.contains("\"name\" must be a non-empty string"),
        "msg: {msg}"
    );
    assert!(
        msg.contains("noname/project.caap"),
        "names the manifest: {msg}"
    );
}

/// Two different manifests claiming one project name are rejected.
#[test]
fn stdlib_project_name_conflict_rejected() {
    let a = corpus_path("fixtures/proj/dup_a/project.caap");
    let b = corpus_path("fixtures/proj/dup_b/project.caap");
    let msg = eval_err_msg(
        "project_dup",
        &with_stdlib_root(&format!(
            "(bind ((pj (load_module \"stdlib.lib.project\")))
               (do
                 ((get pj \"load_project\" null) {a:?})
                 ((get pj \"load_project\" null) {b:?})))"
        )),
    );
    assert!(msg.contains("already taken"), "msg: {msg}");
    assert!(msg.contains("dup_proj"), "names the project: {msg}");
}

/// Audit #13 regression: a manifest that throws mid-load must NOT leave its
/// path wedged on the `loading` stack — re-loading it in the same session
/// reports the SAME real error, never a spurious "dependency cycle".
#[test]
fn stdlib_project_failed_load_does_not_wedge_loading_stack() {
    let manifest = corpus_path("fixtures/proj/noname/project.caap");
    let msg = eval_err_msg(
        "project_reload_after_fail",
        &with_stdlib_root(&format!(
            "(bind ((pj (load_module \"stdlib.lib.project\")))
               (do
                 ; first attempt throws (no name) — swallow it
                 (try ((get pj \"load_project\" null) {manifest:?}) (catch e null))
                 ; second attempt must re-report the real error, not a fake cycle
                 ((get pj \"load_project\" null) {manifest:?})))"
        )),
    );
    assert!(
        msg.contains("\"name\" must be a non-empty string"),
        "re-load should re-report the real error, got: {msg}"
    );
    assert!(
        !msg.contains("dependency cycle"),
        "must NOT fake a cycle after a failed load: {msg}"
    );
}

/// Audit #2/#11/#12 regression: a file WITH a surface header naming an
/// unavailable kit must fail with a LOCATED diagnostic naming the surface kit
/// — never silently fall back to the default kernel reader (which would parse
/// the surface text as kernel forms and swallow the real failure).
///
/// Uses an `fs`-enabled session (see `eval_*_fs`): the loader reads the header
/// through the `fs` service, so surface dispatch only runs when that service is
/// present. A headerless file (read fine, no header) is the only correct
/// fall-through and is covered by the companion test below.
#[test]
fn stdlib_surface_header_unavailable_kit_reports_located_failure() {
    let bad = corpus_path("fixtures/bad_surface_kit.caap");
    let msg = eval_err_msg_fs(
        "surface_bad_kit",
        &with_stdlib_root(&format!("(load {bad:?})")),
    );
    assert!(
        msg.contains("surface kit \"stdlib.frontend.no_such_kit\""),
        "names the failing surface kit (not a default-reader error): {msg}"
    );
    assert!(
        msg.contains("could not be loaded"),
        "reports the kit-load failure: {msg}"
    );
    assert!(
        msg.contains("bad_surface_kit.caap"),
        "located on the offending file: {msg}"
    );
    assert!(
        !msg.contains("expected:") && !msg.contains("parse error"),
        "must NOT be a default-reader parse error (silent fall-through): {msg}"
    );
}

/// The companion to the above: a NORMAL headerless file still loads through
/// the default reader exactly as before — only the header-present-but-failed
/// case changed (it gained a diagnostic), headerless loads degrade gracefully.
/// Runs on the same `fs`-enabled session to prove the change is surgical: even
/// with the `fs` service present (so a header WOULD be read), a file without a
/// header still takes the default path and loads.
#[test]
fn stdlib_headerless_file_still_loads() {
    let ok = corpus_path("fixtures/headerless_value.caap");
    let v = eval_ok_fs(
        "headerless_ok",
        &with_stdlib_root(&format!("(load {ok:?})")),
    );
    assert_eq!(
        v,
        RuntimeValue::Int(42),
        "a headerless file loads unchanged via the default reader"
    );
}

/// The LSP `analyze_source` path has the same surface dispatch. A header naming
/// an unavailable kit must NOT silently degrade to the default analyzer (which
/// would mislead tooling by parsing the surface text as kernel); it returns a
/// weak-evidence diagnostic (no definitions) naming the surface failure.
#[test]
fn stdlib_analyze_surface_header_unavailable_kit_reports_diagnostic() {
    let bad = corpus_path("fixtures/bad_surface_kit.caap");
    let v = eval_ok_fs(
        "analyze_bad_surface",
        &with_stdlib_root(&format!(
            "(bind ((analyze (ctfe_compiler_lookup_value compiler \"stdlib.module.analyze_source\")))
               (bind ((r (analyze {bad:?})))
                 (list_of
                   (size (get r \"definitions\" (list_of)))
                   (sequence_join (get r \"diagnostics\" (list_of)) \"\\n\"))))"
        )),
    );
    let RuntimeValue::List(items) = v else {
        panic!("expected [defs_count, diagnostics], got {v:?}")
    };
    let items = items.borrow();
    assert_eq!(
        items[0],
        RuntimeValue::Int(0),
        "a failed surface analysis yields no (misleading) definitions"
    );
    let RuntimeValue::Str(diags) = &items[1] else {
        panic!("expected joined diagnostics string, got {:?}", items[1])
    };
    assert!(
        diags.contains("surface kit \"stdlib.frontend.no_such_kit\"")
            && diags.contains("could not analyze this file"),
        "reports the surface failure as a diagnostic (not a silent default-parse): {diags}"
    );
}

/// The companion: a headerless file still analyzes through the DEFAULT analyzer
/// — surface dispatch only intercepts header-present files, so an ordinary
/// (clean) kernel file analyzes with no diagnostics, exactly as before.
#[test]
fn stdlib_analyze_headerless_file_uses_default() {
    let ok = corpus_path("fixtures/headerless_value.caap");
    let v = eval_ok_fs(
        "analyze_headerless",
        &with_stdlib_root(&format!(
            "(bind ((analyze (ctfe_compiler_lookup_value compiler \"stdlib.module.analyze_source\")))
               (bind ((r (analyze {ok:?})))
                 (size (get r \"diagnostics\" (list_of)))))"
        )),
    );
    assert_eq!(
        v,
        RuntimeValue::Int(0),
        "a clean headerless file analyzes with no diagnostics via the default path"
    );
}
