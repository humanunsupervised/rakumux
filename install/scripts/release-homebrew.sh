#!/bin/bash

###############################################################################
# release-homebrew.sh - Render, verify and publish the Homebrew formula
#
# 1. Render install/homebrew/rakumux.rb.template with the version and the
#    SHA-256 of the three release tarballs (from release/checksums.txt, checked
#    against the files) into install/homebrew/Formula/rakumux.rb. The repo copy
#    is the source of truth for the tap.
# 2. Verify (needs brew; the GitHub release must already be published, since the
#    formula downloads from it): copy the formula into a temporary local tap,
#    `brew audit --strict --online`, `brew install` from that tap, `brew test`,
#    then uninstall and remove the temporary tap.
# 3. Push: copy the formula into a clone of humanunsupervised/homebrew-rakumux,
#    commit and push.
#
# Usage: release-homebrew.sh <version> [--render-only] [--no-verify] [--no-push] [--output FILE]
#   --render-only   Only step 1 (same as --no-verify --no-push)
#   --no-verify     Skip step 2
#   --no-push       Skip step 3
#   --output FILE   Render to FILE instead of the repo copy (dry runs; implies --no-push)
# Environment:
#   HOMEBREW_TAP_REPO   Tap clone (default: <repo>/../homebrew-rakumux; cloned if missing)
###############################################################################

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info() { printf "${BLUE}[HOMEBREW]${NC} %s\n" "$*"; }
success() { printf "${GREEN}[HOMEBREW]${NC} %s\n" "$*"; }
warn() { printf "${YELLOW}[HOMEBREW]${NC} %s\n" "$*"; }
error() { printf "${RED}[HOMEBREW]${NC} %s\n" "$*" >&2; }

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
TEMPLATE="$PROJECT_ROOT/install/homebrew/rakumux.rb.template"
REPO_FORMULA="$PROJECT_ROOT/install/homebrew/Formula/rakumux.rb"
FORMULA="$REPO_FORMULA"
RELEASE_DIR="$PROJECT_ROOT/release"
TAP_REMOTE="git@github.com:humanunsupervised/homebrew-rakumux.git"
DEFAULT_TAP_DIR="$(cd "$PROJECT_ROOT/.." && pwd)/homebrew-rakumux"
HOMEBREW_TAP_REPO="${HOMEBREW_TAP_REPO:-$DEFAULT_TAP_DIR}"
VERIFY_TAP="rakumux-release/verify"

show_usage() {
    sed -n '/^# Usage:/,/^# *HOMEBREW_TAP_REPO/p' "$0" | sed 's/^# \{0,1\}//'
}

VERSION=""
DO_VERIFY=true
DO_PUSH=true
while [[ $# -gt 0 ]]; do
    case "$1" in
        --render-only)
            DO_VERIFY=false
            DO_PUSH=false
            ;;
        --no-verify) DO_VERIFY=false ;;
        --no-push) DO_PUSH=false ;;
        --output)
            if [[ $# -lt 2 ]]; then
                error "--output requires a file path"
                exit 2
            fi
            FORMULA="$2"
            DO_PUSH=false
            shift
            ;;
        -h | --help)
            show_usage
            exit 0
            ;;
        -*)
            error "Unknown option: $1"
            show_usage >&2
            exit 2
            ;;
        *)
            if [[ -n "$VERSION" ]]; then
                error "Unexpected argument: $1"
                exit 2
            fi
            VERSION="$1"
            ;;
    esac
    shift
done

if [[ -z "$VERSION" ]]; then
    show_usage >&2
    exit 2
fi
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    error "Version must look like 1.2.3 (got: $VERSION)"
    exit 2
fi

sha256_of() {
    if command -v shasum >/dev/null 2>&1; then
        shasum -a 256 "$1" | cut -d' ' -f1
    else
        sha256sum "$1" | cut -d' ' -f1
    fi
}

# SHA-256 of release/<tarball> from checksums.txt, verified against the file.
release_sha() {
    local tarball="$1" listed actual
    if [[ ! -f "$RELEASE_DIR/$tarball" ]]; then
        error "Missing $RELEASE_DIR/$tarball (run build-release.sh $VERSION on macOS)"
        exit 1
    fi
    listed="$(awk -v name="$tarball" '$2 == name { print $1 }' "$RELEASE_DIR/checksums.txt")"
    actual="$(sha256_of "$RELEASE_DIR/$tarball")"
    if [[ ! "$listed" =~ ^[0-9a-f]{64}$ ]]; then
        error "No SHA-256 for $tarball in $RELEASE_DIR/checksums.txt"
        exit 1
    fi
    if [[ "$listed" != "$actual" ]]; then
        error "$tarball does not match checksums.txt ($listed vs $actual)"
        exit 1
    fi
    printf '%s\n' "$actual"
}

render_formula() {
    if [[ ! -f "$RELEASE_DIR/checksums.txt" ]]; then
        error "Missing $RELEASE_DIR/checksums.txt (run build-release.sh $VERSION)"
        exit 1
    fi
    if ! grep -q "^# Rakumux v$VERSION\$" "$RELEASE_DIR/RELEASE_NOTES.md" 2>/dev/null; then
        error "release/ was not built for v$VERSION (see release/RELEASE_NOTES.md)"
        exit 1
    fi

    local intel arm64 linux
    intel="$(release_sha rakumux-macos-intel.tar.gz)"
    arm64="$(release_sha rakumux-macos-arm64.tar.gz)"
    linux="$(release_sha rakumux-linux-x64.tar.gz)"

    local rendered
    rendered="$(mktemp)"
    sed -e "s/@VERSION@/$VERSION/g" \
        -e "s/@SHA256_MACOS_INTEL@/$intel/g" \
        -e "s/@SHA256_MACOS_ARM64@/$arm64/g" \
        -e "s/@SHA256_LINUX_X64@/$linux/g" \
        "$TEMPLATE" >"$rendered"

    if grep -n '@[A-Z0-9_]*@' "$rendered" >&2; then
        rm -f "$rendered"
        error "Unrendered placeholders left in the formula (above)"
        exit 1
    fi
    if command -v ruby >/dev/null 2>&1 && ! ruby -c "$rendered" >/dev/null; then
        rm -f "$rendered"
        error "Rendered formula has Ruby syntax errors"
        exit 1
    fi

    mv "$rendered" "$FORMULA"
    chmod 644 "$FORMULA"
    success "Rendered $FORMULA for v$VERSION"
    info "  macOS Intel: $intel"
    info "  macOS arm64: $arm64"
    info "  Linux x64:   $linux"
}

cleanup_verify_tap() {
    if brew tap 2>/dev/null | grep -qx "$VERIFY_TAP"; then
        brew uninstall --formula "$VERIFY_TAP/rakumux" >/dev/null 2>&1 || true
        brew untap "$VERIFY_TAP" >/dev/null 2>&1 || true
    fi
}

verify_formula() {
    if ! command -v brew >/dev/null 2>&1; then
        error "brew is required to verify the formula (or pass --no-verify)"
        exit 1
    fi
    if brew list --formula rakumux >/dev/null 2>&1; then
        error "rakumux is already installed; the verification installs it from a temporary tap."
        error "Run 'brew uninstall rakumux' first (running sessions keep running), or pass --no-verify."
        exit 1
    fi

    info "Verifying in temporary tap $VERIFY_TAP..."
    trap cleanup_verify_tap EXIT
    cleanup_verify_tap
    brew tap-new --no-git "$VERIFY_TAP" >/dev/null
    cp "$FORMULA" "$(brew --repository "$VERIFY_TAP")/Formula/rakumux.rb"

    brew audit --strict --online --formula "$VERIFY_TAP/rakumux"
    success "brew audit --strict --online passed"
    brew install --formula "$VERIFY_TAP/rakumux"
    brew test "$VERIFY_TAP/rakumux"
    success "brew install + brew test passed"

    cleanup_verify_tap
    trap - EXIT
}

push_formula() {
    if [[ ! -d "$HOMEBREW_TAP_REPO/.git" ]]; then
        info "Cloning $TAP_REMOTE into $HOMEBREW_TAP_REPO..."
        git clone "$TAP_REMOTE" "$HOMEBREW_TAP_REPO"
    fi

    info "Updating tap clone $HOMEBREW_TAP_REPO..."
    git -C "$HOMEBREW_TAP_REPO" checkout main
    git -C "$HOMEBREW_TAP_REPO" pull --ff-only origin main
    mkdir -p "$HOMEBREW_TAP_REPO/Formula"
    cp "$FORMULA" "$HOMEBREW_TAP_REPO/Formula/rakumux.rb"
    git -C "$HOMEBREW_TAP_REPO" add Formula/rakumux.rb

    if git -C "$HOMEBREW_TAP_REPO" diff --cached --quiet; then
        warn "Tap already has this formula; nothing to push"
        return 0
    fi
    git -C "$HOMEBREW_TAP_REPO" commit -m "rakumux $VERSION"
    git -C "$HOMEBREW_TAP_REPO" push origin main
    success "Pushed rakumux $VERSION to humanunsupervised/homebrew-rakumux"
}

render_formula
if [[ "$DO_VERIFY" == "true" ]]; then
    verify_formula
else
    warn "Skipping brew audit/install/test (--no-verify)"
fi
if [[ "$DO_PUSH" == "true" ]]; then
    push_formula
else
    warn "Skipping tap push"
fi

if [[ "$FORMULA" == "$REPO_FORMULA" ]]; then
    info "Commit the rendered formula in this repo: git add install/homebrew/Formula/rakumux.rb"
fi
