#!/bin/bash

###############################################################################
# test-install.sh - Test Rakumux release packages before deployment
#
# Builds (unless SKIP_BUILD=1) and checks the release packages: contents,
# binaries for this host, a real install.sh run into a temporary PREFIX,
# checksums, tarball hygiene and launcher script syntax. Nothing is installed
# outside the temporary directory, and no session daemon is started.
###############################################################################

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info() { printf "${BLUE}[TEST-INFO]${NC} %s\n" "$*"; }
success() { printf "${GREEN}[TEST-SUCCESS]${NC} %s\n" "$*"; }
warn() { printf "${YELLOW}[TEST-WARN]${NC} %s\n" "$*"; }
error() { printf "${RED}[TEST-ERROR]${NC} %s\n" "$*"; }

VERSION="${1:-}"
if [[ -z "$VERSION" ]]; then
    error "Usage: $0 <version>"
    error "Example: $0 0.6.0"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RELEASE_DIR="$REPO_ROOT/release"

info "🧪 Testing Rakumux v$VERSION release packages"
echo "=============================================="

cd "$REPO_ROOT"

# Step 1: Build
if [[ "${SKIP_BUILD:-0}" == "1" ]]; then
    info "📦 Step 1: Skipping build (packages pre-built)"
else
    info "📦 Step 1: Building release packages..."
    "$SCRIPT_DIR/build-release.sh" "$VERSION"
    success "Build completed"
fi

PACKAGES=()
for platform in macos-intel macos-arm64 linux-x64; do
    if [[ -d "$RELEASE_DIR/rakumux-$platform" ]]; then
        PACKAGES+=("$platform")
    fi
done
if [[ ${#PACKAGES[@]} -eq 0 ]]; then
    error "No release packages found in $RELEASE_DIR"
    exit 1
fi
info "Packages: ${PACKAGES[*]}"

run_with_timeout() {
    if command -v gtimeout >/dev/null 2>&1; then
        gtimeout 10s "$@"
    elif command -v timeout >/dev/null 2>&1; then
        timeout 10s "$@"
    else
        "$@"
    fi
}

# Step 2: Package contents
info "📋 Step 2: Verifying package contents..."
REQUIRED_FILES=(
    "rakumux"
    "rakumux-ptyd"
    "rakumux-api"
    "rakumux-mcp"
    "rakumux.sh"
    "rakumux-local.sh"
    "shell/gwr_load.sh"
    "shell/gwr_binary.sh"
    "shell/gwr/check-updates.sh"
    "shell/rakumux-command-hook.sh"
    "install.sh"
    "README.md"
    "LICENSE"
    "THIRD_PARTY_NOTICES"
)
# Removed in 0.6.0 (git/tmux era); must never ship again.
FORBIDDEN_PATHS=(
    "rakumux-cli.sh"
    "services.sh"
    "env-copy"
    "server"
    "api"
    "shell/git"
    "shell/tmux"
)

verify_package() {
    local package_dir="$RELEASE_DIR/rakumux-$1"
    local path
    for path in "${REQUIRED_FILES[@]}"; do
        if [[ ! -f "$package_dir/$path" ]]; then
            error "✗ rakumux-$1: $path missing"
            return 1
        fi
    done
    for path in "${FORBIDDEN_PATHS[@]}"; do
        if [[ -e "$package_dir/$path" ]]; then
            error "✗ rakumux-$1: $path must not ship"
            return 1
        fi
    done
    if ! cmp -s "$package_dir/LICENSE" "$REPO_ROOT/LICENSE"; then
        error "✗ rakumux-$1: LICENSE differs from the root LICENSE"
        return 1
    fi
    success "✓ rakumux-$1 contents"
}

for platform in "${PACKAGES[@]}"; do
    verify_package "$platform"
done

# Step 3: Binaries for this host
info "🔬 Step 3: Testing binaries for this host..."
case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) HOST_PLATFORM="macos-arm64" ;;
    Darwin-x86_64) HOST_PLATFORM="macos-intel" ;;
    Linux-x86_64) HOST_PLATFORM="linux-x64" ;;
    *) HOST_PLATFORM="" ;;
esac

if [[ -n "$HOST_PLATFORM" && -d "$RELEASE_DIR/rakumux-$HOST_PLATFORM" ]]; then
    package_dir="$RELEASE_DIR/rakumux-$HOST_PLATFORM"
    version_output="$(run_with_timeout "$package_dir/rakumux" --version)"
    if [[ "$version_output" != "rakumux $VERSION" ]]; then
        error "rakumux --version printed '$version_output', expected 'rakumux $VERSION'"
        exit 1
    fi
    success "✓ rakumux --version: $version_output"

    if ! ptyd_output="$(run_with_timeout "$package_dir/rakumux-ptyd" --version 2>&1)"; then
        error "rakumux-ptyd --version failed: $ptyd_output"
        exit 1
    fi
    success "✓ rakumux-ptyd --version: $ptyd_output"

    # --version never starts an API, a daemon or an MCP session.
    for binary in rakumux-api rakumux-mcp; do
        if ! binary_output="$(run_with_timeout "$package_dir/$binary" --version 2>&1)"; then
            error "$binary --version failed: $binary_output"
            exit 1
        fi
        if [[ "$binary_output" != "$binary $VERSION" ]]; then
            error "$binary --version printed '$binary_output', expected '$binary $VERSION'"
            exit 1
        fi
        success "✓ $binary --version: $binary_output"
    done

    if ! run_with_timeout "$package_dir/rakumux.sh" --help >/dev/null; then
        error "rakumux.sh --help failed"
        exit 1
    fi
    success "✓ rakumux.sh --help"
else
    warn "No package for this host ($(uname -s) $(uname -m)); skipping binary tests"
fi

# Step 4: install.sh into a temporary PREFIX
info "⚙️  Step 4: Testing install.sh..."
if [[ -n "$HOST_PLATFORM" && -d "$RELEASE_DIR/rakumux-$HOST_PLATFORM" ]]; then
    TEST_PREFIX="$(mktemp -d)"
    trap 'rm -rf "$TEST_PREFIX"' EXIT
    PREFIX="$TEST_PREFIX" "$RELEASE_DIR/rakumux-$HOST_PLATFORM/install.sh" >/dev/null
    installed_version="$(env -u GW_TUI_BIN -u GW_PTYD_BIN "$TEST_PREFIX/bin/rakumux" --version)"
    if [[ "$installed_version" != "rakumux $VERSION" ]]; then
        error "Installed rakumux --version printed '$installed_version'"
        exit 1
    fi
    for path in rakumux rakumux-ptyd rakumux-api rakumux-mcp rakumux.sh LICENSE THIRD_PARTY_NOTICES; do
        if [[ ! -e "$TEST_PREFIX/libexec/rakumux/$path" ]]; then
            error "install.sh did not install $path"
            exit 1
        fi
    done
    for binary in rakumux-api rakumux-mcp; do
        installed_version="$("$TEST_PREFIX/bin/$binary" --version)"
        if [[ "$installed_version" != "$binary $VERSION" ]]; then
            error "Installed $binary --version printed '$installed_version'"
            exit 1
        fi
    done
    success "✓ install.sh installs a working rakumux command"
else
    for platform in "${PACKAGES[@]}"; do
        bash -n "$RELEASE_DIR/rakumux-$platform/install.sh"
    done
    warn "No package for this host; checked install.sh syntax only"
fi

# Step 5: Checksums and tarball hygiene
info "🔐 Step 5: Verifying checksums and tarballs..."
if [[ ! -f "$RELEASE_DIR/checksums.txt" ]]; then
    error "checksums.txt not found at $RELEASE_DIR/checksums.txt"
    exit 1
fi
sha256_of() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        sha256sum "$1" | cut -d' ' -f1
    fi
}
for platform in "${PACKAGES[@]}"; do
    tarball="rakumux-$platform.tar.gz"
    listed="$(awk -v name="$tarball" '$2 == name { print $1 }' "$RELEASE_DIR/checksums.txt")"
    actual="$(sha256_of "$RELEASE_DIR/$tarball")"
    if [[ -z "$listed" || "$listed" != "$actual" ]]; then
        error "✗ $tarball checksum mismatch (listed: ${listed:-none}, actual: $actual)"
        exit 1
    fi
    if tar -tzf "$RELEASE_DIR/$tarball" | grep -E '(^|/)(\._[^/]*|\.DS_Store)$' >/dev/null; then
        error "✗ $tarball contains macOS metadata files"
        exit 1
    fi
    success "✓ $tarball checksum and contents"
done

# Step 6: Script syntax
info "🐚 Step 6: Testing shell script syntax..."
SCRIPTS=(rakumux.sh build.sh shell/rakumux-local.sh)
for script in shell/gwr_*.sh shell/gwr/*.sh shell/build/*.sh install/scripts/*.sh; do
    SCRIPTS+=("$script")
done
for script in "${SCRIPTS[@]}"; do
    if ! bash -n "$script"; then
        error "✗ $script syntax error"
        exit 1
    fi
done
success "✓ ${#SCRIPTS[@]} scripts pass bash -n"

echo
info "🎉 Test Summary for Rakumux v$VERSION"
echo "========================================="
success "✓ Package contents verified (${PACKAGES[*]})"
success "✓ Host binaries and install.sh tested"
success "✓ Checksums and tarballs verified"
success "✓ Shell script syntax validated"
echo
success "🚀 Release v$VERSION is ready for deployment!"
echo
info "To deploy this release:"
info "  1. ./install/scripts/create-github-release.sh $VERSION"
info "  2. ./install/scripts/release-homebrew.sh $VERSION"
info "  3. ./install/scripts/publish-npm.sh $VERSION"
