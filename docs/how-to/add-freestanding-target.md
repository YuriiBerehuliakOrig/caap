# How to add a freestanding target

A *freestanding target* is a bare-metal board/triple the native backend can
cross-compile to (no OS, no `caap-sys-runtime`). They live in ONE place:
[stdlib/backend/driver.caap](../../stdlib/backend/driver.caap)'s `targets` table.
A tool looks a board up BY NAME and passes its entry down to `compile_freestanding`
/ `compile_surface_freestanding`, so the flagless CLI never grows a `--target`.

## The targets table

```
(bind targets
  (assoc (map_of)
    "cortex-m3"
    (assoc (map_of)
      "triple"     "thumbv7m-none-eabi"   ; LLVM target triple (set_target!)
      "cpu"        "cortex-m3"            ; clang -mcpu
      "linker"     "lld"                  ; -fuse-ld value (optional)
      "datalayout" "e-m:e-p:32:32-…")))    ; pins pointer width + alignment
```

Each entry's fields:

| field        | required | purpose                                                       |
|--------------|----------|---------------------------------------------------------------|
| `triple`     | yes      | LLVM target triple; pins backend/emit/llvm via `set_target!` |
| `cpu`        | yes      | clang `-mcpu`                                                  |
| `datalayout` | yes      | LLVM datalayout — pointer width/alignment (drives `ptr` sizing)|
| `linker`     | no       | `-fuse-ld` value when the host linker can't emit the ISA      |

## The one rule: the host linker can't emit cross ELF

The host default (`ld.bfd`) rejects an ARM ELF (`unrecognised emulation mode:
armelf`), so a cross target sets `"linker": "lld"` and `link_bare!` passes it as
`-fuse-ld=lld`. The build then needs `ld.lld` on `PATH` (from the `lld` package).
Without it the build fails at the link step — and the acceptance test self-skips,
by design.

## Touch points

1. **The table** — add a `"<name>"` entry to `targets` in `driver.caap`. With a
   correct triple/cpu/datalayout/linker, **nothing else changes**: the tool layer
   (`tools/s2_build.caap`, the `compile_surface_freestanding` path) looks it up by
   name and forwards it.
2. **An acceptance test** — mirror `stdlib_urun_slice_phase_qemu` in
   [caap/tests/stdlib_codegen_tests.rs](../../caap/tests/stdlib_codegen_tests.rs):
   cross-build a small program for the target, assert the ELF's `e_machine`, and
   (if a simulator exists) run it and assert its output. Mark it
   `#[ignore = "acceptance: …"]` and self-skip when `clang` / the linker / the
   simulator are absent (an environment gap, not a regression).
3. **The doctor** — [scripts/native-doctor.sh](../../scripts/native-doctor.sh)
   already enumerates the `targets` table and reports whether the acceptance path
   runs / builds-only / self-skips on this host; no change needed unless the target
   needs a tool the doctor doesn't yet check.

## Verify (run these, in order)

```bash
# 0. what will this host actually do? (tools, targets, the acceptance verdict)
scripts/native-doctor.sh

# 1. driver still parses
python3 scripts/caap_refactor.py check stdlib/backend/driver.caap

# 2. cross-build a surface program for the new target (one process). With an ARM
#    board this is the URun slice; adapt DIR/entry/target for yours.
cargo build -p caap-cli
cargo run -p caap-cli -- stdlib/bootstrap.caap examples/urun/ur_build.caap \
    examples/urun /tmp/out.elf <your-target-name>
file /tmp/out.elf      # expect: ELF, the target's machine, statically linked

# 3. run it in the target's simulator and assert behavior (ARM example):
qemu-system-arm -M mps2-an385 -cpu cortex-m3 -nographic -kernel /tmp/out.elf
#   the URun slice prints exactly: MABCPDT
```

No `ld.lld` on the host? Install `lld` (`apt-get install -y lld` provides
`ld.lld`); the doctor's verdict line tells you whether the acceptance test will run
fully, build only, or self-skip.
