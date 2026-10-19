#!/bin/bash
# Exercise the public bootstrap through stdin using real release packages.
# Native macOS runners use Apple's Bash 3.2 and a UTF-8 locale.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INSTALLER_SCRIPT="${INSTALLER_SCRIPT:-$ROOT/landing/public/install.sh}"
VERSION="${RAKUMUX_VERSION:-0.1.3}"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/rakumux-public-installer.XXXXXX")"
trap 'rm -rf "$TEST_ROOT"' EXIT

export HOME="$TEST_ROOT/home"
export PREFIX="$TEST_ROOT/prefix"
export RAKUMUX_HOME="$TEST_ROOT/data"
export RAKUMUX_VERSION="$VERSION"
export TMPDIR="$TEST_ROOT"
unset RAKUMUX_SOCKET RAKUMUX_SESSION_ID RAKUMUX_SESSION_TOKEN GW_TUI_BIN GW_PTYD_BIN
mkdir -p "$HOME"

echo "Testing $(uname -s) $(uname -m), Bash $BASH_VERSION, locale ${LC_ALL:-${LANG:-unset}}"
# Like curl | bash: Bash reads the script from stdin, not from a script file.
cat "$INSTALLER_SCRIPT" | /bin/bash

for binary in rakumux rakumux-api rakumux-mcp; do
    actual="$("$PREFIX/bin/$binary" --version)"
    [[ "$actual" == "$binary $VERSION" ]] || {
        echo "Unexpected installed version: $actual" >&2
        exit 1
    }
done
actual="$("$PREFIX/libexec/rakumux/rakumux-ptyd" --version)"
[[ "$actual" == "rakumux-ptyd $VERSION (protocol 1)" ]]
for document in LICENSE THIRD_PARTY_NOTICES; do
    [[ -s "$PREFIX/libexec/rakumux/$document" ]]
done
[[ ! -e "$RAKUMUX_HOME/run/service.json" ]]

echo "Public installer passed: checksum-verified native package, all four versions and licenses; no daemon started."
