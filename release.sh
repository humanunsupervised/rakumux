#!/bin/bash
# Release native binaries through GitHub Actions. No npm or Homebrew publishing.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="humanunsupervised/rakumux"
VERSION=""
INCREMENT="patch"
MODE="stable"
DRY_RUN=false

fail() { printf 'release: %s\n' "$*" >&2; exit 1; }

usage() {
    cat <<EOF
Usage: ./release.sh [VERSION] [--increment=major|minor|patch] [--draft|--prerelease] [--dry-run]

Creates and pushes a reviewed source tag. GitHub Actions builds and tests native
macOS Apple Silicon, macOS Intel and Linux x64 packages, verifies checksums, and
publishes the binary release used by https://rakumux.com/install.sh.

  VERSION          Version without v; defaults to the next published patch
  --increment=TYPE Auto-version increment (default: patch)
  --draft          Upload all assets but leave the GitHub release unpublished
  --prerelease     Publish a prerelease, without changing the latest stable release
  --dry-run        Validate and show the tag operation; push and publish nothing

Run from a clean public checkout whose HEAD has already been pushed to main.
Git SSH/HTTPS push access is required; local gh, npm and Homebrew logins are not.
EOF
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h|--help) usage; exit 0 ;;
        --increment=*) INCREMENT="${1#*=}" ;;
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
if [[ "$DRY_RUN" == "true" ]]; then
    printf 'Dry run: would create and push %s; no tag or release was created.\n' "$tag"
    exit 0
fi
git tag -a "$tag" -m "Rakumux $tag" -m "Release-Mode: $MODE"
git push origin "refs/tags/$tag"
printf 'Binary release started: https://github.com/%s/actions/workflows/release.yml\n' "$REPO"
printf 'The release becomes public only after every platform and package check succeeds.\n'
