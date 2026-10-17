#!/bin/bash

###############################################################################
# stage-npm-package.sh - Stage install/npm for packing from release tarballs
#
# Fills install/npm from the release built by build-release.sh, so npm ships
# exactly what the tarballs ship:
#   resources/prebuilt/<platform>/{orchestra,orchestra-ptyd,orchestra-api,orchestra-mcp}  per tarball
#   resources/scripts/{orchestra.sh,orchestra-local.sh,shell/}  from one tarball
#   THIRD_PARTY_NOTICES                                         from release/
# and sets package.json "version". LICENSE is committed in install/npm.
#
# Usage: stage-npm-package.sh <version> [--allow-partial]
#   --allow-partial   Don't require all three platforms (local dry runs)
###############################################################################

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
NPM_DIR="$PROJECT_ROOT/install/npm"
RELEASE_DIR="$PROJECT_ROOT/release"
RESOURCES_DIR="$NPM_DIR/resources"
PLATFORMS=(macos-intel macos-arm64 linux-x64)

info() { printf '\033[0;34m[NPM-STAGE]\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m[NPM-STAGE]\033[0m %s\n' "$*"; }
error() { printf '\033[0;31m[NPM-STAGE]\033[0m %s\n' "$*" >&2; }

VERSION=""
ALLOW_PARTIAL=false
for arg in "$@"; do
    case "$arg" in
        --allow-partial) ALLOW_PARTIAL=true ;;
        -*)
            error "Unknown option: $arg"
            exit 2
            ;;
        *) VERSION="$arg" ;;
    esac
done
if [[ -z "$VERSION" ]]; then
    error "Usage: $0 <version> [--allow-partial]"
    exit 2
fi

if ! grep -q "^# Orchestra v$VERSION\$" "$RELEASE_DIR/RELEASE_NOTES.md" 2>/dev/null; then
    error "release/ was not built for v$VERSION; run ./install/scripts/build-release.sh $VERSION"
    exit 1
fi
if [[ ! -f "$RELEASE_DIR/THIRD_PARTY_NOTICES" ]]; then
    error "Missing $RELEASE_DIR/THIRD_PARTY_NOTICES"
    exit 1
fi
if [[ ! -f "$NPM_DIR/LICENSE" ]]; then
    error "Missing $NPM_DIR/LICENSE"
    exit 1
fi

info "Setting package.json version to $VERSION..."
node -e '
const fs = require("fs");
const file = process.argv[1];
const pkg = JSON.parse(fs.readFileSync(file, "utf8"));
pkg.version = process.argv[2];
fs.writeFileSync(file, JSON.stringify(pkg, null, 2) + "\n");
' "$NPM_DIR/package.json" "$VERSION"

rm -rf "$RESOURCES_DIR"
mkdir -p "$RESOURCES_DIR/prebuilt" "$RESOURCES_DIR/scripts"

work_dir="$(mktemp -d)"
trap 'rm -rf "$work_dir"' EXIT

staged=()
scripts_staged=false
for platform in "${PLATFORMS[@]}"; do
    tarball="$RELEASE_DIR/orchestra-$platform.tar.gz"
    if [[ ! -f "$tarball" ]]; then
        if [[ "$ALLOW_PARTIAL" == "true" ]]; then
            warn "Skipping $platform: $tarball not found"
            continue
        fi
        error "Missing $tarball (a full release needs all of: ${PLATFORMS[*]})"
        exit 1
    fi

    mkdir -p "$work_dir/$platform"
    tar -xzf "$tarball" -C "$work_dir/$platform"
    package_dir="$work_dir/$platform/orchestra-$platform"

    mkdir -p "$RESOURCES_DIR/prebuilt/$platform"
    for binary in orchestra orchestra-ptyd orchestra-api orchestra-mcp; do
        if [[ ! -f "$package_dir/$binary" ]]; then
            error "$tarball has no $binary"
            exit 1
        fi
        cp "$package_dir/$binary" "$RESOURCES_DIR/prebuilt/$platform/$binary"
        chmod 755 "$RESOURCES_DIR/prebuilt/$platform/$binary"
    done

    if [[ "$scripts_staged" == "false" ]]; then
        cp "$package_dir/orchestra.sh" "$package_dir/orchestra-local.sh" "$RESOURCES_DIR/scripts/"
        cp -R "$package_dir/shell" "$RESOURCES_DIR/scripts/"
        find "$RESOURCES_DIR/scripts" -type f -name '*.sh' -exec chmod 755 {} +
        scripts_staged=true
    fi
    staged+=("$platform")
done

if [[ "$scripts_staged" == "false" ]]; then
    error "No release tarballs found in $RELEASE_DIR"
    exit 1
fi

cp "$RELEASE_DIR/THIRD_PARTY_NOTICES" "$NPM_DIR/THIRD_PARTY_NOTICES"
chmod 644 "$NPM_DIR/THIRD_PARTY_NOTICES"

info "Staged @humanu/orchestra@$VERSION with binaries for: ${staged[*]}"
