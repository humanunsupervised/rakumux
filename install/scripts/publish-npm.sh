#!/bin/bash

###############################################################################
# publish-npm.sh - Publish Rakumux to the npm registry
#
# Stages install/npm from release tarballs (stage-npm-package.sh: version,
# the binaries per platform, launcher scripts, THIRD_PARTY_NOTICES), checks
# the packed file list, then publishes rakumux.
#
# Usage: publish-npm.sh <version> [--from-release] [--tag=TAG] [--dry-run]
#   --from-release  Stage from the published GitHub release v<version>, the
#                   one install.sh installs: its three tarballs, checked
#                   against its checksums.txt. Without it, from a local build
#                   in release/. The release workflow uses it, and so should
#                   a publish by hand.
#   --tag=TAG       npm dist-tag. Default: latest, or next for a prerelease
#                   (a version with a "-", or a GitHub release marked as
#                   one). A prerelease never goes to latest.
#   --dry-run       Stage and `npm pack --dry-run`; no npm credentials needed,
#                   nothing is published. Without --from-release, missing
#                   platforms are allowed.
# A version already on npm is left alone (exit 0), so re-runs are safe.
# Auth: npm trusted publishing (OIDC) in GitHub Actions with id-token: write;
# otherwise NPM_TOKEN (or .env, or an `npm login` session) and NPM_OTP.
# GH_TOKEN or GITHUB_TOKEN, when set, is used to read the release's metadata.
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
FROM_RELEASE=false
DIST_TAG=""
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=true ;;
        --from-release) FROM_RELEASE=true ;;
        --tag=*) DIST_TAG="${arg#--tag=}" ;;
        -*)
            error "Unknown option: $arg"
            exit 2
            ;;
        *) VERSION="$arg" ;;
    esac
done
if [[ -z "$VERSION" ]]; then
    error "Version number required"
    echo "Usage: $0 <version> [--from-release] [--tag=TAG] [--dry-run]"
    exit 1
fi
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
    error "Invalid version: $VERSION (expected e.g. 0.1.33, without the v)"
    exit 2
fi
if [[ -n "$DIST_TAG" && ! "$DIST_TAG" =~ ^[A-Za-z][A-Za-z0-9._-]*$ ]]; then
    error "Invalid npm dist-tag: $DIST_TAG"
    exit 2
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
NPM_DIR="$REPO_ROOT/install/npm"
NPM_OTP_VALUE="${NPM_OTP:-${NPM_PUBLISH_OTP:-}}"
GITHUB_REPO="humanunsupervised/rakumux"
NPM_REGISTRY="https://registry.npmjs.org"
PLATFORMS=(macos-intel macos-arm64 linux-x64)
RELEASE_ASSETS=(checksums.txt rakumux-macos-intel.tar.gz rakumux-macos-arm64.tar.gz rakumux-linux-x64.tar.gz)
# npm trusted publishing (OIDC) needs at least this npm.
TRUSTED_PUBLISHING_NPM="11.5.1"
PACKAGE="$(node -p 'require(process.argv[1]).name' "$NPM_DIR/package.json")"

# Files the packed package must contain (in addition to package.json).
REQUIRED_PACKED_FILES=(
    "LICENSE"
    "THIRD_PARTY_NOTICES"
    "README.md"
    "install.js"
    "bin/rakumux.js"
    "bin/rakumux-local.js"
    "resources/scripts/rakumux.sh"
    "resources/scripts/rakumux-local.sh"
    "resources/scripts/shell/gwr_load.sh"
)

WORK_DIR=""
NPM_USERCONFIG_FILE=""
cleanup() {
    if [[ -n "$WORK_DIR" ]]; then rm -rf "$WORK_DIR"; fi
    if [[ -n "$NPM_USERCONFIG_FILE" ]]; then rm -f "$NPM_USERCONFIG_FILE"; fi
}
trap cleanup EXIT

# A line GitHub Actions shows on the run's summary, and nothing elsewhere.
annotate() {
    if [[ "${GITHUB_ACTIONS:-}" == "true" ]]; then printf '::%s::%s\n' "$1" "$2"; fi
}

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | cut -d' ' -f1
    else
        shasum -a 256 "$1" | cut -d' ' -f1
    fi
}

# Is npm version $1 at least $2 (major.minor.patch)?
version_at_least() {
    node -e '
const parse = (v) => v.split("-")[0].split(".").map(Number);
const [have, want] = process.argv.slice(1).map(parse);
for (let i = 0; i < 3; i++) if (have[i] !== want[i]) process.exit(have[i] > want[i] ? 0 : 1);
' "$1" "$2"
}

# Check that the GitHub release v$VERSION is published and has every
# package. Sets RELEASE_IS_PRERELEASE.
RELEASE_IS_PRERELEASE=false
read_release() {
    local tag="v$VERSION" token="${GH_TOKEN:-${GITHUB_TOKEN:-}}"
    local api_url="https://api.github.com/repos/$GITHUB_REPO/releases/tags/$tag"
    local metadata="$WORK_DIR/release.json" auth="" status facts name

    info "Reading GitHub release $tag of $GITHUB_REPO..."
    # The token goes to curl as config on stdin, never on its command line.
    if [[ -n "$token" ]]; then auth="header = \"Authorization: Bearer $token\""; fi
    status="$(printf '%s\n' "$auth" |
        curl -sSL --retry 3 --config - -H 'Accept: application/vnd.github+json' \
            -o "$metadata" -w '%{http_code}' "$api_url")" || {
        error "Couldn't reach GitHub for release $tag"
        exit 1
    }
    case "$status" in
        200) ;;
        404)
            error "$GITHUB_REPO has no published release $tag (a draft doesn't count)"
            exit 1
            ;;
        *)
            error "GitHub answered $status for release $tag"
            exit 1
            ;;
    esac

    facts="$(node -e '
const release = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
console.log(release.draft ? "draft" : release.prerelease ? "prerelease" : "published");
for (const asset of release.assets || []) console.log("asset " + asset.name);
' "$metadata")"
    case "$(head -1 <<<"$facts")" in
        draft)
            error "Release $tag is still a draft; publish it on GitHub first"
            exit 1
            ;;
        prerelease) RELEASE_IS_PRERELEASE=true ;;
    esac

    for name in "${RELEASE_ASSETS[@]}"; do
        if ! grep -qxF "asset $name" <<<"$facts"; then
            error "Release $tag has no $name; npm needs all of: ${PLATFORMS[*]}"
            exit 1
        fi
    done
}

# Download the release's assets into $1, check every tarball against its
# checksums.txt, and add what stage-npm-package.sh reads besides them.
download_release() {
    local dir="$1" tag="v$VERSION" platform name expected
    mkdir -p "$dir"
    for name in "${RELEASE_ASSETS[@]}"; do
        info "Downloading $name..."
        curl -fsSL --retry 3 -o "$dir/$name" \
            "https://github.com/$GITHUB_REPO/releases/download/$tag/$name" || {
            error "Couldn't download $name from release $tag"
            exit 1
        }
    done
    for platform in "${PLATFORMS[@]}"; do
        name="rakumux-$platform.tar.gz"
        expected="$(awk -v f="$name" '$2 == f { print $1 }' "$dir/checksums.txt")"
        if [[ ! "$expected" =~ ^[0-9a-f]{64}$ ]]; then
            error "checksums.txt of release $tag has no single entry for $name"
            exit 1
        fi
        if [[ "$(sha256_of "$dir/$name")" != "$expected" ]]; then
            error "Checksum mismatch for $name of release $tag"
            exit 1
        fi
    done
    success "Release $tag: all ${#PLATFORMS[@]} tarballs match checksums.txt"

    mkdir "$WORK_DIR/notices"
    if ! tar -xzf "$dir/rakumux-linux-x64.tar.gz" -C "$WORK_DIR/notices" \
        rakumux-linux-x64/THIRD_PARTY_NOTICES 2>/dev/null; then
        error "rakumux-linux-x64.tar.gz of release $tag has no THIRD_PARTY_NOTICES"
        exit 1
    fi
    mv "$WORK_DIR/notices/rakumux-linux-x64/THIRD_PARTY_NOTICES" "$dir/THIRD_PARTY_NOTICES"

    # stage-npm-package.sh checks the first line; the notes aren't packed.
    local notes="$REPO_ROOT/install/release-notes/$VERSION.md"
    {
        printf '# Rakumux v%s\n' "$VERSION"
        if [[ -f "$notes" ]]; then tail -n +2 "$notes"; fi
    } >"$dir/RELEASE_NOTES.md"
}

if [[ "$DRY_RUN" == "true" ]]; then
    info "Dry run of $PACKAGE@$VERSION for npm..."
else
    info "Publishing $PACKAGE@$VERSION to npm..."
    # A version is published once: re-running a release leaves it alone.
    registry_status="$(curl -sS --retry 3 -o /dev/null -w '%{http_code}' \
        "$NPM_REGISTRY/$PACKAGE/$VERSION")" || {
        error "Couldn't reach the npm registry"
        exit 1
    }
    case "$registry_status" in
        200)
            success "$PACKAGE@$VERSION is already on npm; nothing to publish"
            annotate notice "$PACKAGE@$VERSION is already on npm; nothing to publish"
            exit 0
            ;;
        404) ;;
        *)
            error "The npm registry answered $registry_status for $PACKAGE@$VERSION"
            exit 1
            ;;
    esac
fi

if [[ "$FROM_RELEASE" == "true" ]]; then
    WORK_DIR="$(mktemp -d)"
    read_release
fi

PRERELEASE="$RELEASE_IS_PRERELEASE"
if [[ "$VERSION" == *-* ]]; then PRERELEASE=true; fi
if [[ -z "$DIST_TAG" ]]; then
    DIST_TAG=latest
    if [[ "$PRERELEASE" == "true" ]]; then DIST_TAG=next; fi
fi
if [[ "$PRERELEASE" == "true" && "$DIST_TAG" == "latest" ]]; then
    error "v$VERSION is a prerelease: it goes to npm under next (or another --tag), never latest"
    exit 1
fi

STAGE_FLAGS=()
if [[ "$FROM_RELEASE" == "true" ]]; then
    download_release "$WORK_DIR/release"
    STAGE_FLAGS+=("--release-dir=$WORK_DIR/release")
elif [[ "$DRY_RUN" == "true" ]]; then
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
    for binary in rakumux rakumux-ptyd rakumux-api rakumux-mcp; do
        if ! grep -qx "resources/prebuilt/$platform/$binary" <<<"$packed_files"; then
            error "Packed package is missing resources/prebuilt/$platform/$binary"
            exit 1
        fi
    done
done
if grep -E '(^|/)(rakumux-cli|services\.sh|env-copy)' <<<"$packed_files"; then
    error "Packed package contains removed files (above)"
    exit 1
fi
success "Packed file list OK ($(wc -l <<<"$packed_files" | tr -d ' ') files)"

if [[ "$DRY_RUN" == "true" ]]; then
    printf '%s\n' "$packed_files" | sed 's/^/    /'
    success "Dry run: $PACKAGE@$VERSION staged and packed for --tag $DIST_TAG, not published"
    exit 0
fi

# Configure npm auth.
# In GitHub Actions with id-token: write, npm exchanges the job's OIDC token
# for a short-lived publish token (trusted publishing) and adds provenance;
# there is no npm secret. Otherwise, when NPM_TOKEN is configured, use it
# explicitly so release automation does not silently fall back to an
# interactive login session.
TRUSTED_PUBLISHING=false
if [[ "${GITHUB_ACTIONS:-}" == "true" && -n "${ACTIONS_ID_TOKEN_REQUEST_URL:-}" && -z "${NPM_TOKEN:-}" ]]; then
    TRUSTED_PUBLISHING=true
fi

if [[ "$TRUSTED_PUBLISHING" == "true" ]]; then
    npm_version="$(npm --version)"
    if ! version_at_least "$npm_version" "$TRUSTED_PUBLISHING_NPM"; then
        error "npm $npm_version can't use trusted publishing; it needs $TRUSTED_PUBLISHING_NPM or newer"
        exit 1
    fi
    info "Using npm trusted publishing (OIDC) with npm $npm_version"
else
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
    elif npm whoami >/dev/null 2>&1; then
        info "Using existing npm login session"
    fi

    if ! npm whoami >/dev/null 2>&1; then
        error "Not logged in to npm. Run 'npm login' or set a publish-capable NPM_TOKEN"
        exit 1
    fi
fi

info "Publishing to npm registry under --tag $DIST_TAG..."
PUBLISH_ARGS=(publish --access public --tag "$DIST_TAG")
if [[ -n "$NPM_OTP_VALUE" && "$TRUSTED_PUBLISHING" != "true" ]]; then
    info "Using npm OTP from environment"
    PUBLISH_ARGS+=(--otp "$NPM_OTP_VALUE")
fi

if (cd "$NPM_DIR" && npm "${PUBLISH_ARGS[@]}"); then
    success "Successfully published $PACKAGE@$VERSION to npm (--tag $DIST_TAG)!"
    annotate notice "Published $PACKAGE@$VERSION to npm under $DIST_TAG"
    info "View at: https://www.npmjs.com/package/$PACKAGE"
    info "Install with: npm install -g $PACKAGE"
else
    error "npm publish failed"
    error "Common issues:"
    if [[ "$TRUSTED_PUBLISHING" == "true" ]]; then
        error "  - No trusted publisher on npmjs.com for this repository and workflow"
        error "    (package settings: GitHub Actions, $GITHUB_REPO, release.yml)"
        error "  - The job lacks permissions: id-token: write"
    else
        error "  - Not logged in (run: npm login)"
        error "  - Publish requires an OTP or a granular access token with bypass 2FA"
        error "  - A stale NPM_TOKEN in .env can override a working npm login session"
    fi
    error "  - Version already exists (can't republish same version)"
    exit 1
fi
