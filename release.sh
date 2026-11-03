#!/bin/bash
# Release through GitHub Actions: native binaries to a draft GitHub release,
# signed and published by the maintainer, then npm.
# No Homebrew publishing.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="humanunsupervised/rakumux"
VERSION=""
INCREMENT="patch"
MODE="stable"
DRY_RUN=false
TARGETS="macos-intel,macos-arm64,linux-x64"

fail() { printf 'release: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<EOF
Usage: ./release.sh [VERSION] [--targets=LIST] [--increment=major|minor|patch] [--draft|--prerelease] [--dry-run]

Creates and pushes a reviewed source tag. GitHub Actions builds and tests only
the selected native packages, verifies checksums and uploads them as a draft
release. The maintainer then signs its checksums.txt with the release key and
publishes it: the release used by
https://rakumux.com/install.sh and the in-app updater, which refuse unsigned
releases. Publishing sends those packages to npm as rakumux (when all three
targets are selected; a prerelease under the next dist-tag).

  VERSION          Version without v; defaults to the next published patch
  --targets=LIST   One target or comma-separated targets (alias: --target)
                   Defaults: macos-intel,macos-arm64,linux-x64 (native CI runners)
  --increment=TYPE Auto-version increment (default: patch)
  --draft          Signed, but the GitHub release is left unpublished
  --prerelease     Publish a prerelease, without changing the latest stable release
  --dry-run        Validate and show the tag operation; push and publish nothing

Run from a clean public checkout whose HEAD has already been pushed to main.
Git SSH/HTTPS push access is required; local gh, npm and Homebrew logins are not.

Examples:
  ./release.sh                              # all three platforms
  ./release.sh --targets=linux-x64          # Linux-only override
  ./release.sh --targets=macos-intel,macos-arm64 --draft
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        --increment=*) INCREMENT="${1#*=}" ;;
        --target=*|--targets=*) TARGETS="${1#*=}"; [[ -n "$TARGETS" ]] || fail 'select at least one target' ;;
        --dry-run) DRY_RUN=true ;;
        --draft|--prerelease)
            [[ "$MODE" == "stable" ]] || fail "choose only one of --draft and --prerelease"
            MODE="${1#--}"
            ;;
        -*) fail "unknown option: $1" ;;
        *) [[ -z "$VERSION" ]] || fail "only one version is allowed"; VERSION="${1#v}" ;;
    esac
    shift
done

case "$INCREMENT" in major|minor|patch) ;; *) fail "invalid increment: $INCREMENT" ;; esac
"$ROOT/install/scripts/release-targets.sh" "$TARGETS" >/dev/null
# Every release carries the keys its own updates must be signed by.
"$ROOT/install/scripts/release-keys.sh" check >/dev/null || fail "add the release signing keys to install/release-keys.pub first"
cd "$ROOT"
command -v git >/dev/null 2>&1 || fail "git is required"
[[ -f .github/workflows/release.yml ]] || fail "binary release workflow is missing"
[[ -z "$(git status --porcelain)" ]] || fail "commit the reviewed changes first; the checkout must be clean"

remote="$(git remote get-url origin)"
case "$remote" in
    "git@github.com:$REPO.git"|"https://github.com/$REPO.git"|"https://github.com/$REPO") ;;
    *) fail "origin must be the clean public $REPO repository; do not publish private history" ;;
esac

if [[ -z "$VERSION" ]]; then
    VERSION="$(GITHUB_REPO="$REPO" "$ROOT/install/scripts/get-next-version.sh" "$INCREMENT")"
fi
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || fail "invalid version: $VERSION"
if [[ "$VERSION" == *-* && "$MODE" == "stable" ]]; then MODE="prerelease"; fi
tag="v$VERSION"
head="$(git rev-parse HEAD)"
published="$(git ls-remote origin refs/heads/main | awk '{print $1}')"
[[ -n "$published" && "$head" == "$published" ]] || fail "push this reviewed commit to public main before releasing"
if git show-ref --verify --quiet "refs/tags/$tag"; then fail "local tag $tag already exists"; fi
[[ -z "$(git ls-remote origin "refs/tags/$tag")" ]] || fail "remote tag $tag already exists; tags are never overwritten"

printf 'Release: %s (%s)\nSource: %s\nRepository: %s\n' "$tag" "$MODE" "$head" "$REPO"
printf 'Targets: %s\n' "$TARGETS"
if [[ "$DRY_RUN" == "true" ]]; then
    printf 'Dry run: would create and push %s; no tag or release was created.\n' "$tag"
    exit 0
fi
git tag -a "$tag" -m "Rakumux $tag" -m "Release-Mode: $MODE" -m "Release-Targets: $TARGETS"
git push origin "refs/tags/$tag"
printf 'Binary release started: https://github.com/%s/actions/workflows/release.yml\n' "$REPO"
printf 'CI builds a draft once every selected target and package check succeeds.\n'
printf 'Then sign the draft of %s and publish it; npm follows.\n' "$tag"
