#!/bin/bash
###############################################################################
# sign-draft.sh - Sign a release's draft and publish it, unattended (the release
# workflow's `sign` job, after its environment's reviewer approved it)
#
# Usage: release-scripts/sign-draft.sh TAG [--repo OWNER/NAME] [--trusted-by FILE]
#
# The secret minisign key is the contents of RAKUMUX_SIGNING_KEY (a key made
# with `minisign -G -W`: no password; the secret store and the approval are its
# protection). It is written only to a private temporary folder, removed on
# exit.
#
# Checks, as signing by hand does: TAG's release is a draft without a
# signature; TAG is an annotated release tag with a Release-Mode; checksums.txt
# lists each package once, every package matches it, and nothing else is
# there. Then signs checksums.txt (trusted comment `rakumux-release tag:TAG
# file:checksums.txt timestamp:N`), checks the signature against
# install/release-keys.pub (RAKUMUX_RELEASE_KEYS overrides) and, with
# --trusted-by, against that keys file too (the newest published release's: so
# every copy that updated to it accepts this one), checks checksums.txt didn't
# change meanwhile, uploads checksums.txt.minisig and publishes the release as
# its mode says (stable: the latest; prerelease; draft: left a draft).
#
# In GitHub Actions it writes `mode=` and, once published, `published=true`
# to GITHUB_OUTPUT. Needs gh (GH_TOKEN with contents: write), minisign and the
# standard tools. See "Signed releases" in install/AGENTS.md.
###############################################################################
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
KEYS_SCRIPT="$SCRIPT_DIR/release-keys.sh"
REPO="${GITHUB_REPOSITORY:-humanunsupervised/rakumux}"
TAG=""
TRUSTED_BY=""
PLATFORMS="macos-intel macos-arm64 linux-x64"

fail() {
    printf 'sign-draft: %s\n' "$*" >&2
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --repo) [[ $# -ge 2 ]] || fail "--repo needs OWNER/NAME"; REPO="$2"; shift ;;
        --trusted-by) [[ $# -ge 2 ]] || fail "--trusted-by needs a keys file"; TRUSTED_BY="$2"; shift ;;
        -h | --help) sed -n '3,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        -*) fail "unknown option: $1" ;;
        *) [[ -z "$TAG" ]] || fail "only one release"; TAG="$1" ;;
    esac
    shift
done
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || fail "not a release tag: ${TAG:-none given}"
[[ -n "${RAKUMUX_SIGNING_KEY:-}" ]] || fail "RAKUMUX_SIGNING_KEY is empty: the release environment has no signing key"
[[ -z "$TRUSTED_BY" || -f "$TRUSTED_BY" ]] || fail "no keys file: $TRUSTED_BY"
command -v gh >/dev/null 2>&1 || fail "gh is needed"
command -v minisign >/dev/null 2>&1 || fail "minisign is needed"

output() {
    if [[ -n "${GITHUB_OUTPUT:-}" ]]; then printf '%s\n' "$@" >>"$GITHUB_OUTPUT"; fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

"$KEYS_SCRIPT" check >/dev/null
WORK="$(mktemp -d "${TMPDIR:-/tmp}/rakumux-sign-draft.XXXXXX")"
chmod 700 "$WORK"
trap 'rm -rf "$WORK"' EXIT

# The draft, unsigned, and what its tag says it becomes.
draft="$(gh release view "$TAG" --repo "$REPO" --json isDraft --jq .isDraft)" ||
    fail "$REPO has no release $TAG"
[[ "$draft" == true ]] || fail "$TAG is already published; a published release is never re-signed"
assets="$(gh release view "$TAG" --repo "$REPO" --json assets --jq '.assets[].name')"
if printf '%s\n' "$assets" | grep -qx 'checksums.txt.minisig'; then
    fail "$TAG already has checksums.txt.minisig"
fi
tag_object="$(gh api "repos/$REPO/git/ref/tags/$TAG" --jq '.object.type + " " + .object.sha')" ||
    fail "couldn't read tag $TAG"
[[ "$tag_object" == "tag "* ]] || fail "$TAG isn't an annotated release tag"
annotation="$(gh api "repos/$REPO/git/tags/${tag_object#tag }" --jq .message)"
mode="$(printf '%s\n' "$annotation" | sed -n 's/^Release-Mode: //p' | head -1)"
case "$mode" in stable | prerelease | draft) ;; *) fail "$TAG has no Release-Mode annotation" ;; esac
output "mode=$mode"

# Every package, checked against checksums.txt, and nothing else.
mkdir "$WORK/assets"
gh release download "$TAG" --repo "$REPO" --dir "$WORK/assets" >/dev/null ||
    fail "couldn't download the assets of $TAG"
checksums="$WORK/assets/checksums.txt"
[[ -f "$checksums" ]] || fail "$TAG has no checksums.txt"
listed=0
while read -r hash name extra; do
    [[ -z "$hash" && -z "$name" ]] && continue
    [[ -z "${extra:-}" && "$hash" =~ ^[0-9a-f]{64}$ ]] || fail "checksums.txt has a malformed line: $hash $name"
    platform="${name#rakumux-}"
    platform="${platform%.tar.gz}"
    case "$name" in rakumux-*.tar.gz) ;; *) platform="" ;; esac
    case " $PLATFORMS " in
        *" $platform "*) [[ -n "$platform" ]] || fail "checksums.txt lists an unexpected file: $name" ;;
        *) fail "checksums.txt lists an unexpected file: $name" ;;
    esac
    [[ "$(awk -v f="$name" '$2 == f' "$checksums" | wc -l | tr -d ' ')" == 1 ]] ||
        fail "checksums.txt lists $name more than once"
    [[ -f "$WORK/assets/$name" ]] || fail "$TAG has no $name, which checksums.txt lists"
    [[ "$(sha256_of "$WORK/assets/$name")" == "$hash" ]] || fail "$name doesn't match checksums.txt"
    listed=$((listed + 1))
done <"$checksums"
[[ "$listed" -gt 0 ]] || fail "checksums.txt lists no packages"
for path in "$WORK/assets"/*; do
    name="${path##*/}"
    [[ "$name" == checksums.txt ]] && continue
    [[ -n "$(awk -v f="$name" '$2 == f' "$checksums")" ]] ||
        fail "$TAG has $name, which checksums.txt doesn't list"
done
printf 'Signing %s (%s): checksums.txt, every package checked against it:\n' "$TAG" "$mode"
sed 's/^/  /' "$checksums"

# Sign, with the key only in a private folder.
mkdir -m 700 "$WORK/key"
(umask 077 && printf '%s\n' "$RAKUMUX_SIGNING_KEY" >"$WORK/key/release.key")
signature="$WORK/checksums.txt.minisig"
comment="rakumux-release tag:$TAG file:checksums.txt timestamp:$(date +%s)"
minisign -S -s "$WORK/key/release.key" -m "$checksums" -x "$signature" -t "$comment" \
    -c "Rakumux $TAG release signature" </dev/null >/dev/null ||
    fail "minisign didn't sign (a key with a password can't sign unattended)"
rm -rf "$WORK/key"
"$KEYS_SCRIPT" verify "$checksums" "$signature" "$TAG" >/dev/null ||
    fail "the signing key isn't one of install/release-keys.pub; nothing was uploaded"
if [[ -n "$TRUSTED_BY" ]]; then
    RAKUMUX_RELEASE_KEYS="$TRUSTED_BY" "$KEYS_SCRIPT" verify "$checksums" "$signature" "$TAG" >/dev/null ||
        fail "the newest published release doesn't trust the signing key yet; sign $TAG with a key it lists"
fi

# The draft's checksums.txt is still the one signed.
mkdir "$WORK/again"
gh release download "$TAG" --repo "$REPO" --dir "$WORK/again" --pattern checksums.txt >/dev/null ||
    fail "couldn't read checksums.txt of $TAG again"
cmp -s "$checksums" "$WORK/again/checksums.txt" || fail "checksums.txt of $TAG changed while signing; nothing was uploaded"
gh release upload "$TAG" "$signature" --repo "$REPO" >/dev/null || fail "couldn't upload the signature"
printf 'Signed: checksums.txt.minisig uploaded to %s\n' "$TAG"

case "$mode" in
    stable) gh release edit "$TAG" --repo "$REPO" --draft=false --latest >/dev/null ;;
    prerelease) gh release edit "$TAG" --repo "$REPO" --draft=false --prerelease --latest=false >/dev/null ;;
    draft)
        printf '%s stays a draft (its tag says so).\n' "$TAG"
        exit 0
        ;;
esac
output "published=true"
printf 'Published %s.\n' "$TAG"
