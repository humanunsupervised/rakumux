#!/bin/bash
###############################################################################
# release-keys.sh - Rakumux's release signing keys and signature checks
#
# Usage:
#   release-keys.sh list                    the keys in install/release-keys.pub
#   release-keys.sh check                   fails unless it lists at least one valid key
#   release-keys.sh verify FILE SIG TAG     FILE (checksums.txt) is signed by one
#                                           of the keys, for release TAG
#   release-keys.sh sync                    write the keys into landing/public/install.sh
#
# RAKUMUX_RELEASE_KEYS names another keys file (tests). A release's
# checksums.txt.minisig is a minisign signature of its checksums.txt whose
# trusted comment begins `rakumux-release tag:<tag> file:checksums.txt`
# (sign-release.sh makes it). See "Signed releases" in install/AGENTS.md.
###############################################################################
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
KEYS_FILE="${RAKUMUX_RELEASE_KEYS:-$REPO_ROOT/install/release-keys.pub}"
INSTALLER="$REPO_ROOT/landing/public/install.sh"

fail() {
    printf 'release keys: %s\n' "$*" >&2
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

# The keys KEYS_FILE lists, one per line.
list_keys() {
    [[ -f "$KEYS_FILE" ]] || fail "no keys file: $KEYS_FILE"
    sed -e 's/#.*//' "$KEYS_FILE" | awk 'NF { print $1 }'
}

check_keys() {
    local keys key seen="" count=0
    keys="$(list_keys)"
    for key in $keys; do
        [[ "$key" =~ ^RW[A-Za-z0-9+/]{54}$ ]] ||
            fail "$KEYS_FILE: not a minisign public key: $key"
        case " $seen " in *" $key "*) fail "$KEYS_FILE lists one key twice" ;; esac
        seen="$seen $key"
        count=$((count + 1))
    done
    [[ "$count" -gt 0 ]] ||
        fail "$KEYS_FILE lists no release keys yet; add your public keys first (install/AGENTS.md, \"Signed releases\")"
    printf '%s lists %s release key(s)\n' "$KEYS_FILE" "$count"
}

# Rewrite the RELEASE_KEYS line of the installer.
sync_installer() {
    local keys line
    keys="$(list_keys | tr '\n' ' ' | sed 's/ *$//')"
    line="RELEASE_KEYS=\"$keys\""
    grep -q '^RELEASE_KEYS="' "$INSTALLER" || fail "$INSTALLER has no RELEASE_KEYS line"
    awk -v line="$line" '/^RELEASE_KEYS="/ { print line; next } { print }' "$INSTALLER" >"$INSTALLER.new"
    chmod 0644 "$INSTALLER.new"
    mv "$INSTALLER.new" "$INSTALLER"
    printf 'Wrote %s into %s\n' "${keys:-no keys}" "$INSTALLER"
}

case "${1:-}" in
    list) list_keys ;;
    check) check_keys ;;
    verify)
        [[ $# -eq 4 ]] || fail "usage: release-keys.sh verify FILE SIGNATURE TAG"
        [[ -f "$2" ]] || fail "no such file: $2"
        [[ -f "$3" ]] || fail "no signature: $3"
        keys="$(list_keys)"
        [[ -n "$keys" ]] || fail "$KEYS_FILE lists no release keys"
        # shellcheck disable=SC2086 # one key per word
        if rakumux_check_signature "$2" "$3" "$4" $keys; then
            printf 'Signature verified: %s is signed for %s by a Rakumux release key\n' "$2" "$4"
        else
            status=$?
            if [[ "$status" == 2 ]]; then
                fail "install minisign (or OpenSSL 3) to check signatures"
            fi
            fail "$3 is not a valid signature of $2 for $4 by a key in $KEYS_FILE"
        fi
        ;;
    sync) sync_installer ;;
    -h | --help | "")
        sed -n '3,16p' "$0" | sed 's/^# \{0,1\}//'
        ;;
    *) fail "unknown command: $1" ;;
esac
