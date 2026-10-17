#!/bin/bash

###############################################################################
# full-release-automation.sh - Complete Release Automation
#
# Handles the entire closed-source release workflow with automatic version
# management and pre-deployment testing.
###############################################################################

set -euo pipefail

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

info() { printf "${BLUE}[RELEASE]${NC} %s\n" "$*"; }
success() { printf "${GREEN}[RELEASE]${NC} %s\n" "$*"; }
warn() { printf "${YELLOW}[RELEASE]${NC} %s\n" "$*"; }
error() { printf "${RED}[RELEASE]${NC} %s\n" "$*"; }

# Parse command line arguments (the loop below reads every argument)
VERSION=""
INCREMENT_TYPE="patch"
SKIP_TEST="false"
DRY_RUN="false"
DRAFT="false"

# Function to show usage
show_usage() {
    cat << EOF
Usage: $0 [VERSION] [--increment=TYPE] [--skip-test] [--draft] [--prerelease] [--dry-run] [--repo=OWNER/NAME]
 
Arguments:
  VERSION             Specific version to release (optional)
  --increment=TYPE    Version increment: major, minor, patch (default: patch)
  --skip-test         Skip pre-deployment testing
  --draft             Create the GitHub release as a draft (no public visibility);
                      the Homebrew formula is rendered but not verified or pushed
  --prerelease        Mark the GitHub release as a prerelease
  --dry-run           Build and test, preview release notes, render the formula
                      into release/rakumux.rb and pack (not publish) npm
  --repo=OWNER/NAME   Override target repository (default: humanunsupervised/rakumux)
  --notes-last=N      Generate notes from only the last N commits (bootstrap helper)
 
Examples:
  $0                                   # Auto-increment patch version
  $0 --increment=minor                 # Auto-increment minor version
  $0 1.0.0                              # Release specific version 1.0.0
  $0 1.0.0 --skip-test                  # Release 1.0.0 without testing
  $0 --dry-run                          # Preview release notes and commands only
  $0 --draft --repo=me/sandbox          # Create a draft release in a sandbox repo
  $0 --notes-last=5                     # Use only the latest 5 commits in release notes
 
If VERSION is not provided, automatically determines next version from GitHub releases.
EOF
}

# Parse arguments
CREATE_FLAGS=()
while [[ $# -gt 0 ]]; do
    case $1 in
        --increment=*)
            INCREMENT_TYPE="${1#*=}"
            shift
            ;;
        --skip-test)
            SKIP_TEST="true"
            shift
            ;;
        --draft|--prerelease|--dry-run)
            [[ "$1" == "--dry-run" ]] && DRY_RUN="true"
            [[ "$1" == "--draft" ]] && DRAFT="true"
            CREATE_FLAGS+=("$1")
            shift
            ;;
        --notes-last=*)
            CREATE_FLAGS+=("$1")
            shift
            ;;
        --repo=*)
            CREATE_FLAGS+=("$1")
            shift
            ;;
        -h|--help)
            show_usage
            exit 0
            ;;
        *)
            if [[ -z "$VERSION" ]]; then
                VERSION="$1"
            else
                # Pass through any extra args to the create step
                CREATE_FLAGS+=("$1")
            fi
            shift
            ;;
    esac
done

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$SCRIPT_DIR/../.."
NPM_PUBLISH_STATUS="not-run"
NPM_PUBLISH_NOTE=""

load_npm_token_from_env_file() {
    local dotenv_file="$REPO_ROOT/.env"

    if [[ -z "${NPM_TOKEN:-}" && -f "$dotenv_file" ]]; then
        set -a
        # shellcheck disable=SC1090
        source "$dotenv_file"
        set +a
    fi
}

verify_npm_token_auth() {
    local userconfig_file
    userconfig_file="$(mktemp)"
    printf '%s\n' "//registry.npmjs.org/:_authToken=${NPM_TOKEN}" > "$userconfig_file"

    if NPM_CONFIG_USERCONFIG="$userconfig_file" npm whoami >/dev/null 2>&1; then
        rm -f "$userconfig_file"
        return 0
    fi

    rm -f "$userconfig_file"
    return 1
}

prompt_for_npm_login() {
    local answer

    if [[ ! -t 0 ]]; then
        error "Not logged in to npm. Run 'npm login' before running release automation."
        return 1
    fi

    warn "Not logged in to npm."
    printf "Run 'npm login' now? [y/N] "
    read -r answer
    case "$answer" in
        y|Y|yes|YES)
            npm login
            ;;
        *)
            error "Run 'npm login' before running release automation."
            return 1
            ;;
    esac
}

preflight_npm_auth() {
    info "🔐 Preflight: checking npm authentication..."

    if ! command -v npm >/dev/null 2>&1; then
        error "npm is required to publish the release package"
        exit 1
    fi

    load_npm_token_from_env_file

    if [[ -n "${NPM_TOKEN:-}" ]]; then
        if verify_npm_token_auth; then
            success "npm authentication verified with NPM_TOKEN"
            return 0
        fi

        error "NPM_TOKEN is set but npm authentication failed"
        error "Update or unset NPM_TOKEN, or run 'npm login' before release automation"
        exit 1
    fi

    if npm whoami >/dev/null 2>&1; then
        success "npm login session verified"
        return 0
    fi

    if ! prompt_for_npm_login; then
        exit 1
    fi

    if npm whoami >/dev/null 2>&1; then
        success "npm login session verified"
        return 0
    fi

    error "npm authentication still failed after login"
    exit 1
}

record_release_tracking_tag() {
    local version="$1"
    local tracking_tag="private-release/v$version"

    if git rev-parse -q --verify "refs/tags/$tracking_tag" >/dev/null 2>&1; then
        warn "Tracking tag already exists: $tracking_tag"
    elif git tag "$tracking_tag" >/dev/null 2>&1; then
        success "Recorded tracking tag: $tracking_tag"
    else
        warn "Could not create tracking tag $tracking_tag"
        warn "Future release notes may need --notes-last until a baseline tag exists"
    fi
}

# Dry runs never publish, so they don't need npm credentials
if [[ "$DRY_RUN" != "true" ]]; then
    preflight_npm_auth
fi

# Auto-determine version if not provided
if [[ -z "$VERSION" ]]; then
    info "Auto-determining next version..."
    if ! VERSION=$("$SCRIPT_DIR/get-next-version.sh" "$INCREMENT_TYPE"); then
        error "Failed to determine next version"
        exit 1
    fi
fi

info "🎵 Rakumux Full Release Automation v$VERSION"
echo "================================================"

# Step 0: Build release packages
info "🏗️ Step 0: Building release packages..."
if ! "$SCRIPT_DIR/build-release.sh" "$VERSION"; then
    error "Release build failed!"
    exit 1
fi
success "Release packages built"

# Step 1: Pre-deployment testing (unless skipped)
if [[ "$SKIP_TEST" != "true" ]]; then
    info "🧪 Step 1: Pre-deployment testing..."
    if ! SKIP_BUILD=1 "$SCRIPT_DIR/test-install.sh" "$VERSION"; then
        error "Pre-deployment testing failed!"
        error "Fix issues before deployment or use --skip-test to bypass"
        exit 1
    fi
    success "Pre-deployment testing passed"
else
    warn "⏭️  Skipping pre-deployment testing"
fi

# Step 2: Verify release packages exist
info "📦 Step 2: Verifying release packages..."
if [[ ! -d "release" ]] || [[ ! -f "release/RELEASE_NOTES.md" ]]; then
    error "Release packages not found after build step!"
    error "Check build logs or rerun: ./install/scripts/build-release.sh $VERSION"
    exit 1
fi
success "Release packages verified"

# Step 3: Create GitHub release
info "🚀 Step 3: Creating GitHub release..."
# Safely expand optional flags even if array is unset/empty
if ! "$SCRIPT_DIR/create-github-release.sh" "$VERSION" ${CREATE_FLAGS+"${CREATE_FLAGS[@]}"}; then
    error "GitHub release creation failed"
    error "You may need to:"
    error "  1. Check GitHub authentication: gh auth status"
    error "  2. Ensure public repository exists: https://github.com/humanunsupervised/rakumux"
    error "  3. Delete existing release if needed: gh release delete v$VERSION --repo humanunsupervised/rakumux"
    exit 1
fi
success "GitHub release created"

# Step 4: Record local tracking tag for future release notes baselines
if [[ "$DRY_RUN" == "true" ]]; then
    warn "🏷️ Step 4: Dry run, not recording a release baseline tag"
else
    info "🏷️ Step 4: Recording local release baseline..."
    record_release_tracking_tag "$VERSION"
fi

# Step 5: Homebrew formula: render from the template, verify against the
# published release (brew audit/install/test), push to the tap. Drafts (assets
# not public yet) only render the formula; dry runs render into release/.
HOMEBREW_FLAGS=()
if [[ "$DRY_RUN" == "true" ]]; then
    HOMEBREW_FLAGS+=("--render-only" "--output" "$REPO_ROOT/release/rakumux.rb")
elif [[ "$DRAFT" == "true" ]]; then
    HOMEBREW_FLAGS+=("--render-only")
fi
info "🍺 Step 5: Releasing the Homebrew formula..."
if ! "$SCRIPT_DIR/release-homebrew.sh" "$VERSION" ${HOMEBREW_FLAGS+"${HOMEBREW_FLAGS[@]}"}; then
    error "Homebrew release failed"
    error "Rerun after fixing: ./install/scripts/release-homebrew.sh $VERSION"
    exit 1
fi
success "Homebrew formula released (commit install/homebrew/Formula/rakumux.rb)"

# Step 6: Publish to npm
NPM_FLAGS=()
if [[ "$DRY_RUN" == "true" ]]; then
    NPM_FLAGS+=("--dry-run")
fi
info "📦 Step 6: Publishing to npm..."
if "$SCRIPT_DIR/publish-npm.sh" "$VERSION" ${NPM_FLAGS+"${NPM_FLAGS[@]}"}; then
    if [[ "$DRY_RUN" == "true" ]]; then
        success "npm package staged and packed (dry run, not published)"
        NPM_PUBLISH_STATUS="dry-run"
    else
        success "Published to npm"
        NPM_PUBLISH_STATUS="success"
    fi
else
    error "npm publish failed during release automation"
    NPM_PUBLISH_STATUS="failed"
    NPM_PUBLISH_NOTE="Manual follow-up required: ensure 'npm whoami' succeeds, then run 'cd install/npm && npm publish --access public' or rerun the release script."
    warn "You can manually publish later with: cd install/npm && npm publish --access public"
fi

# Final summary
echo
success "🎉 RELEASE AUTOMATION COMPLETE!"
echo "================================"
info "Version: $VERSION"
info "GitHub Release: https://github.com/humanunsupervised/rakumux/releases/tag/v$VERSION"
if [[ "$NPM_PUBLISH_STATUS" == "failed" ]]; then
    warn "npm publish status: FAILED"
    [[ -n "$NPM_PUBLISH_NOTE" ]] && warn "$NPM_PUBLISH_NOTE"
else
    # macOS ships with Bash 3.2 which lacks ${var^^} uppercase expansion.
    # Use tr to keep the script compatible across environments.
    UPPER_STATUS="$(printf '%s' "$NPM_PUBLISH_STATUS" | tr '[:lower:]' '[:upper:]')"
    info "npm publish status: $UPPER_STATUS"
fi
echo
success "🎵 Rakumux is ready to use!"
