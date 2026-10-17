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
GITHUB_REPO="humanunsupervised/orchestra"

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
    if ! command -v gh >/dev/null 2>&1; then
        error "GitHub CLI (gh) is not installed"
        error "Please install with: brew install gh"
        exit 1
    fi
    
    # Check if gh is authenticated
    if ! gh auth status >/dev/null 2>&1; then
        error "GitHub CLI is not authenticated"
        error "Please run: gh auth login"
        exit 1
    fi
    
    info "Fetching latest release from $GITHUB_REPO..."
    
    # Get the latest release version
    local latest_version
    latest_version=$(gh release list --repo "$GITHUB_REPO" --limit 10 --json tagName --jq '.[0].tagName' 2>/dev/null || echo "")
    
    if [[ -z "$latest_version" ]]; then
        warn "No releases found on GitHub"
        echo "0.0.0"
        return
    fi
    
    # Remove 'v' prefix if present
    latest_version="${latest_version#v}"
    
    # Validate version format and find first valid one if current is invalid
    if [[ ! "$latest_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        warn "Latest release has invalid version format: $latest_version"
        info "Searching for most recent valid version..."
        
        # Get more releases and find first valid one
        local releases
        releases=$(gh release list --repo "$GITHUB_REPO" --limit 20 --json tagName --jq '.[].tagName' 2>/dev/null || echo "")
        
        while IFS= read -r version; do
            local clean_version="${version#v}"
            if [[ "$clean_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
                latest_version="$clean_version"
                info "Found valid version: $latest_version"
                break
            fi
        done <<< "$releases"
        
        # If no valid version found, start from 0.0.0
        if [[ ! "$latest_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
            warn "No valid versions found, starting from 0.0.0"
            echo "0.0.0"
            return
        fi
    fi
    
    info "Latest version found: $latest_version"
    echo "$latest_version"
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
  - GitHub CLI (gh) installed and authenticated
  - Access to read releases from $GITHUB_REPO
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
next_version=$(increment_version "$latest_version" "$INCREMENT_TYPE")

success "Version increment: $latest_version → $next_version (${INCREMENT_TYPE})"

# Just output the version for use in other scripts
echo "$next_version"