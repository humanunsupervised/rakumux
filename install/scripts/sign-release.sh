#!/bin/bash
###############################################################################
# sign-release.sh - Sign a draft release and publish it (on the maintainer's Mac)
#
# Usage: install/scripts/sign-release.sh vX.Y.Z [--key FILE | --op REFERENCE]
#                                        [--repo OWNER/NAME] [--yes]
#
# The release workflow builds vX.Y.Z and leaves it a draft. This downloads
# the draft's checksums.txt and every package, checks each package against
# it, shows what it is about to sign, signs checksums.txt with minisign
# (trusted comment `rakumux-release tag:vX.Y.Z file:checksums.txt
# timestamp:N`, so the signature can't be moved onto another release),
# checks the signature against install/release-keys.pub, uploads
# checksums.txt.minisig and publishes the draft: as the latest release, or a
# prerelease, as the tag's Release-Mode says (a `draft` release is signed and
# left a draft). Publishing starts the workflow's npm job.
#
#   --key FILE       the minisign secret key (default ~/.minisign/rakumux-release.key)
#   --op REFERENCE   read it from 1Password instead (`op read`), e.g.
#                    op://Private/Rakumux release key/rakumux-release.key;
#                    kept in a private temporary folder and removed after signing
#   --repo NAME      default humanunsupervised/rakumux
#   --yes            don't ask before signing
#
# Needs gh (logged in with push access), minisign and standard tools; `op`
# only with --op. minisign asks for the key's password. See "Signed
# releases" in install/AGENTS.md.
###############################################################################
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="humanunsupervised/rakumux"
TAG=""
KEY="$HOME/.minisign/rakumux-release.key"
OP_REFERENCE=""
ASSUME_YES=false
PLATFORMS="macos-intel macos-arm64 linux-x64"

fail() {
    printf 'sign-release: %s\n' "$*" >&2
    exit 1
}

usage() { sed -n '3,27p' "$0" | sed 's/^# \{0,1\}//'; }

while [[ $# -gt 0 ]]; do
    case "$1" in
        -h | --help) usage; exit 0 ;;
        --key) [[ $# -ge 2 ]] || fail "--key needs a file"; KEY="$2"; shift ;;
        --key=*) KEY="${1#*=}" ;;
        --op) [[ $# -ge 2 ]] || fail "--op needs a reference"; OP_REFERENCE="$2"; shift ;;
        --op=*) OP_REFERENCE="${1#*=}" ;;
        --repo) [[ $# -ge 2 ]] || fail "--repo needs OWNER/NAME"; REPO="$2"; shift ;;
        --repo=*) REPO="${1#*=}" ;;
        --yes) ASSUME_YES=true ;;
        -*) fail "unknown option: $1" ;;
        *) [[ -z "$TAG" ]] || fail "only one release"; TAG="$1" ;;
    esac
    shift
done
[[ -n "$TAG" ]] || { usage >&2; exit 1; }
[[ "$TAG" == v* ]] || TAG="v$TAG"
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || fail "not a release tag: $TAG"
command -v gh >/dev/null 2>&1 || fail "gh is needed (brew install gh, then gh auth login)"
command -v minisign >/dev/null 2>&1 || fail "minisign is needed (brew install minisign)"
if [[ -n "$OP_REFERENCE" ]]; then
    command -v op >/dev/null 2>&1 || fail "--op needs the 1Password CLI (op)"
fi

sha256_of() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        sha256sum "$1" | cut -d' ' -f1
    fi
}

# A release is signed only by a key every installed Rakumux will accept.
"$SCRIPT_DIR/release-keys.sh" check >/dev/null

WORK="$(mktemp -d "${TMPDIR:-/tmp}/rakumux-sign.XXXXXX")"
chmod 700 "$WORK"
trap 'rm -rf "$WORK"' EXIT

# The draft and what its tag says it should become.
draft="$(gh release view "$TAG" --repo "$REPO" --json isDraft --jq .isDraft)" ||
    fail "$REPO has no release $TAG (the workflow makes the draft; is it done?)"
[[ "$draft" == true ]] || fail "$TAG is already published; a published release is never re-signed"
assets="$(gh release view "$TAG" --repo "$REPO" --json assets --jq '.assets[].name')"
if printf '%s\n' "$assets" | grep -qx 'checksums.txt.minisig'; then
    fail "$TAG already has checksums.txt.minisig; delete that asset first if it must be signed again"
fi
tag_object="$(gh api "repos/$REPO/git/ref/tags/$TAG" --jq '.object.type + " " + .object.sha')" ||
    fail "couldn't read tag $TAG"
[[ "$tag_object" == "tag "* ]] || fail "$TAG isn't an annotated release tag"
annotation="$(gh api "repos/$REPO/git/tags/${tag_object#tag }" --jq .message)"
commit="$(gh api "repos/$REPO/git/tags/${tag_object#tag }" --jq .object.sha)"
mode="$(printf '%s\n' "$annotation" | sed -n 's/^Release-Mode: //p' | head -1)"
case "$mode" in stable | prerelease | draft) ;; *) fail "$TAG has no Release-Mode annotation" ;; esac

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

printf 'Release %s of %s (%s), from commit %s\n\n' "$TAG" "$REPO" "$mode" "$commit"
printf 'checksums.txt, every package checked against it:\n'
sed 's/^/  /' "$checksums"
printf '\nsha256(checksums.txt) = %s\n\n' "$(sha256_of "$checksums")"
case "$mode" in
    stable) next="publish it as the latest release" ;;
    prerelease) next="publish it as a prerelease" ;;
    draft) next="leave it a draft (its tag says --draft)" ;;
esac
if [[ "$ASSUME_YES" != true ]]; then
    printf 'Sign checksums.txt of %s and %s? [y/N] ' "$TAG" "$next"
    read -r answer </dev/tty || answer=""
    case "$answer" in y | Y | yes) ;; *) fail "nothing was signed" ;; esac
fi

# The secret key: a file of the user's, or from 1Password for this signature only.
if [[ -n "$OP_REFERENCE" ]]; then
    mkdir -m 700 "$WORK/key"
    KEY="$WORK/key/release.key"
    (umask 077 && op read "$OP_REFERENCE" >"$KEY") ||
        fail "couldn't read $OP_REFERENCE from 1Password"
fi
[[ -f "$KEY" ]] || fail "no secret key at $KEY (--key FILE, or --op REFERENCE)"
signature="$WORK/checksums.txt.minisig"
comment="rakumux-release tag:$TAG file:checksums.txt timestamp:$(date +%s)"
minisign -S -s "$KEY" -m "$checksums" -x "$signature" -t "$comment" -c "Rakumux $TAG release signature" ||
    fail "minisign didn't sign"
rm -rf "$WORK/key"
"$SCRIPT_DIR/release-keys.sh" verify "$checksums" "$signature" "$TAG" >/dev/null ||
    fail "that key isn't one of install/release-keys.pub; nothing was uploaded"

# The draft's checksums.txt is still the one signed.
mkdir "$WORK/again"
gh release download "$TAG" --repo "$REPO" --dir "$WORK/again" --pattern checksums.txt >/dev/null ||
    fail "couldn't read checksums.txt of $TAG again"
cmp -s "$checksums" "$WORK/again/checksums.txt" || fail "checksums.txt of $TAG changed while signing; nothing was uploaded"
gh release upload "$TAG" "$signature" --repo "$REPO" >/dev/null || fail "couldn't upload the signature"
printf 'Signed: %s/checksums.txt.minisig uploaded to %s\n' "$TAG" "$TAG"

case "$mode" in
    stable) gh release edit "$TAG" --repo "$REPO" --draft=false --latest >/dev/null ;;
    prerelease) gh release edit "$TAG" --repo "$REPO" --draft=false --prerelease --latest=false >/dev/null ;;
    draft)
        printf '%s stays a draft. To publish it later: gh release edit %s --repo %s --draft=false --latest\n' "$TAG" "$TAG" "$REPO"
        exit 0
        ;;
esac
printf 'Published %s. npm gets it from the workflow: https://github.com/%s/actions/workflows/release.yml\n' "$TAG" "$REPO"
