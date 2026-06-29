//! Opt-in DWARF debug info for native (LLVM) builds. The span substrate already
//! exists in the kernel; this is a pure stdlib + tools feature: the tools parse
//! a `debug[=line-tables|full]` positional, the driver threads a debug level
//! into the LLVM emitter, and the emitter writes textual `!DI…` metadata that
//! `clang -x ir` preserves into the object's `.debug_info` (no `-g` needed).
//!
//! Two layers of proof:
//!   1. `..._emit_has_di_nodes` / `..._no_debug_emit_is_clean` (always run):
//!      emit the IR for a real source file WITH and WITHOUT debug and assert the
//!      DI nodes are present / absent. The no-debug emit must be byte-identical
//!      to today's output (no `!dbg`, no `!DI…`, no `!llvm.` roots).
//!   2. `..._builds_with_debug_info` / `..._no_debug_build_is_clean` (acceptance;
//!      self-skip without clang): build the binary with and without debug and
//!      assert readelf shows `.debug_info` only for the debug build, the
//!      function symbol is present, and the debug binary STILL RUNS with the
//!      correct exit code (debug must never change behaviour).
use std::path::{Path, PathBuf};
use std::process::Command;

use caap_core::RuntimeValue;

mod common;

use common::{stdlib_bootstrap, stdlib_path, with_stdlib_root};

/// A tiny real program with a typed function and a `main` that calls it; its
/// exit code is 42. A REAL file (not synthesized specs) so the nodes carry
/// spans and line info is meaningful.
const PROGRAM_SRC: &str =
    "(defn add ((a i32)(b i32)) i32 (int_add a b))\n(bind main (lambda () (add 40 2)))\n(main)\n";

/// A program with a struct and a function over a pointer-to-struct, to exercise
/// the Phase-2 composite-type metadata (DICompositeType + DW_TAG_member +
/// DW_TAG_pointer_type). main returns Point{40,2}.x + 0 = 40... so exit 42 needs
/// x=42: build it to return 42 for the run check.
const STRUCT_SRC: &str = "(struct Point (x i32) (y i32))\n\
     (defn px ((p ptr_Point)) i32 (get p \"x\"))\n\
     (defn mk () i32 (bind ((p (ref (make_Point 42 2)))) (px p)))\n\
     (bind main (lambda () (mk)))\n\
     (main)\n";

/// A program with a `bind`-LOCAL (not a parameter) under a typed function, to
/// exercise the Phase-2 local-variable spill: `n` is an i32 local that should
/// become a !DILocalVariable WITHOUT an `arg:` slot (a DW_TAG_variable, not a
/// formal parameter) + a debug-only spill alloca + an llvm.dbg.declare. The
/// codegen path must be unchanged (the body still uses the SSA value).
/// f(40) = (40+1)+1 = 42, so main exits 42.
const BIND_LOCAL_SRC: &str = "(defn f ((a i32)) i32 \
     (bind ((n (int_add a 1))) (int_add n 1)))\n\
     (bind main (lambda () (f 40)))\n\
     (main)\n";

/// A program with an ARRAY local: `a` is a stack `[4 x i32]` buffer that decays
/// to a typed pointer for codegen, but under full debug should be described as a
/// genuine DW_TAG_array_type (baseType i32, DISubrange count 4) over its own
/// buffer — NOT as a decayed pointer and NOT with a debug spill. The program
/// writes 42 into element 0 and reads it back, so `fill`/`main` exit 42.
const ARRAY_LOCAL_SRC: &str = "(defn fill () i32 \
     (bind ((a (array_local 4 i32))) (do (ptr_write a 42) (ptr_read a))))\n\
     (bind main (lambda () (fill)))\n\
     (main)\n";

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

fn caap_binary() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    ["../target/debug/caap", "../target/release/caap"]
        .iter()
        .map(|r| manifest.join(r))
        .find(|p| p.is_file())
        .expect("caap binary (build the workspace first)")
}

/// Emit `src` through stdlib's own LLVM backend at the given `debug_level`
/// ("none" | "line-tables" | "full") and return the IR text. Diagnostics fail.
fn emit_llvm(name: &str, src: &str, debug_level: &str) -> String {
    let path = std::env::temp_dir().join(format!("{name}_{}.caap", std::process::id()));
    std::fs::write(&path, src).expect("write source");
    let p = path.to_str().unwrap().to_string();
    let v = common::eval_ok(
        name,
        &with_stdlib_root(&format!(
            "(bind ((res
                ((get (load_module \"stdlib.backend.emit.llvm\") \"emit_program_debug\" null)
                 (ctfe_compiler_load_surface_file_template compiler {p:?})
                 {debug_level:?})))
               (list_of
                 (get res \"text\" null)
                 (size (get res \"diagnostics\" (list_of)))
                 (sequence_join
                   (sequence_map (get res \"diagnostics\" (list_of))
                     (lambda (d) (get d \"message\" \"?\")))
                   \"; \")))"
        )),
    );
    std::fs::remove_file(&path).ok();
    let RuntimeValue::List(items) = v else {
        panic!("expected list, got {v:?}");
    };
    let items = items.borrow();
    let (RuntimeValue::Str(text), RuntimeValue::Int(n)) = (&items[0], &items[1]) else {
        panic!("unexpected emit result shape: {items:?}");
    };
    assert_eq!(*n, 0, "emit diagnostics: {}", items[2]);
    text.to_string()
}

/// `full` debug info adds every DWARF metadata kind the line-table layer needs,
/// plus a per-instruction `!dbg`. The emitted IR must carry them.
#[test]
fn dwarf_full_emit_has_di_nodes() {
    let ir = emit_llvm("dwarf_emit_full", PROGRAM_SRC, "full");

    for node in [
        "!DIFile(",
        "!DICompileUnit(",
        "!DISubprogram(",
        "!DISubroutineType(",
        "!DIBasicType(",
        "!DILocation(",
        "!llvm.module.flags",
        "!llvm.dbg.cu",
        "Debug Info Version",
    ] {
        assert!(ir.contains(node), "debug IR should contain `{node}`:\n{ir}");
    }
    // the functions are tagged with their subprogram, instructions with a loc
    assert!(
        ir.contains("@add(i32 %a, i32 %b) !dbg !"),
        "@add's define line should carry !dbg:\n{ir}"
    );
    assert!(
        ir.contains(", !dbg !"),
        "instructions should carry a !dbg location:\n{ir}"
    );
    // the function lines come from the source spans (add on line 1, main line 2)
    assert!(
        ir.contains("name: \"add\"") && ir.contains("line: 1"),
        "add should be described at line 1:\n{ir}"
    );
    assert!(
        ir.contains("name: \"main\"") && ir.contains("line: 2"),
        "main should be described at line 2:\n{ir}"
    );
}

/// Phase 2 — "full" debug additionally emits parameter locals (a
/// !DILocalVariable + an llvm.dbg.declare over a spill slot) and composite type
/// metadata (struct -> DICompositeType + DW_TAG_member; pointer ->
/// DW_TAG_pointer_type). The spill is debug-only: the codegen path is unchanged.
#[test]
fn dwarf_full_emit_has_locals_and_composite_types() {
    // scalar params -> locals
    let ir = emit_llvm("dwarf_emit_locals", PROGRAM_SRC, "full");
    assert!(
        ir.contains("@llvm.dbg.declare"),
        "full debug should emit llvm.dbg.declare for params:\n{ir}"
    );
    assert!(
        ir.contains("!DILocalVariable(name: \"a\", arg: 1"),
        "param `a` should be a DILocalVariable(arg: 1):\n{ir}"
    );
    assert!(
        ir.contains("!DILocalVariable(name: \"b\", arg: 2"),
        "param `b` should be a DILocalVariable(arg: 2):\n{ir}"
    );
    // the spill leaves the real arithmetic intact (codegen unchanged)
    assert!(
        ir.contains("%t1 = add i32 %a, %b"),
        "the debug spill must not change the arithmetic:\n{ir}"
    );

    // struct + pointer params -> composite types
    let irs = emit_llvm("dwarf_emit_composite", STRUCT_SRC, "full");
    assert!(
        irs.contains("DW_TAG_structure_type") && irs.contains("name: \"Point\""),
        "the struct should be a DICompositeType:\n{irs}"
    );
    assert!(
        irs.contains("DW_TAG_member") && irs.contains("name: \"x\""),
        "the struct fields should be DW_TAG_member derived types:\n{irs}"
    );
    assert!(
        irs.contains("DW_TAG_pointer_type"),
        "the ptr_Point parameter should be a DW_TAG_pointer_type:\n{irs}"
    );
}

/// Phase 2 (A2) — a `bind`-LOCAL (not a parameter) under "full" debug becomes a
/// !DILocalVariable with NO `arg:` slot (a DW_TAG_variable, not a formal
/// parameter), paired with a debug-only spill alloca + an llvm.dbg.declare. The
/// codegen path must be unchanged: the arithmetic over the SSA value is intact.
#[test]
fn dwarf_full_emit_has_bind_local() {
    let ir = emit_llvm("dwarf_emit_bind_local", BIND_LOCAL_SRC, "full");
    // a local variable: a DILocalVariable named `n` WITHOUT an `arg:` field
    // (parameters carry `arg: N`; a local omits it). Match the full prefix up to
    // the next field so the assertion can't be satisfied by a parameter node.
    assert!(
        ir.contains("!DILocalVariable(name: \"n\", scope:"),
        "the bind-local `n` should be a DILocalVariable with no `arg:` slot:\n{ir}"
    );
    // ...and it is NOT mistakenly emitted as a parameter
    assert!(
        !ir.contains("!DILocalVariable(name: \"n\", arg:"),
        "the bind-local `n` must NOT carry an `arg:` slot:\n{ir}"
    );
    // a debug-only spill alloca + store + dbg.declare back the local. The slot
    // name is `%n.dbg<N>` (a unique suffix, so a shadowed local can't collide).
    assert!(
        ir.contains("%n.dbg") && ir.contains(" = alloca i32"),
        "the local should have a debug-only spill alloca:\n{ir}"
    );
    assert!(
        ir.contains("store i32 %t1, ptr %n.dbg"),
        "the SSA local value should be stored into its spill slot:\n{ir}"
    );
    assert!(
        ir.contains("call void @llvm.dbg.declare(metadata ptr %n.dbg"),
        "the spill slot should be llvm.dbg.declare'd:\n{ir}"
    );
    // the codegen path is UNCHANGED: the real arithmetic over the SSA value
    // (%t1 = a+1, then +1) still drives the result — the spill is additive
    assert!(
        ir.contains("%t1 = add i32 %a, 1") && ir.contains("add i32 %t1, 1"),
        "the debug spill must not change the arithmetic:\n{ir}"
    );

    // a SHADOWED local (two `x`s in nested binds) must not produce two allocas
    // with the same SSA name (LLVM rejects that) — the unique suffix prevents it
    let shadow = "(defn g ((a i32)) i32 \
         (bind ((x (int_add a 1))) (bind ((x (int_add x 1))) x)))\n\
         (bind main (lambda () (g 40)))\n(main)\n";
    let irg = emit_llvm("dwarf_emit_bind_shadow", shadow, "full");
    let allocas: Vec<&str> = irg
        .lines()
        .filter(|l| l.contains(".dbg") && l.contains("= alloca"))
        .collect();
    assert_eq!(
        allocas.len(),
        3,
        "two shadowed `x` locals + the param `a` spill = 3 distinct allocas:\n{irg}"
    );
    // all spill-slot names are distinct (no duplicate SSA value name)
    let mut names: Vec<&str> = allocas
        .iter()
        .map(|l| l.trim().split(' ').next().unwrap())
        .collect();
    names.sort_unstable();
    let n_before = names.len();
    names.dedup();
    assert_eq!(
        names.len(),
        n_before,
        "shadowed-local spill slots must have unique names:\n{irg}"
    );
}

/// Phase 2 (W3) — an ARRAY local under "full" debug is described as a genuine
/// DW_TAG_array_type (a !DICompositeType with a DISubrange bound + an i32 base),
/// declared DIRECTLY over the `[N x T]` buffer with NO debug spill alloca/store.
/// The codegen path must be byte-identical: the array storage, the element store
/// and the load are untouched (the array shape is carried for the DWARF path
/// only).
#[test]
fn dwarf_full_emit_has_array_local() {
    let ir = emit_llvm("dwarf_emit_array_local", ARRAY_LOCAL_SRC, "full");

    // the array local `a` is a DW_TAG_array_type composite, not a pointer
    assert!(
        ir.contains("!DICompositeType(tag: DW_TAG_array_type"),
        "the array local should be a DW_TAG_array_type:\n{ir}"
    );
    // a DISubrange gives the bound (count: 4) and the base type is i32
    assert!(
        ir.contains("!DISubrange(count: 4)"),
        "the array type should carry a DISubrange(count: 4):\n{ir}"
    );
    assert!(
        ir.contains("baseType:") && ir.contains("!DIBasicType(name: \"i32\""),
        "the array element type should be a DIBasicType(i32):\n{ir}"
    );
    // size is N*elemsize bits = 4*32 = 128
    assert!(
        ir.contains("DW_TAG_array_type, baseType: !") && ir.contains(", size: 128,"),
        "the array type size should be 4*32 = 128 bits:\n{ir}"
    );
    // the local `a` is a DILocalVariable typed by the array composite (a local,
    // no `arg:` slot)
    assert!(
        ir.contains("!DILocalVariable(name: \"a\", scope:"),
        "the array local `a` should be a DILocalVariable with no `arg:` slot:\n{ir}"
    );
    assert!(
        !ir.contains("!DILocalVariable(name: \"a\", arg:"),
        "the array local `a` must NOT carry an `arg:` slot:\n{ir}"
    );
    // it is declared DIRECTLY over the [4 x i32] buffer (`%t1`), NOT a debug
    // spill: there is NO `%a.dbg` slot for an array local
    assert!(
        ir.contains("@llvm.dbg.declare(metadata ptr %t1,"),
        "the array should be dbg.declare'd over its own buffer (%t1):\n{ir}"
    );
    assert!(
        !ir.contains("%a.dbg"),
        "an array local must NOT get a debug spill alloca (declared over the buffer):\n{ir}"
    );
    // codegen is unchanged: the buffer alloca + the element store + load are
    // exactly the no-debug shape (the array shape is DWARF-only)
    assert!(
        ir.contains("%t1 = alloca [4 x i32]"),
        "the [4 x i32] buffer alloca must be intact:\n{ir}"
    );
    assert!(
        ir.contains("store i32 42, ptr %t1") && ir.contains("%t2 = load i32, ptr %t1"),
        "the element store/load over the buffer must be unchanged:\n{ir}"
    );
}

/// The no-debug emit of an ARRAY-local program is byte-clean: the array shape is
/// carried on the value record for the DWARF path only, so with debug off there
/// is no DI metadata, no spill, and the buffer/store/load are byte-identical.
#[test]
fn dwarf_no_debug_array_local_emit_is_clean() {
    let ir = emit_llvm("dwarf_emit_none_array", ARRAY_LOCAL_SRC, "none");
    assert!(
        !ir.contains("!dbg")
            && !ir.contains("!DI")
            && !ir.contains("DW_TAG_array_type")
            && !ir.contains("llvm.dbg.declare"),
        "no-debug IR of an array-local program must be clean:\n{ir}"
    );
    assert!(
        ir.contains("%t1 = alloca [4 x i32]")
            && ir.contains("store i32 42, ptr %t1")
            && ir.contains("define i32 @fill("),
        "no-debug IR should still lower the array buffer + store:\n{ir}"
    );
}

/// "line-tables" is the line-info-only leg: it must NOT emit locals or
/// composite types (those belong to "full").
#[test]
fn dwarf_line_tables_has_no_locals() {
    let ir = emit_llvm("dwarf_lines_no_locals", PROGRAM_SRC, "line-tables");
    assert!(
        !ir.contains("llvm.dbg.declare") && !ir.contains("DILocalVariable"),
        "line-tables must not emit locals:\n{ir}"
    );
    // the bind-local spill is also a "full"-only feature — line-tables of a
    // program WITH a bind-local still emits no locals / no spill alloca
    let ir2 = emit_llvm("dwarf_lines_no_bind_local", BIND_LOCAL_SRC, "line-tables");
    assert!(
        !ir2.contains("llvm.dbg.declare")
            && !ir2.contains("DILocalVariable")
            && !ir2.contains(".dbg = alloca"),
        "line-tables must not spill bind-locals:\n{ir2}"
    );
    // an array local is a "full"-only DW_TAG_array_type too — line-tables of an
    // array-local program emits no array type / no local
    let ir3 = emit_llvm("dwarf_lines_no_array_local", ARRAY_LOCAL_SRC, "line-tables");
    assert!(
        !ir3.contains("llvm.dbg.declare")
            && !ir3.contains("DILocalVariable")
            && !ir3.contains("DW_TAG_array_type"),
        "line-tables must not describe array locals:\n{ir3}"
    );
}

/// "line-tables" still emits the CU/file/subprogram/location scaffold (the
/// primary deliverable: source-line breakpoints + function-named backtraces).
#[test]
fn dwarf_line_tables_emit_has_di_nodes() {
    let ir = emit_llvm("dwarf_emit_lines", PROGRAM_SRC, "line-tables");
    for node in [
        "!DICompileUnit(",
        "!DISubprogram(",
        "!DILocation(",
        "!llvm.dbg.cu",
    ] {
        assert!(
            ir.contains(node),
            "line-table IR should contain `{node}`:\n{ir}"
        );
    }
}

/// The no-debug emit MUST be byte-identical to today's output: no `!dbg`, no
/// `!DI…`, no `!llvm.` named-metadata roots.
#[test]
fn dwarf_no_debug_emit_is_clean() {
    let ir = emit_llvm("dwarf_emit_none", PROGRAM_SRC, "none");
    assert!(!ir.contains("!dbg"), "no-debug IR must have no !dbg:\n{ir}");
    assert!(
        !ir.contains("!DI"),
        "no-debug IR must have no !DI nodes:\n{ir}"
    );
    assert!(
        !ir.contains("!llvm."),
        "no-debug IR must have no !llvm. metadata roots:\n{ir}"
    );
    // it still really lowered both functions
    assert!(
        ir.contains("define i32 @add(") && ir.contains("define i32 @main("),
        "no-debug IR should still contain the functions:\n{ir}"
    );

    // a program WITH a bind-local is also byte-clean with debug off: no spill
    // alloca, no dbg.declare, no DI metadata — the codegen path is untouched
    let irb = emit_llvm("dwarf_emit_none_bind", BIND_LOCAL_SRC, "none");
    assert!(
        !irb.contains("!dbg")
            && !irb.contains("!DI")
            && !irb.contains(".dbg = alloca")
            && !irb.contains("llvm.dbg.declare"),
        "no-debug IR of a bind-local program must be clean:\n{irb}"
    );
    assert!(
        irb.contains("%t1 = add i32 %a, 1") && irb.contains("define i32 @f("),
        "no-debug IR should still lower the bind-local arithmetic:\n{irb}"
    );
}

/// Build a binary from `src` at `debug_level` via tools/s2_build.caap, run it,
/// and return (binary_path, exit_code). Panics if the build fails. The caller
/// removes the binary after inspecting it.
fn build_and_run(name: &str, src: &str, debug_level: Option<&str>) -> (PathBuf, Option<i32>) {
    let tmp = std::env::temp_dir().join(format!("dwarf-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("mktmp");
    let composed = tmp.join("native_composed.caap");
    std::fs::write(
        &composed,
        format!(
            "(do\n  (ctfe_compiler_execute_bootstrap_file compiler {:?})\n  (ctfe_compiler_execute_bootstrap_file compiler {:?}))\n",
            stdlib_bootstrap(),
            stdlib_path("boot/native_emit.caap"),
        ),
    )
    .expect("write composed");

    let prog = tmp.join("prog.caap");
    std::fs::write(&prog, src).expect("write program");

    // keep the binary OUTSIDE tmp so it survives the dir cleanup for readelf+run
    let bin = std::env::temp_dir().join(format!("dwarf-{name}-bin-{}", std::process::id()));

    let caap = caap_binary();
    let mut cmd = Command::new(&caap);
    cmd.arg(&composed)
        .arg(stdlib_path("../tools/s2_build.caap"))
        .arg(&prog)
        .arg(&bin);
    if let Some(level) = debug_level {
        cmd.arg(format!("debug={level}"));
    }
    let build = cmd.output().expect("run s2_build");
    let built = build.status.success() && bin.is_file();
    let build_stderr = String::from_utf8_lossy(&build.stderr).into_owned();
    if !built {
        std::fs::remove_dir_all(&tmp).ok();
        panic!("native build failed:\nstderr: {build_stderr}");
    }
    // run BEFORE the tmp dir cleanup (the binary itself lives outside tmp)
    let run_code = Command::new(&bin)
        .output()
        .expect("run binary")
        .status
        .code();
    std::fs::remove_dir_all(&tmp).ok();
    (bin, run_code)
}

/// `readelf -S BIN` shows a `.debug_info` section (the binary carries DWARF).
fn has_debug_info(bin: &Path) -> bool {
    let out = Command::new("readelf")
        .arg("-S")
        .arg(bin)
        .output()
        .expect("run readelf");
    String::from_utf8_lossy(&out.stdout).contains(".debug_info")
}

/// A FULL debug build carries `.debug_info`, the `add` function symbol survives,
/// and the binary STILL RUNS with the correct exit code (42) — debug info must
/// never change behaviour.
#[test]
#[ignore = "acceptance: builds a native binary WITH DWARF debug info (clang) and asserts .debug_info + correct exit code"]
fn dwarf_full_build_has_debug_info_and_runs() {
    if !on_path("clang") {
        eprintln!("skipping: clang unavailable");
        return;
    }
    let (bin, run_code) = build_and_run("full", PROGRAM_SRC, Some("full"));

    let dbg = has_debug_info(&bin);

    // the function symbol is present (function-named backtraces)
    let nm = Command::new("nm").arg(&bin).output();
    let has_add = nm
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.ends_with(" T add"))
        })
        .unwrap_or(true); // nm absent -> don't fail on the symbol check

    std::fs::remove_file(&bin).ok();

    assert!(dbg, "debug build must have a .debug_info section");
    assert!(has_add, "the `add` function symbol should be present");
    assert_eq!(
        run_code,
        Some(42),
        "the debug binary must still run with the correct exit code (42)"
    );
}

/// The no-debug build carries NO `.debug_info` (byte-identical behaviour to
/// today) and runs with the correct exit code.
#[test]
#[ignore = "acceptance: builds a native binary WITHOUT debug info (clang) and asserts no .debug_info"]
fn dwarf_no_debug_build_is_clean_and_runs() {
    if !on_path("clang") {
        eprintln!("skipping: clang unavailable");
        return;
    }
    let (bin, run_code) = build_and_run("none", PROGRAM_SRC, None);
    let dbg = has_debug_info(&bin);
    std::fs::remove_file(&bin).ok();

    assert!(
        !dbg,
        "the no-debug build must NOT have a .debug_info section"
    );
    assert_eq!(
        run_code,
        Some(42),
        "the no-debug binary must run with the correct exit code (42)"
    );
}

/// `objdump --dwarf=info BIN` output (empty when objdump is absent).
fn dwarf_info(bin: &Path) -> String {
    Command::new("objdump")
        .arg("--dwarf=info")
        .arg(bin)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Phase 2 end-to-end: a FULL debug build of a struct program emits the
/// parameter + composite DIEs into the binary's `.debug_info` (objdump shows
/// DW_TAG_formal_parameter and DW_TAG_structure_type), and STILL RUNS with the
/// correct exit code (42) — locals/composite metadata must never change
/// behaviour. objdump-dependent assertions self-skip if objdump is absent.
#[test]
#[ignore = "acceptance: builds a struct program with FULL DWARF (clang) and asserts parameter/composite DIEs + correct exit code"]
fn dwarf_full_struct_build_has_variable_dies_and_runs() {
    if !on_path("clang") {
        eprintln!("skipping: clang unavailable");
        return;
    }
    let (bin, run_code) = build_and_run("struct", STRUCT_SRC, Some("full"));
    assert!(
        has_debug_info(&bin),
        "the full-debug struct build must have .debug_info"
    );
    if on_path("objdump") {
        let info = dwarf_info(&bin);
        assert!(
            info.contains("DW_TAG_formal_parameter"),
            "the DWARF should describe the function parameter:\n{info}"
        );
        assert!(
            info.contains("DW_TAG_structure_type"),
            "the DWARF should describe the Point struct:\n{info}"
        );
    } else {
        eprintln!("objdump absent: skipping the DIE-tree assertions");
    }
    std::fs::remove_file(&bin).ok();
    assert_eq!(
        run_code,
        Some(42),
        "the full-debug struct binary must run with the correct exit code (42)"
    );
}

/// Phase 2 (A2) end-to-end: a FULL debug build of a program with a `bind`-LOCAL
/// emits a DW_TAG_variable (the local `n`, NOT a formal parameter) into
/// `.debug_info` and STILL RUNS with the correct exit code (42). When lldb is
/// present, `frame variable` inside `f` shows the local `n`. The local-variable
/// metadata must never change behaviour. objdump/lldb assertions self-skip when
/// the tool is absent.
#[test]
#[ignore = "acceptance: builds a bind-local program with FULL DWARF (clang) and asserts DW_TAG_variable + correct exit code"]
fn dwarf_full_bind_local_build_has_variable_die_and_runs() {
    if !on_path("clang") {
        eprintln!("skipping: clang unavailable");
        return;
    }
    let (bin, run_code) = build_and_run("bindlocal", BIND_LOCAL_SRC, Some("full"));
    assert!(
        has_debug_info(&bin),
        "the full-debug bind-local build must have .debug_info"
    );
    if on_path("objdump") {
        let info = dwarf_info(&bin);
        // the bind-local `n` is a DW_TAG_variable (a local), distinct from the
        // DW_TAG_formal_parameter that describes `f`'s parameter `a`
        assert!(
            info.contains("DW_TAG_variable"),
            "the DWARF should describe the bind-local as a DW_TAG_variable:\n{info}"
        );
        assert!(
            info.contains("DW_TAG_formal_parameter"),
            "the DWARF should still describe the parameter `a`:\n{info}"
        );
    } else {
        eprintln!("objdump absent: skipping the DIE-tree assertions");
    }
    // lldb: a breakpoint in `f` should let `frame variable` see the local `n`
    if on_path("lldb") {
        let out = Command::new("lldb")
            .args([
                "--batch",
                "-o",
                "breakpoint set --name f",
                "-o",
                "run",
                "-o",
                "frame variable",
                "-o",
                "quit",
            ])
            .arg(&bin)
            .output();
        if let Ok(out) = out {
            let text = String::from_utf8_lossy(&out.stdout);
            // best-effort: assert lldb knows the local `n` by name if it ran the
            // frame-variable command at all (it prints "(int) n = …"). Some lldb
            // builds need extra setup to stop; do not hard-fail if it could not.
            if text.contains("frame variable") || text.contains(") n ") {
                assert!(
                    text.contains(" n ") || text.contains(" n=") || text.contains("n ="),
                    "lldb frame variable should list the bind-local `n`:\n{text}"
                );
            } else {
                eprintln!("lldb did not stop at f / produce frame variables; skipping");
            }
        }
    } else {
        eprintln!("lldb absent: skipping the frame-variable check");
    }
    std::fs::remove_file(&bin).ok();
    assert_eq!(
        run_code,
        Some(42),
        "the full-debug bind-local binary must run with the correct exit code (42)"
    );
}

/// Phase 2 (W3) end-to-end: a FULL debug build of a program with an ARRAY LOCAL
/// emits a DW_TAG_array_type (the `[4 x i32]` local `a`) with its element bound
/// into `.debug_info` and STILL RUNS with the correct exit code (42). The array
/// metadata must never change behaviour. objdump assertions self-skip when
/// objdump is absent.
#[test]
#[ignore = "acceptance: builds an array-local program with FULL DWARF (clang) and asserts DW_TAG_array_type + correct exit code"]
fn dwarf_full_array_local_build_has_array_die_and_runs() {
    if !on_path("clang") {
        eprintln!("skipping: clang unavailable");
        return;
    }
    let (bin, run_code) = build_and_run("arraylocal", ARRAY_LOCAL_SRC, Some("full"));
    assert!(
        has_debug_info(&bin),
        "the full-debug array-local build must have .debug_info"
    );
    if on_path("objdump") {
        let info = dwarf_info(&bin);
        // the array local is a DW_TAG_array_type with a DW_TAG_subrange_type
        // (the bound) and an i32 base — objdump renders the subrange as an
        // upper_bound (count-1) or count, so accept either spelling
        assert!(
            info.contains("DW_TAG_array_type"),
            "the DWARF should describe the array local as a DW_TAG_array_type:\n{info}"
        );
        assert!(
            info.contains("DW_TAG_subrange_type"),
            "the array type should carry a DW_TAG_subrange_type bound:\n{info}"
        );
        // the bound covers 4 elements: objdump prints either DW_AT_upper_bound 3
        // (count-1) or DW_AT_count 4 depending on version
        assert!(
            info.contains("DW_AT_upper_bound") || info.contains("DW_AT_count"),
            "the subrange should carry an upper_bound/count for the 4 elements:\n{info}"
        );
        assert!(
            info.contains("DW_TAG_variable"),
            "the array local should be a DW_TAG_variable:\n{info}"
        );
    } else {
        eprintln!("objdump absent: skipping the DIE-tree assertions");
    }
    std::fs::remove_file(&bin).ok();
    assert_eq!(
        run_code,
        Some(42),
        "the full-debug array-local binary must run with the correct exit code (42)"
    );
}
