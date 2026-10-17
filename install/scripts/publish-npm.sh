#!/bin/bash

###############################################################################
# publish-npm.sh - Publish Orchestra to the npm registry
#
# Stages install/npm from the release tarballs (stage-npm-package.sh: version,
# the binaries per platform, launcher scripts, THIRD_PARTY_NOTICES), checks
# the packed file list, then publishes @humanu/orchestra.
#
# Usage: publish-npm.sh <version> [--dry-run]
#   --dry-run   Stage (missing platforms allowed) and `npm pack --dry-run`;
#               no npm credentials needed, nothing is published.
# Environment: NPM_TOKEN (or .env, or an `npm login` session), NPM_OTP.
###############################################################################

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info() { printf "${BLUE}[NPM]${NC} %s\n" "$*"; }
success() { printf "${GREEN}[NPM]${NC} %s\n" "$*"; }
warn() { printf "${YELLOW}[NPM]${NC} %s\n" "$*"; }
error() { printf "${RED}[NPM]${NC} %s\n" "$*"; }

VERSION=""
DRY_RUN=false
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=true ;;
        -*)
            error "Unknown option: $arg"
            exit 2
            ;;
        *) VERSION="$arg" ;;
    esac
done
if [[ -z "$VERSION" ]]; then
    error "Version number required"
    echo "Usage: $0 <version> [--dry-run]"
    exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
NPM_DIR="$REPO_ROOT/install/npm"
NPM_OTP_VALUE="${NPM_OTP:-${NPM_PUBLISH_OTP:-}}"

# Files the packed package must contain (in addition to package.json).
REQUIRED_PACKED_FILES=(
    "LICENSE"
    "THIRD_PARTY_NOTICES"
    "README.md"
    "install.js"
    "bin/orchestra.js"
    "bin/orchestra-local.js"
    "resources/scripts/orchestra.sh"
    "resources/scripts/orchestra-local.sh"
    "resources/scripts/shell/gwr_load.sh"
)

info "Publishing Orchestra v$VERSION to npm..."

STAGE_FLAGS=()
if [[ "$DRY_RUN" == "true" ]]; then
    STAGE_FLAGS+=("--allow-partial")
fi
"$SCRIPT_DIR/stage-npm-package.sh" "$VERSION" ${STAGE_FLAGS+"${STAGE_FLAGS[@]}"}

# Check the packed file list before anything is published.
info "Checking packed file list..."
pack_json="$(cd "$NPM_DIR" && npm pack --dry-run --json 2>/dev/null)"
packed_files="$(printf '%s' "$pack_json" | node -e '
const data = JSON.parse(require("fs").readFileSync(0, "utf8"));
for (const file of data[0].files) console.log(file.path);
')"
for file in "${REQUIRED_PACKED_FILES[@]}"; do
    if ! grep -qx "$file" <<<"$packed_files"; then
        error "Packed package is missing $file"
        exit 1
    fi
done
for platform_dir in "$NPM_DIR"/resources/prebuilt/*/; do
    platform="$(basename "$platform_dir")"
    for binary in orchestra orchestra-ptyd orchestra-api orchestra-mcp; do
        if ! grep -qx "resources/prebuilt/$platform/$binary" <<<"$packed_files"; then
            error "Packed package is missing resources/prebuilt/$platform/$binary"
            exit 1
        fi
    done
done
if grep -E '(^|/)(orchestra-cli|services\.sh|env-copy)' <<<"$packed_files"; then
    error "Packed package contains removed files (above)"
    exit 1
fi
success "Packed file list OK ($(wc -l <<<"$packed_files" | tr -d ' ') files)"

if [[ "$DRY_RUN" == "true" ]]; then
    printf '%s\n' "$packed_files" | sed 's/^/    /'
    success "Dry run: @humanu/orchestra@$VERSION staged and packed, not published"
    exit 0
fi

# Configure npm auth.
# When NPM_TOKEN is configured, use it explicitly so release automation does not
# silently fall back to an interactive login session.
NPM_USERCONFIG_FILE=""
DOTENV_FILE="$REPO_ROOT/.env"
if [[ -z "${NPM_TOKEN:-}" && -f "$DOTENV_FILE" ]]; then
    set -a
    # shellcheck disable=SC1090
    source "$DOTENV_FILE"
    set +a
fi

if [[ -n "${NPM_TOKEN:-}" ]]; then
    info "Using NPM_TOKEN for authentication"
    NPM_USERCONFIG_FILE="$(mktemp)"
    printf '%s\n' "//registry.npmjs.org/:_authToken=${NPM_TOKEN}" >"$NPM_USERCONFIG_FILE"
    export NPM_CONFIG_USERCONFIG="$NPM_USERCONFIG_FILE"
    trap 'rm -f "$NPM_USERCONFIG_FILE"' EXIT
elif npm whoami >/dev/null 2>&1; then
    info "Using existing npm login session"
fi

if ! npm whoami >/dev/null 2>&1; then
    error "Not logged in to npm. Run 'npm login' or set a publish-capable NPM_TOKEN"
    exit 1
fi

info "Publishing to npm registry..."
PUBLISH_ARGS=(publish --access public)
if [[ -n "$NPM_OTP_VALUE" ]]; then
    info "Using npm OTP from environment"
    PUBLISH_ARGS+=(--otp "$NPM_OTP_VALUE")
fi

if (cd "$NPM_DIR" && npm "${PUBLISH_ARGS[@]}"); then
    success "Successfully published @humanu/orchestra@$VERSION to npm!"
    info "View at: https://www.npmjs.com/package/@humanu/orchestra"
    info "Install with: npm install -g @humanu/orchestra"
else
    error "npm publish failed"
    error "Common issues:"
    error "  - Version already exists (can't republish same version)"
    error "  - Not logged in (run: npm login)"
    error "  - Publish requires an OTP or a granular access token with bypass 2FA"
    error "  - A stale NPM_TOKEN in .env can override a working npm login session"
    exit 1
fi
