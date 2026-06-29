#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

apply=false
cargo_clean=false

usage() {
  cat <<'USAGE'
Usage: scripts/clean-local-artifacts.sh [--dry-run] [--apply] [--cargo-clean]

Safely inspect or remove allowlisted local build artifacts.

Default behavior is a dry run. Nothing is removed unless --apply is passed.

Options:
  --dry-run      Show what would be removed. This is the default.
  --apply        Remove allowlisted small artifacts.
  --cargo-clean  Include Cargo's target/ cache. With --dry-run, only reports it.
  -h, --help     Show this help.
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --dry-run)
      apply=false
      ;;
    --apply)
      apply=true
      ;;
    --cargo-clean)
      cargo_clean=true
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'unknown option: %s\n\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
  shift
done

mode="dry-run"
if "$apply"; then
  mode="apply"
fi

size_of() {
  local path="$1"
  if [ -e "$path" ]; then
    du -sh "$path" 2>/dev/null | awk '{print $1}'
  else
    printf '0'
  fi
}

remove_path() {
  local path="$1"

  case "$path" in
    .caap_build|target/tmp|target/urun.elf)
      ;;
    *)
      printf 'refusing to clean non-allowlisted path: %s\n' "$path" >&2
      exit 3
      ;;
  esac

  if [ ! -e "$path" ]; then
    return 0
  fi

  if "$apply"; then
    printf 'removing %s (%s)\n' "$path" "$(size_of "$path")"
    rm -rf -- "$path"
  else
    printf 'would remove %s (%s)\n' "$path" "$(size_of "$path")"
  fi
}

printf 'Repository: %s\n' "$repo_root"
printf 'Mode: %s\n' "$mode"
printf 'target/: %s\n' "$(size_of target)"
printf '.caap_build/: %s\n' "$(size_of .caap_build)"

remove_path .caap_build
remove_path target/tmp
remove_path target/urun.elf

if "$cargo_clean"; then
  if "$apply"; then
    printf 'running cargo clean for target/ (%s)\n' "$(size_of target)"
    cargo clean
  else
    printf 'would run cargo clean for target/ (%s)\n' "$(size_of target)"
  fi
else
  printf 'keeping target/; pass --cargo-clean to include the Cargo build cache\n'
fi
