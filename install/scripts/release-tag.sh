#!/bin/bash
# Restore annotated tag metadata after Actions checks out the underlying commit.
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tag="${1:-}"
fail() {
    printf 'release tag: %s\n' "$*" >&2
    if [[ "${GITHUB_ACTIONS:-}" == true ]]; then printf '::error::Release metadata: %s\n' "$*" >&2; fi
    exit 1
}
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || fail 'invalid version tag'
# checkout can map the commit SHA directly onto refs/tags/<version>. Fetch
# the actual tag object back from origin; this changes only the runner's ref.
git fetch --no-tags --force origin "refs/tags/$tag:refs/tags/$tag" >&2 || fail 'could not fetch the authoritative release tag'
[[ "$(git cat-file -t "refs/tags/$tag")" == 'tag' ]] || fail 'release tag must be annotated'
[[ "$(git rev-parse HEAD)" == "$(git rev-parse "$tag^{commit}")" ]] || fail 'checkout does not match the release source'
annotation="$(git for-each-ref --format='%(contents)' "refs/tags/$tag")"
mode="$(printf '%s\n' "$annotation" | sed -n 's/^Release-Mode: //p' | head -1)"
targets="$(printf '%s\n' "$annotation" | sed -n 's/^Release-Targets: //p' | head -1)"
case "$mode" in stable|draft|prerelease) ;; *) fail 'missing release-mode annotation' ;; esac
matrix="$("$SCRIPT_DIR/release-targets.sh" "$targets")" || fail 'invalid or missing release targets'
printf 'mode=%s\ntargets=%s\nmatrix=%s\n' "$mode" "$targets" "$matrix"
