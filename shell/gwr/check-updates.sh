#!/bin/bash

###############################################################################
# check-updates.sh - Check for Orchestra updates from npm and brew
#
# Determines installation method and checks for newer versions
###############################################################################

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info() { printf "${BLUE}[UPDATE]${NC} %s\n" "$*"; }
success() { printf "${GREEN}[UPDATE]${NC} %s\n" "$*"; }
warn() { printf "${YELLOW}[UPDATE]${NC} %s\n" "$*"; }
error() { printf "${RED}[UPDATE]${NC} %s\n" "$*"; }

# Version of the installed binary, passed in by orchestra.sh (`orchestra --version`).
CURRENT_VERSION="${ORCHESTRA_CURRENT_VERSION:-unknown}"
NPM_PACKAGE="@humanu/orchestra"
BREW_FORMULA="orchestra"

# Function to get latest npm version
get_npm_latest_version() {
    if command -v npm >/dev/null 2>&1; then
        npm view "$NPM_PACKAGE" version 2>/dev/null || echo "unknown"
    else
        echo "npm_not_found"
    fi
}

# Function to get latest homebrew version
get_brew_latest_version() {
    if command -v brew >/dev/null 2>&1; then
        # First line: "==> [tap/]orchestra: stable 0.6.0 (bottled), HEAD"
        local latest
        latest="$(brew info "$BREW_FORMULA" 2>/dev/null | sed -n '1s/.*: stable \([^ ,]*\).*/\1/p')"
        echo "${latest:-unknown}"
    else
        echo "brew_not_found"
    fi
}

# Installed Homebrew version ("orchestra 0.5.81 0.6.0" -> newest listed).
get_brew_installed_version() {
    brew list --versions "$BREW_FORMULA" 2>/dev/null | awk '{ print $NF }'
}

# Function to detect installation method
detect_installation_method() {
    local method="unknown"
    
    # Check if installed via npm
    if command -v npm >/dev/null 2>&1; then
        if npm list -g "$NPM_PACKAGE" >/dev/null 2>&1; then
            method="npm"
        fi
    fi
    
    # Check if installed via homebrew (override npm if both)
    if command -v brew >/dev/null 2>&1; then
        if brew list "$BREW_FORMULA" >/dev/null 2>&1; then
            method="brew"
        fi
    fi
    
    # Check if running from local development
    # shell/gwr/ -> repository root
    if [[ -f "$(dirname "${BASH_SOURCE[0]}")/../../tui/Cargo.toml" ]]; then
        method="local"
    fi
    
    echo "$method"
}

# Compare dotted numeric versions: prints equal, older (current < latest) or newer.
# Prints "unknown" when either side isn't a dotted number (e.g. a lookup failed).
version_compare() {
    local current="$1" latest="$2"
    local version_re='^[0-9]+(\.[0-9]+)*$'

    if [[ ! "$current" =~ $version_re || ! "$latest" =~ $version_re ]]; then
        echo "unknown"
        return
    fi

    local IFS=.
    local -a cur lat
    read -r -a cur <<<"$current"
    read -r -a lat <<<"$latest"
    local i a b
    for ((i = 0; i < ${#cur[@]} || i < ${#lat[@]}; i++)); do
        a=$((10#${cur[i]:-0}))
        b=$((10#${lat[i]:-0}))
        if ((a < b)); then
            echo "older"
            return
        elif ((a > b)); then
            echo "newer"
            return
        fi
    done
    echo "equal"
}

# Main update check function
check_updates() {
    info "Checking for Orchestra updates..."
    echo
    
    # Detect installation method
    local install_method
    install_method=$(detect_installation_method)
    info "Detected installation method: $install_method"
    
    # Get current and latest versions based on installation method
    local current_version latest_version comparison
    
    case "$install_method" in
        "npm")
            current_version=$(npm list -g "$NPM_PACKAGE" 2>/dev/null | grep "$NPM_PACKAGE@" | sed 's/.*@//' | head -1)
            latest_version=$(get_npm_latest_version)
            ;;
        "brew")
            current_version="$(get_brew_installed_version)"
            if [[ -z "$current_version" ]]; then
                current_version="$CURRENT_VERSION"
            fi
            latest_version=$(get_brew_latest_version)
            ;;
        "local")
            info "Running from local development source"
            info "Current version: $CURRENT_VERSION (development)"
            info "To update, pull latest changes from git repository"
            return 0
            ;;
        *)
            warn "Could not detect installation method"
            current_version="$CURRENT_VERSION"
            latest_version=$(get_npm_latest_version)
            ;;
    esac
    
    echo
    info "Current version: $current_version"
    info "Latest version: $latest_version"
    echo
    
    # Compare versions
    comparison=$(version_compare "$current_version" "$latest_version")
    
    case "$comparison" in
        "equal")
            success "✅ You are running the latest version!"
            ;;
        "older")
            warn "⚠️  A newer version is available!"
            echo
            info "Update instructions:"
            case "$install_method" in
                "npm")
                    echo "  npm install -g $NPM_PACKAGE"
                    ;;
                "brew")
                    echo "  brew upgrade $BREW_FORMULA"
                    ;;
                *)
                    echo "  npm install -g $NPM_PACKAGE"
                    echo "  or"
                    echo "  brew install $BREW_FORMULA"
                    ;;
            esac
            echo
            info "Release notes: https://github.com/humanunsupervised/orchestra/releases"
            return 1
            ;;
        "newer")
            success "✅ You are running a newer version than latest release!"
            info "This might be a development or pre-release version."
            ;;
        *)
            error "Could not determine version comparison"
            return 1
            ;;
    esac
    
    return 0
}

# Run the update check
check_updates "$@"
