#!/usr/bin/env bash
# Rakumux installer: curl -fsSL https://rakumux.com/install.sh | bash
#
# Downloads the latest release for this machine from GitHub, checks the
# release's signature (checksums.txt.minisig, by a Rakumux release key below)
# and the package against its checksums.txt, and runs the package's own
# install.sh, which puts the commands in $PREFIX/bin (default /usr/local,
# asking for sudo when it needs to). Another prefix:
#   curl -fsSL https://rakumux.com/install.sh | PREFIX="$HOME/.local" bash
# A specific version: RAKUMUX_VERSION=0.1.0.
#
# The signature is checked with minisign, else OpenSSL 3; with neither (as on
# a Mac without `brew install minisign`: its LibreSSL has no Ed25519), the
# installer says so and goes by HTTPS and checksums.txt alone. A release with
# no signature is refused, except one made before releases were signed and
# asked for with RAKUMUX_VERSION.
set -euo pipefail

REPO="humanunsupervised/rakumux"
# Release signing keys, from install/release-keys.pub (release-keys.sh sync).
RELEASE_KEYS="RWSbAhedaBF6C8DldJ7V1E44o4xiwGcF5k8uNDRmH0+k4drHtANpaq2E"
# The last release made before releases were signed.
LAST_UNSIGNED="0.1.40"

fail() {
    echo "rakumux install: $*" >&2
    exit 1
}

# BEGIN release signature check
# The same in landing/public/install.sh and install/scripts/release-keys.sh
# (tests/test-release-signatures.sh compares them). Bash 3.2.

# What can check a minisign signature here: minisign, else openssl when it
# has Ed25519 and BLAKE2b-512 (OpenSSL 3; macOS's LibreSSL has neither),
# else none.
rakumux_signature_tool() {
    if command -v minisign >/dev/null 2>&1; then
        echo minisign
        return 0
    fi
    if command -v openssl >/dev/null 2>&1 && rakumux_openssl_works; then
        echo openssl
        return 0
    fi
    echo none
}

# openssl checks RFC 8032's second Ed25519 test vector (the message "r"; it
# can't verify an empty one) and hashes with BLAKE2b-512.
rakumux_openssl_works() {
    local probe status=0
    probe="$(mktemp -d "${TMPDIR:-/tmp}/rakumux-openssl.XXXXXX")" || return 1
    printf '%s' 'MCowBQYDK2VwAyEAPUAXw+hDiVqStwqnTRt+vJyYLM8uxJaMwM1V8Sr0Zgw=' |
        openssl base64 -d -A >"$probe/key.der" 2>/dev/null || status=1
    printf '%s' 'kqAJqfDUyrhyDoILX2QlQKKye1QWUD+Ps3YiI+vbadoIWsHkPhWZbkWPNhPQ8R2MOHsurrQwKu6wDSkWErsMAA==' |
        openssl base64 -d -A >"$probe/signature" 2>/dev/null || status=1
    printf r >"$probe/message"
    if [ "$status" = 0 ]; then
        openssl pkeyutl -verify -pubin -keyform DER -inkey "$probe/key.der" -rawin \
            -in "$probe/message" -sigfile "$probe/signature" >/dev/null 2>&1 || status=1
    fi
    if [ "$status" = 0 ]; then
        openssl dgst -blake2b512 -binary "$probe/message" >/dev/null 2>&1 || status=1
    fi
    rm -rf "$probe"
    return "$status"
}

# rakumux_openssl_verify DIR KEY FILE SIGNATURE: prints the trusted comment
# of SIGNATURE (minisign, prehashed) once it and its comment are KEY's (a
# base64 minisign public key) signatures of FILE. DIR is scratch space.
rakumux_openssl_verify() {
    local dir="$1" key="$2" file="$3" signature="$4" line comment
    printf '%s' "$key" | openssl base64 -d -A >"$dir/key" 2>/dev/null || return 1
    [ "$(wc -c <"$dir/key" | tr -d ' ')" = 42 ] || return 1
    [ "$(head -c 2 "$dir/key")" = Ed ] || return 1
    sed -n 2p "$signature" | tr -d '\r\n' | openssl base64 -d -A >"$dir/signature" 2>/dev/null || return 1
    [ "$(wc -c <"$dir/signature" | tr -d ' ')" = 74 ] || return 1
    # Prehashed (ED): the signature is of the file's BLAKE2b-512 hash.
    [ "$(head -c 2 "$dir/signature")" = ED ] || return 1
    [ "$(head -c 10 "$dir/key" | tail -c 8 | od -An -tx1 | tr -d ' \n')" = \
        "$(head -c 10 "$dir/signature" | tail -c 8 | od -An -tx1 | tr -d ' \n')" ] || return 1
    printf '%s' 'MCowBQYDK2VwAyEA' | openssl base64 -d -A >"$dir/key.der" 2>/dev/null || return 1
    tail -c 32 "$dir/key" >>"$dir/key.der"
    tail -c 64 "$dir/signature" >"$dir/signature.ed25519"
    openssl dgst -blake2b512 -binary "$file" >"$dir/hash" 2>/dev/null || return 1
    openssl pkeyutl -verify -pubin -keyform DER -inkey "$dir/key.der" -rawin \
        -in "$dir/hash" -sigfile "$dir/signature.ed25519" >/dev/null 2>&1 || return 1
    line="$(sed -n 3p "$signature" | tr -d '\r')"
    case "$line" in "trusted comment: "*) ;; *) return 1 ;; esac
    comment="${line#trusted comment: }"
    # The comment's own signature covers the file's signature and the comment.
    { cat "$dir/signature.ed25519"; printf '%s' "$comment"; } >"$dir/global"
    sed -n 4p "$signature" | tr -d '\r\n' | openssl base64 -d -A >"$dir/global.signature" 2>/dev/null || return 1
    [ "$(wc -c <"$dir/global.signature" | tr -d ' ')" = 64 ] || return 1
    openssl pkeyutl -verify -pubin -keyform DER -inkey "$dir/key.der" -rawin \
        -in "$dir/global" -sigfile "$dir/global.signature" >/dev/null 2>&1 || return 1
    printf '%s\n' "$comment"
}

# rakumux_comment_names COMMENT TAG: the trusted comment is release TAG's.
rakumux_comment_names() {
    local expected="rakumux-release tag:$2 file:checksums.txt"
    case "$1" in
        "$expected" | "$expected "*) return 0 ;;
        *) return 1 ;;
    esac
}

# rakumux_check_signature FILE SIGNATURE TAG KEY...: FILE is signed by one
# of the KEYs, with a trusted comment naming release TAG. Status 0: it is;
# 1: it isn't; 2: nothing here can check it.
rakumux_check_signature() {
    local file="$1" signature="$2" tag="$3" tool key dir comment status=1
    shift 3
    tool="$(rakumux_signature_tool)"
    if [ "$tool" = none ]; then return 2; fi
    dir="$(mktemp -d "${TMPDIR:-/tmp}/rakumux-signature.XXXXXX")" || return 1
    for key in "$@"; do
        if [ "$tool" = minisign ]; then
            comment="$(minisign -V -Q -P "$key" -m "$file" -x "$signature" 2>/dev/null)" || continue
        else
            comment="$(rakumux_openssl_verify "$dir" "$key" "$file" "$signature")" || continue
        fi
        if rakumux_comment_names "$comment" "$tag"; then
            status=0
            break
        fi
    done
    rm -rf "$dir"
    return "$status"
}
# END release signature check

# version_at_most A B: A's major.minor.patch is B's or older.
version_at_most() {
    local a1 a2 a3 b1 b2 b3
    IFS=. read -r a1 a2 a3 <<<"${1%%-*}"
    IFS=. read -r b1 b2 b3 <<<"${2%%-*}"
    [ "$a1" -lt "$b1" ] && return 0
    [ "$a1" -gt "$b1" ] && return 1
    [ "$a2" -lt "$b2" ] && return 0
    [ "$a2" -gt "$b2" ] && return 1
    [ "$a3" -le "$b3" ]
}

# The release's checksums.txt is signed for $tag by a release key, or why not.
check_signature() {
    local status=0
    if [[ -z "$RELEASE_KEYS" ]]; then
        echo "The release signature wasn't checked: this installer has no release keys."
        return 0
    fi
    if ! curl -fsSL "$base/checksums.txt.minisig" -o "$tmp/checksums.txt.minisig" 2>/dev/null; then
        # Only when asked for by version: GitHub alone can't make an old
        # unsigned release the latest one and have it installed.
        if [[ -n "${RAKUMUX_VERSION:-}" ]] && version_at_most "${tag#v}" "$LAST_UNSIGNED"; then
            echo "$tag was released before releases were signed, so it has no signature to check."
            return 0
        fi
        fail "$tag has no signature (checksums.txt.minisig); not installing it"
    fi
    # shellcheck disable=SC2086 # one key per word
    rakumux_check_signature "$tmp/checksums.txt" "$tmp/checksums.txt.minisig" "$tag" $RELEASE_KEYS || status=$?
    case "$status" in
        0) echo "Signature verified: $tag is signed by a Rakumux release key." ;;
        2) echo "The release signature wasn't checked: install minisign (brew install minisign) to check it." ;;
        *) fail "$tag's signature isn't a valid Rakumux release signature; not installing it" ;;
    esac
}

# Everything runs from main, so a download cut short runs nothing.
main() {
    case "$(uname -s)-$(uname -m)" in
        Darwin-arm64) platform="macos-arm64" ;;
        Darwin-x86_64) platform="macos-intel" ;;
        Linux-x86_64 | Linux-amd64) platform="linux-x64" ;;
        *) fail "no prebuilt Rakumux for $(uname -s) $(uname -m) (macOS and Linux x64 only)" ;;
    esac

    command -v curl >/dev/null 2>&1 || fail "curl is needed"
    command -v tar >/dev/null 2>&1 || fail "tar is needed"

    if [[ -n "${RAKUMUX_VERSION:-}" ]]; then
        tag="v${RAKUMUX_VERSION#v}"
    else
        # The latest release's tag, so every file comes from that one release.
        tag="$(curl -fsSLI -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")" ||
            fail "couldn't find the latest release at https://github.com/$REPO/releases"
        tag="${tag##*/}"
    fi
    [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]] || fail "not a release version: $tag"
    base="https://github.com/$REPO/releases/download/$tag"
    package="rakumux-$platform"

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT

    echo "Downloading ${package} ${tag}..."
    curl -fsSL "$base/$package.tar.gz" -o "$tmp/$package.tar.gz" ||
        fail "couldn't download $package for $tag; check its available targets at https://github.com/$REPO/releases"
    curl -fsSL "$base/checksums.txt" -o "$tmp/checksums.txt" || fail "couldn't download checksums.txt"
    check_signature

    expected="$(awk -v f="$package.tar.gz" '$2 == f { print $1 }' "$tmp/checksums.txt")"
    [[ -n "$expected" ]] || fail "checksums.txt has no entry for $package.tar.gz"
    if command -v shasum >/dev/null 2>&1; then
        actual="$(shasum -a 256 "$tmp/$package.tar.gz" | cut -d' ' -f1)"
    else
        actual="$(sha256sum "$tmp/$package.tar.gz" | cut -d' ' -f1)"
    fi
    [[ "$actual" == "$expected" ]] || fail "checksum mismatch for $package.tar.gz"

    tar -xzf "$tmp/$package.tar.gz" -C "$tmp"
    (cd "$tmp/$package" && ./install.sh)
}

main "$@"
