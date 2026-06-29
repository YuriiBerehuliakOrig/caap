#!/usr/bin/env bash
# native-doctor — report whether this host can run the freestanding/native
# acceptance path (audit 2026-06-19 maintainability finding #7).
#
# The URun slice (and any `compile_freestanding` cortex-m3 build) needs clang
# plus ld.lld; the host default `ld.bfd` cannot emit ARM ELF
# ("unrecognised emulation mode: armelf"), so the build forces `-fuse-ld=lld`.
# A missing ld.lld makes the acceptance test SELF-SKIP (PASS, not run) —
# this command tells you that up front instead of late.
#
#   scripts/native-doctor.sh
set -u

root="$(cd "$(dirname "$0")/.." && pwd)"
ok=0; warn=0

have() { command -v "$1" >/dev/null 2>&1; }
ver()  { "$1" --version 2>/dev/null | head -1; }

line() {  # name  present?  detail
  if [ "$2" = yes ]; then printf "  \033[32m✓\033[0m %-22s %s\n" "$1" "$3"
  else printf "  \033[31m✗\033[0m %-22s %s\n" "$1" "$3"; fi
}

echo "== required toolchain =="
if have clang; then line clang yes "$(ver clang)"; ok=$((ok+1)); else line clang no "missing — install LLVM/clang"; fi

if have ld.lld; then line "ld.lld" yes "$(ver ld.lld)"; ok=$((ok+1))
else line "ld.lld" no "missing — install lld; cross link will FAIL"; fi

if have qemu-system-arm; then line "qemu-system-arm" yes "$(ver qemu-system-arm)"; ok=$((ok+1))
else line "qemu-system-arm" no "missing — UART/QEMU step is skipped"; warn=$((warn+1)); fi

have timeout && line timeout yes "(bounds the QEMU run)" || { line timeout no "missing"; warn=$((warn+1)); }

echo
echo "== freestanding targets (stdlib/backend/driver.caap 'targets') =="
grep -oE '^\s{4}"[a-z0-9-]+"' "$root/stdlib/backend/driver.caap" | tr -d ' "' | sed 's/^/  - /'

echo
echo "== verdict: URun acceptance (stdlib_urun_slice_phase_qemu) =="
if have clang && have ld.lld; then
  if have qemu-system-arm && have timeout; then
    echo "  RUNS FULLY — cross-build + QEMU UART assertion (expect MABCPDT)."
  else
    echo "  BUILDS the ARM ELF but SKIPS the QEMU UART check (no qemu/timeout)."
  fi
else
  echo "  SELF-SKIPS (PASS, not run): needs clang + ld.lld."
  echo "  Fix: install lld, e.g.  sudo apt-get install -y lld   (provides ld.lld)."
fi

echo
if [ "$ok" -ge 2 ] && have ld.lld; then exit 0; else exit 1; fi
