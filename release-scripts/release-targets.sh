#!/bin/bash
# Validate a target selection and emit the corresponding native Actions matrix.
set -euo pipefail
targets="${1:-}"
fail() { printf 'release targets: %s\n' "$*" >&2; exit 1; }
[[ -n "$targets" ]] || fail 'select at least one target'
case "$targets" in ,*|*,|*,,*) fail 'empty target in selection' ;; esac
IFS=',' read -r -a selected <<< "$targets"
seen=","
matrix='{"include":['
separator=""
for target in "${selected[@]}"; do
    [[ "$seen" != *",$target,"* ]] || fail "duplicate target: $target"
    seen="$seen$target,"
    case "$target" in
        linux-x64) entry='{"os":"ubuntu-24.04","platform":"linux-x64","target":"x86_64-unknown-linux-musl"}' ;;
        macos-arm64) entry='{"os":"macos-15","platform":"macos-arm64","target":"aarch64-apple-darwin"}' ;;
        macos-intel) entry='{"os":"macos-15-intel","platform":"macos-intel","target":"x86_64-apple-darwin"}' ;;
        *) fail "unsupported target: $target (use linux-x64, macos-arm64 or macos-intel)" ;;
    esac
    matrix="$matrix$separator$entry"
    separator=","
done
printf '%s]}\n' "$matrix"
