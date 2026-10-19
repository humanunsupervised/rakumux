#!/bin/bash
# Reproduce Actions flattening a local annotated tag. Uses local fixture
# repositories only: never accesses GitHub or changes any release tag there.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMP="$(mktemp -d)"
trap 'rm -rf "$TEMP"' EXIT
git init --bare --quiet "$TEMP/origin.git"
git init --quiet "$TEMP/source"
git -C "$TEMP/source" -c user.name=Test -c user.email=test@example.invalid commit --quiet --allow-empty -m 'Fixture source'
git -C "$TEMP/source" remote add origin "$TEMP/origin.git"
git -C "$TEMP/source" -c user.name=Test -c user.email=test@example.invalid tag -a v1.2.3 -m 'Fixture release' -m 'Release-Mode: stable' -m 'Release-Targets: linux-x64'
git -C "$TEMP/source" push --quiet origin refs/tags/v1.2.3
git -C "$TEMP/source" update-ref refs/tags/v1.2.3 HEAD
[[ "$(git -C "$TEMP/source" cat-file -t refs/tags/v1.2.3)" == commit ]]
(cd "$TEMP/source" && "$ROOT/install/scripts/release-tag.sh" v1.2.3) > "$TEMP/output"
[[ "$(git -C "$TEMP/source" cat-file -t refs/tags/v1.2.3)" == tag ]]
grep -qx 'mode=stable' "$TEMP/output"
grep -qx 'targets=linux-x64' "$TEMP/output"
grep -q '"platform":"linux-x64"' "$TEMP/output"
if (cd "$TEMP/source" && "$ROOT/install/scripts/release-tag.sh" invalid) > /dev/null 2>&1; then
    printf 'Invalid release tag unexpectedly accepted\n' >&2
    exit 1
fi
printf 'Annotated release-tag recovery regression test passed.\n'
