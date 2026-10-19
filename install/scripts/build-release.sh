#!/bin/bash

###############################################################################
# build-release.sh - Native binary release builder
#
# Builds the Rakumux binaries for every release platform and packages them
# with the launcher scripts, LICENSE and THIRD_PARTY_NOTICES:
#
#   release/rakumux-<platform>/          staged package (for inspection)
#     rakumux                            tui binary (the rakumux program)
#     rakumux-ptyd                       session daemon
#     rakumux-api                        external HTTP API (127.0.0.1)
#     rakumux-mcp                        MCP server for coding agents
#     rakumux.sh, rakumux-local.sh     launchers
#     shell/                               launcher modules + command hook
#     LICENSE, THIRD_PARTY_NOTICES, README.md, install.sh
#   release/rakumux-<platform>.tar.gz    the release asset
#   release/checksums.txt, release/RELEASE_NOTES.md, release/THIRD_PARTY_NOTICES
#
# Platforms built on macOS: macos-intel (x86_64-apple-darwin), macos-arm64
# (aarch64-apple-darwin) and linux-x64 (x86_64-unknown-linux-musl, cargo-zigbuild).
# On Linux x86_64: linux-x64 only (x86_64-unknown-linux-gnu).
###############################################################################

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
RELEASE_DIR="$PROJECT_ROOT/release"
LICENSE_FILE="$PROJECT_ROOT/LICENSE"

show_usage() {
    cat << EOF
Usage: $0 [VERSION] [INCREMENT_TYPE] [--no-clean] [--platform=PLATFORM]

Arguments:
  VERSION             Specific version to build (optional)
  INCREMENT_TYPE      Version increment: major, minor, patch (default: patch)
  --no-clean          Skip 'cargo clean' (faster local dry-runs)
  --platform=PLATFORM Build only macos-arm64, macos-intel or linux-x64 (CI)

Examples:
  $0                    # Auto-increment patch version
  $0 1.0.0              # Build specific version 1.0.0
  $0 -- minor           # Auto-increment minor version
  $0 1.0.0 --no-clean   # Rebuild 1.0.0 without cleaning the cargo target dir

If VERSION is not provided, determines the next version from GitHub releases.
Requires cargo-about (THIRD_PARTY_NOTICES); on macOS also cargo-zigbuild + zig
(Linux build) and the rustup targets x86_64-apple-darwin, aarch64-apple-darwin
and x86_64-unknown-linux-musl.
Linux can select the static target with RAKUMUX_LINUX_TARGET=x86_64-unknown-linux-musl.
EOF
}

VERSION=""
INCREMENT_TYPE="patch"
CLEAN_BUILD=true
ONLY_PLATFORM=""
positional_count=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        -h | --help)
            show_usage
            exit 0
            ;;
        --no-clean)
            CLEAN_BUILD=false
            shift
            ;;
        --platform=*)
            ONLY_PLATFORM="${1#*=}"
            shift
            ;;
        --)
            # Legacy form: `$0 -- minor` (no version, increment type only)
            shift
            if [[ $# -gt 0 ]]; then
                INCREMENT_TYPE="$1"
                shift
            fi
            ;;
        -*)
            echo "❌ Unknown option: $1" >&2
            show_usage >&2
            exit 2
            ;;
        *)
            if [[ $positional_count -eq 0 ]]; then
                VERSION="$1"
            else
                INCREMENT_TYPE="$1"
            fi
            positional_count=$((positional_count + 1))
            shift
            ;;
    esac
done

if [[ -z "$VERSION" ]]; then
    echo "🔍 Auto-determining next version..."
    if ! VERSION=$("$SCRIPT_DIR/get-next-version.sh" "$INCREMENT_TYPE"); then
        echo "❌ Failed to determine next version"
        exit 1
    fi
fi

cd "$PROJECT_ROOT"

# Runtime files every package ships, besides the binaries (paths relative to the repo).
RUNTIME_SCRIPTS=(
    "rakumux.sh"
    "shell/gwr_load.sh"
    "shell/gwr_colors.sh"
    "shell/gwr_logging.sh"
    "shell/gwr_binary.sh"
    "shell/gwr_terminal.sh"
    "shell/gwr_usage.sh"
    "shell/gwr/check-updates.sh"
    "shell/rakumux-command-hook.sh"
)

# Release platforms for this host: "platform:target-triple:cargo-subcommand".
release_platforms() {
    if [[ -n "$ONLY_PLATFORM" ]]; then
        case "$(uname -s):$ONLY_PLATFORM" in
            Darwin:macos-intel) echo "macos-intel:x86_64-apple-darwin:build" ;;
            Darwin:macos-arm64) echo "macos-arm64:aarch64-apple-darwin:build" ;;
            Linux:linux-x64)
                local target="${RAKUMUX_LINUX_TARGET:-x86_64-unknown-linux-gnu}"
                case "$target" in x86_64-unknown-linux-gnu|x86_64-unknown-linux-musl) ;; *) echo "Unsupported Linux target: $target" >&2; return 1 ;; esac
                echo "linux-x64:$target:build"
                ;;
            *) echo "Unsupported platform for this host: $ONLY_PLATFORM" >&2; return 1 ;;
        esac
        return
    fi
    case "$(uname -s)-$(uname -m)" in
        Darwin-*)
            echo "macos-intel:x86_64-apple-darwin:build"
            echo "macos-arm64:aarch64-apple-darwin:build"
            echo "linux-x64:x86_64-unknown-linux-musl:zigbuild"
            ;;
        Linux-x86_64)
            echo "linux-x64:x86_64-unknown-linux-gnu:build"
            ;;
        *)
            echo "❌ Unsupported release host: $(uname -s) $(uname -m)" >&2
            return 1
            ;;
    esac
}

sha256_of() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        sha256sum "$1" | cut -d' ' -f1
    fi
}

# Fail when a binary isn't built for the platform it is packaged as.
verify_binary_arch() {
    local binary="$1" platform="$2" description
    description="$(file -b "$binary")"
    local expected
    case "$platform" in
        macos-intel) expected="Mach-O 64-bit.*x86_64" ;;
        macos-arm64) expected="Mach-O 64-bit.*arm64" ;;
        linux-x64) expected="ELF 64-bit.*x86-64" ;;
        *)
            echo "❌ Unknown platform: $platform" >&2
            exit 1
            ;;
    esac
    if [[ ! "$description" =~ $expected ]]; then
        echo "❌ $binary is not a $platform binary: $description" >&2
        exit 1
    fi
}

build_platform() {
    local platform="$1" triple="$2" subcommand="$3"

    if [[ "$subcommand" == "zigbuild" ]] && ! command -v cargo-zigbuild >/dev/null 2>&1; then
        echo "❌ cargo-zigbuild is required to build $platform." >&2
        echo "   Install with: cargo install cargo-zigbuild && brew install zig" >&2
        exit 1
    fi

    echo "🔨 Building $platform ($triple)..."
    (
        cd tui
        cargo "$subcommand" --release --locked --target "$triple" \
            -p tui -p rakumux-ptyd -p rakumux-api -p rakumux-mcp
    )
}

create_package() {
    local platform="$1" bin_dir="$2"
    local package_name="rakumux-$platform"
    local package_dir="$RELEASE_DIR/$package_name"

    echo "  📦 Creating $package_name..."
    mkdir -p "$package_dir"

    local binary
    for binary in tui rakumux-ptyd rakumux-api rakumux-mcp; do
        if [[ ! -f "$bin_dir/$binary" ]]; then
            echo "  ❌ $binary not found for $platform ($bin_dir/$binary)" >&2
            exit 1
        fi
        verify_binary_arch "$bin_dir/$binary" "$platform"
    done
    cp "$bin_dir/tui" "$package_dir/rakumux"
    for binary in rakumux-ptyd rakumux-api rakumux-mcp; do
        cp "$bin_dir/$binary" "$package_dir/$binary"
    done

    local script
    for script in "${RUNTIME_SCRIPTS[@]}"; do
        if [[ ! -f "$script" ]]; then
            echo "  ❌ Runtime script missing: $script" >&2
            exit 1
        fi
        mkdir -p "$package_dir/$(dirname "$script")"
        cp "$script" "$package_dir/$script"
    done
    cp shell/rakumux-local.sh "$package_dir/rakumux-local.sh"

    cp "$LICENSE_FILE" "$package_dir/LICENSE"
    cp "$RELEASE_DIR/THIRD_PARTY_NOTICES" "$package_dir/THIRD_PARTY_NOTICES"

    write_install_script "$package_dir/install.sh"
    write_package_readme "$package_dir/README.md" "$platform"

    chmod 755 "$package_dir/rakumux" "$package_dir/rakumux-ptyd" "$package_dir/rakumux-api" \
        "$package_dir/rakumux-mcp" "$package_dir/install.sh"
    find "$package_dir" -type f -name '*.sh' -exec chmod 755 {} +
    chmod 644 "$package_dir/LICENSE" "$package_dir/THIRD_PARTY_NOTICES" "$package_dir/README.md"

    make_tarball "$package_name"

    local sha256
    sha256="$(sha256_of "$RELEASE_DIR/$package_name.tar.gz")"
    echo "    SHA256: $sha256"
    echo "$sha256  $package_name.tar.gz" >>"$RELEASE_DIR/checksums.txt"
    echo "  ✅ Package created: release/$package_name.tar.gz"
}

# Tarball without macOS metadata: no AppleDouble ._* files, no xattrs, no owner names.
make_tarball() {
    local package_name="$1"
    local tarball="$RELEASE_DIR/$package_name.tar.gz"

    find "$RELEASE_DIR/$package_name" \( -name '._*' -o -name '.DS_Store' \) -exec rm -f {} +

    local tar_flags=()
    if tar --version 2>/dev/null | grep -q bsdtar; then
        tar_flags=(--no-mac-metadata --no-xattrs --uid 0 --gid 0)
    else
        tar_flags=(--no-xattrs --owner=0 --group=0 --numeric-owner)
    fi

    COPYFILE_DISABLE=1 tar "${tar_flags[@]}" -czf "$tarball" -C "$RELEASE_DIR" "$package_name"

    if tar -tzf "$tarball" | grep -E '(^|/)(\._[^/]*|\.DS_Store)$' >/dev/null; then
        echo "  ❌ $tarball contains macOS metadata files" >&2
        exit 1
    fi
}

write_install_script() {
    cat >"$1" << 'EOF'
#!/bin/bash
# Rakumux installer for release tarballs.
# Installs the package into $PREFIX/libexec/rakumux and the rakumux,
# rakumux-local, rakumux-mcp and rakumux-api commands into $PREFIX/bin
# (PREFIX defaults to /usr/local).

set -euo pipefail

PACKAGE_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-/usr/local}"
INSTALL_DIR="$PREFIX/bin"
LIBEXEC_DIR="$PREFIX/libexec/rakumux"

echo "Installing Rakumux to $LIBEXEC_DIR (commands in $INSTALL_DIR)..."

# True when any path (or, if it doesn't exist yet, its nearest existing parent)
# isn't writable by the current user.
needs_sudo() {
    local path dir
    for path in "$@"; do
        dir="$path"
        while [[ ! -e "$dir" ]]; do
            dir="$(dirname "$dir")"
        done
        [[ -w "$dir" ]] || return 0
    done
    return 1
}

SUDO=""
if needs_sudo "$LIBEXEC_DIR" "$(dirname "$LIBEXEC_DIR")" "$INSTALL_DIR" \
    "$INSTALL_DIR/rakumux" "$INSTALL_DIR/rakumux-local" \
    "$INSTALL_DIR/rakumux-mcp" "$INSTALL_DIR/rakumux-api"; then
    echo "Need sudo permissions to install to $PREFIX"
    SUDO="sudo"
fi

# Replace any previous installation, including 0.5.x leftovers.
$SUDO rm -rf "$LIBEXEC_DIR"
$SUDO rm -f "$INSTALL_DIR/rakumux-cli" "$INSTALL_DIR/rakumux-bin"
$SUDO mkdir -p "$LIBEXEC_DIR" "$INSTALL_DIR"

cd "$PACKAGE_DIR"
$SUDO cp rakumux rakumux-ptyd rakumux-api rakumux-mcp rakumux.sh rakumux-local.sh \
    LICENSE THIRD_PARTY_NOTICES "$LIBEXEC_DIR/"
$SUDO cp -R shell "$LIBEXEC_DIR/"
$SUDO chmod 755 "$LIBEXEC_DIR/rakumux" "$LIBEXEC_DIR/rakumux-ptyd" "$LIBEXEC_DIR/rakumux-api" \
    "$LIBEXEC_DIR/rakumux-mcp" "$LIBEXEC_DIR/rakumux.sh" "$LIBEXEC_DIR/rakumux-local.sh"
$SUDO find "$LIBEXEC_DIR/shell" -type f -name '*.sh' -exec chmod 755 {} +

$SUDO tee "$INSTALL_DIR/rakumux" >/dev/null << WRAPPER
#!/bin/bash
export GW_TUI_BIN="$LIBEXEC_DIR/rakumux"
export GW_PTYD_BIN="$LIBEXEC_DIR/rakumux-ptyd"
exec "$LIBEXEC_DIR/rakumux.sh" "\$@"
WRAPPER

$SUDO tee "$INSTALL_DIR/rakumux-local" >/dev/null << WRAPPER
#!/bin/bash
exec "$LIBEXEC_DIR/rakumux-local.sh" "\$@"
WRAPPER

# The MCP server and the API run directly (no launcher): stdout must stay
# clean for MCP, and both find each other and the daemon through these.
$SUDO tee "$INSTALL_DIR/rakumux-mcp" >/dev/null << WRAPPER
#!/bin/bash
export RAKUMUX_API_BIN="$LIBEXEC_DIR/rakumux-api"
export RAKUMUX_PTYD_BIN="$LIBEXEC_DIR/rakumux-ptyd"
exec "$LIBEXEC_DIR/rakumux-mcp" "\$@"
WRAPPER

$SUDO tee "$INSTALL_DIR/rakumux-api" >/dev/null << WRAPPER
#!/bin/bash
export RAKUMUX_PTYD_BIN="$LIBEXEC_DIR/rakumux-ptyd"
exec "$LIBEXEC_DIR/rakumux-api" "\$@"
WRAPPER

$SUDO chmod 755 "$INSTALL_DIR/rakumux" "$INSTALL_DIR/rakumux-local" \
    "$INSTALL_DIR/rakumux-mcp" "$INSTALL_DIR/rakumux-api"

echo "✅ Rakumux installed successfully!"
echo ""
echo "Type 'rakumux' in any folder to start."
echo "Coding agents: claude mcp add --scope user rakumux -- $INSTALL_DIR/rakumux-mcp"
EOF
}

write_package_readme() {
    local readme="$1" platform="$2"
    cat >"$readme" << EOF
# Rakumux v$VERSION - $platform

Terminal sessions for coding agents, in one fast TUI.

## Install

\`\`\`bash
./install.sh                 # installs to /usr/local (PREFIX=/opt/rakumux ./install.sh to change)
\`\`\`

## Usage

\`\`\`bash
rakumux                    # Open the TUI for the current folder
rakumux new --name dev     # Create a session and attach to it
rakumux ls                 # List sessions
rakumux --help             # All commands
\`\`\`

## Contents

- \`rakumux\`: the Rakumux program (TUI and CLI)
- \`rakumux-ptyd\`: the session daemon, started on demand
- \`rakumux-api\`: the external HTTP API (127.0.0.1 only), started on demand by rakumux-mcp
- \`rakumux-mcp\`: the MCP server for coding agents (Claude Code, Codex, OpenCode)
- \`rakumux.sh\`, \`shell/\`: launcher
- \`rakumux-local.sh\`: launches a local source checkout (development)
- \`LICENSE\`: Apache License 2.0
- \`THIRD_PARTY_NOTICES\`: licenses of the open-source components included

Built from commit $(git rev-parse HEAD 2>/dev/null || echo unknown)
EOF
}

write_release_notes() {
    cat >"$RELEASE_DIR/RELEASE_NOTES.md" << EOF
# Rakumux v$VERSION

Terminal sessions for coding agents, in one fast TUI.

## Installation

\`\`\`bash
curl -fsSL https://rakumux.com/install.sh | bash
\`\`\`

### Release tarballs

\`\`\`bash
curl -L https://github.com/humanunsupervised/rakumux/releases/download/v$VERSION/rakumux-macos-arm64.tar.gz | tar xz
cd rakumux-macos-arm64 && ./install.sh
\`\`\`

- **macOS Apple Silicon**: \`rakumux-macos-arm64.tar.gz\`
- **macOS Intel**: \`rakumux-macos-intel.tar.gz\`
- **Linux x64**: \`rakumux-linux-x64.tar.gz\`

Each package contains \`LICENSE\` and \`THIRD_PARTY_NOTICES\`.

## Usage

\`\`\`bash
rakumux             # Open the TUI for the current folder
rakumux --help      # All commands
\`\`\`

Built: $(date)
Commit: $(git rev-parse HEAD 2>/dev/null || echo unknown)
EOF
}

###############################################################################
# Main
###############################################################################

echo "🏗️  Building Rakumux v$VERSION for distribution..."

if ! platform_list="$(release_platforms)"; then
    exit 1
fi
PLATFORMS=()
while IFS= read -r line; do
    PLATFORMS+=("$line")
done <<<"$platform_list"

if [[ ! -f "$LICENSE_FILE" ]]; then
    echo "❌ License file not found: $LICENSE_FILE" >&2
    exit 1
fi

rm -rf "$RELEASE_DIR"
mkdir -p "$RELEASE_DIR"
: >"$RELEASE_DIR/checksums.txt"

export RAKUMUX_VERSION="$VERSION"
export COPYFILE_DISABLE=1

if [[ "$CLEAN_BUILD" == "true" ]]; then
    echo "🧹 cargo clean..."
    (cd tui && cargo clean)
fi

for entry in "${PLATFORMS[@]}"; do
    IFS=: read -r platform triple subcommand <<<"$entry"
    build_platform "$platform" "$triple" "$subcommand"
done

echo "📜 Generating THIRD_PARTY_NOTICES..."
"$SCRIPT_DIR/generate-third-party-notices.sh" "$RELEASE_DIR/THIRD_PARTY_NOTICES"

echo "📋 Creating release packages..."
for entry in "${PLATFORMS[@]}"; do
    IFS=: read -r platform triple subcommand <<<"$entry"
    create_package "$platform" "tui/target/$triple/release"
done

write_release_notes

echo ""
echo "🎉 Release packages created in release/"
echo ""
echo "📋 Release Summary:"
ls -la "$RELEASE_DIR"/*.tar.gz
echo ""
echo "🔐 SHA256 Checksums:"
cat "$RELEASE_DIR/checksums.txt"
echo ""
echo "📝 Production release entry point: ./release.sh $VERSION"
echo "1. ./install/scripts/test-install.sh $VERSION        # with SKIP_BUILD=1"
echo "2. ./install/scripts/create-github-release.sh $VERSION"
