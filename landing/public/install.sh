#!/usr/bin/env bash
# Rakumux installer: curl -fsSL https://rakumux.com/install.sh | bash
#
# Downloads the latest release for this machine from GitHub, checks it
# against the release's checksums.txt, and runs the package's own
# install.sh, which puts the commands in $PREFIX/bin (default /usr/local,
# asking for sudo when it needs to). Another prefix:
#   curl -fsSL https://rakumux.com/install.sh | PREFIX="$HOME/.local" bash
# A specific version: RAKUMUX_VERSION=0.1.0.
set -euo pipefail

REPO="humanunsupervised/rakumux"

fail() {
    echo "rakumux install: $*" >&2
    exit 1
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
        base="https://github.com/$REPO/releases/download/v${RAKUMUX_VERSION#v}"
    else
        base="https://github.com/$REPO/releases/latest/download"
    fi
    package="rakumux-$platform"

    tmp="$(mktemp -d)"
    trap 'rm -rf "$tmp"' EXIT

    echo "Downloading $package…"
    curl -fsSL "$base/$package.tar.gz" -o "$tmp/$package.tar.gz" ||
        fail "couldn't download $package for this release; check its available targets at https://github.com/$REPO/releases"
    curl -fsSL "$base/checksums.txt" -o "$tmp/checksums.txt" || fail "couldn't download checksums.txt"

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
