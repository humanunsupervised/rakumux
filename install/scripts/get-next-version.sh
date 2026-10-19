#!/bin/bash

###############################################################################
# get-next-version.sh - Automatic Version Management
#
# Automatically determines the next version by checking the latest release
# on GitHub and incrementing based on the specified component.
###############################################################################

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info() { printf "${BLUE}[VERSION]${NC} %s\n" "$*" >&2; }
success() { printf "${GREEN}[VERSION]${NC} %s\n" "$*" >&2; }
warn() { printf "${YELLOW}[VERSION]${NC} %s\n" "$*" >&2; }
error() { printf "${RED}[VERSION]${NC} %s\n" "$*" >&2; }

# Default increment type
INCREMENT_TYPE="${1:-patch}"

# GitHub repository (from humanunsupervised org)
GITHUB_REPO="${GITHUB_REPO:-humanunsupervised/rakumux}"

# Function to validate increment type
validate_increment_type() {
    case "$INCREMENT_TYPE" in
        major|minor|patch)
            return 0
            ;;
        *)
            error "Invalid increment type: $INCREMENT_TYPE"
            error "Valid options: major, minor, patch"
            exit 1
            ;;
    esac
}

# Function to get latest version from GitHub releases
get_latest_version() {
    command -v curl >/dev/null 2>&1 || { error "curl is required"; return 1; }
    local body status latest_version
    body="$(mktemp)"
    if status="$(curl -sS -L --connect-timeout 10 --max-time 30 --output "$body" --write-out '%{http_code}' \
        "https://api.github.com/repos/$GITHUB_REPO/releases/latest")"; then
        case "$status" in
            404) rm -f "$body"; printf 'none\n'; return 0 ;;
            200) ;;
            *) rm -f "$body"; error "GitHub returned HTTP $status"; return 1 ;;
        esac
    else
        rm -f "$body"
        error "could not read GitHub releases"
        return 1
    fi
    latest_version="$(sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' "$body" | head -1)"
    rm -f "$body"
    latest_version="${latest_version#v}"
    [[ "$latest_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { error "invalid stable release version"; return 1; }
    printf '%s\n' "$latest_version"
}

# Function to increment version
increment_version() {
    local version="$1"
    local increment_type="$2"
    
    # Parse version into components
    if [[ ! "$version" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
        error "Invalid version format: $version"
        error "Expected format: X.Y.Z (e.g., 1.2.3)"
        exit 1
    fi
    
    local major="${BASH_REMATCH[1]}"
    local minor="${BASH_REMATCH[2]}"
    local patch="${BASH_REMATCH[3]}"
    
    # Increment based on type
    case "$increment_type" in
        major)
            major=$((major + 1))
            minor=0
            patch=0
            ;;
        minor)
            minor=$((minor + 1))
            patch=0
            ;;
        patch)
            patch=$((patch + 1))
            ;;
    esac
    
    echo "$major.$minor.$patch"
}

# Function to show usage
show_usage() {
    cat << EOF
Usage: $0 [INCREMENT_TYPE]

Automatically determines the next version by checking the latest GitHub release
and incrementing the specified component.

Arguments:
  INCREMENT_TYPE    What to increment: major, minor, patch (default: patch)

Examples:
  $0              # Increment patch: 0.1.4 → 0.1.5
  $0 patch        # Increment patch: 0.1.4 → 0.1.5  
  $0 minor        # Increment minor: 0.1.4 → 0.2.0
  $0 major        # Increment major: 0.1.4 → 1.0.0

Environment Variables:
  GITHUB_REPO     GitHub repository (default: $GITHUB_REPO)
  
Requirements:
  - curl
  - Public read access to releases from $GITHUB_REPO (no login needed)
EOF
}

# Handle help
if [[ "$INCREMENT_TYPE" == "-h" || "$INCREMENT_TYPE" == "--help" ]]; then
    show_usage
    exit 0
fi

# Main execution
validate_increment_type

latest_version=$(get_latest_version)
if [[ "$latest_version" == "none" ]]; then
    printf '0.1.0\n'
    exit 0
fi
next_version=$(increment_version "$latest_version" "$INCREMENT_TYPE")

success "Version increment: $latest_version → $next_version (${INCREMENT_TYPE})"

# Just output the version for use in other scripts
echo "$next_version"
