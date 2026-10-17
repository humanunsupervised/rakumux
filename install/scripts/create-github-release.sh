#!/bin/bash

# Create GitHub Release with Binary Assets
# Automates the GitHub release creation and asset upload process
set -e

VERSION=${1:-"0.1.0"}
REPO="humanunsupervised/rakumux"
RELEASE_DIR="release"
DRAFT="false"
PRERELEASE="false"
DRY_RUN="false"
NOTES_LAST_COMMITS=""

# Parse optional flags
shift || true
while [[ $# -gt 0 ]]; do
  case "$1" in
    --repo=*) REPO="${1#*=}"; shift ;;
    --notes-last=*) NOTES_LAST_COMMITS="${1#*=}"; shift ;;
    --draft) DRAFT="true"; shift ;;
    --prerelease) PRERELEASE="true"; shift ;;
    --dry-run) DRY_RUN="true"; shift ;;
    *) shift ;;
  esac
done

echo "🚀 Creating GitHub release v$VERSION for $REPO..."

# Check if we're authenticated with GitHub (unless dry-run)
if [[ "$DRY_RUN" != "true" ]]; then
  if ! gh auth status >/dev/null 2>&1; then
      echo "❌ Not authenticated with GitHub CLI"
      echo ""
      echo "🔧 Quick Setup (choose one):"
      echo "1. Personal Access Token: echo 'your_token' | gh auth login --with-token"
      echo "2. SSH: gh auth login --git-protocol ssh"
      echo "3. Environment: export GITHUB_TOKEN='your_token'"
      echo ""
      echo "💡 Get token at: https://github.com/settings/tokens (needs 'repo' scope)"
      exit 1
  fi
fi

# Create dynamic release notes
echo "📝 Preparing release notes for v$VERSION..."
TEMP_NOTES=$(mktemp)
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NOTES_ARGS=("$VERSION" "--repo=$REPO")
if [[ -n "$NOTES_LAST_COMMITS" ]]; then
  NOTES_ARGS+=("--notes-last=$NOTES_LAST_COMMITS")
fi
# Curated notes (install/release-notes/<version>.md) win over notes generated from commits.
CURATED_NOTES="$SCRIPT_DIR/../release-notes/$VERSION.md"
if [[ -f "$CURATED_NOTES" ]]; then
    echo "📝 Using install/release-notes/$VERSION.md"
    cp "$CURATED_NOTES" "$TEMP_NOTES"
elif ! bash "$SCRIPT_DIR/generate-release-notes.sh" "${NOTES_ARGS[@]}" > "$TEMP_NOTES" 2>/dev/null; then
    echo "Release $VERSION is out!" > "$TEMP_NOTES"
fi

if [[ "$DRY_RUN" == "true" ]]; then
  echo "🔎 DRY RUN: Would create release with the following notes:\n" 
  sed 's/^/    /' "$TEMP_NOTES" | head -200
  echo ""
  echo "🔎 DRY RUN: Would run: gh release create v$VERSION --repo $REPO --title 'Rakumux v$VERSION' --notes-file <notes> --latest"
  [[ -n "$NOTES_LAST_COMMITS" ]] && echo "  + notes generated from last $NOTES_LAST_COMMITS commits"
  [[ "$DRAFT" == "true" ]] && echo "  + with --draft"
  [[ "$PRERELEASE" == "true" ]] && echo "  + with --prerelease"
  rm -f "$TEMP_NOTES"
  exit 0
fi

# Check if release directory exists
if [ ! -d "$RELEASE_DIR" ]; then
    echo "❌ Release directory not found. Run ./install/scripts/build-release.sh first"
    rm -f "$TEMP_NOTES"
    exit 1
fi

# Check if release notes exist (built notes are separate; we still allow creating with TEMP_NOTES)
if [ ! -f "$RELEASE_DIR/RELEASE_NOTES.md" ]; then
    echo "ℹ️  Warning: build RELEASE_NOTES.md not found; continuing with generated notes"
fi

# Check if tag already exists
if gh release view "v$VERSION" --repo "$REPO" >/dev/null 2>&1; then
    echo "❌ Release v$VERSION already exists"
    echo "To delete and recreate: gh release delete v$VERSION --repo $REPO --yes"
    rm -f "$TEMP_NOTES"
    exit 1
fi

# Build flags
EXTRA_FLAGS=()
[[ "$DRAFT" == "true" ]] && EXTRA_FLAGS+=("--draft")
[[ "$PRERELEASE" == "true" ]] && EXTRA_FLAGS+=("--prerelease")

# Create release
echo "📝 Creating release v$VERSION..."

gh release create "v$VERSION" \
    --repo "$REPO" \
    --title "Rakumux v$VERSION" \
    --notes-file "$TEMP_NOTES" \
    --latest \
    "${EXTRA_FLAGS[@]}"

echo "✅ Release created successfully!"

# Upload binary assets
echo "📦 Uploading release assets..."

# Find and upload all .tar.gz files
for package in "$RELEASE_DIR"/*.tar.gz; do
    if [ -f "$package" ]; then
        package_name=$(basename "$package")
        echo "  📤 Uploading $package_name..."
        gh release upload "v$VERSION" "$package" \
            --repo "$REPO" \
            --clobber
        echo "  ✅ Uploaded $package_name"
    fi
done

# Upload checksums file if it exists
if [ -f "$RELEASE_DIR/checksums.txt" ]; then
    echo "  📤 Uploading checksums.txt..."
    gh release upload "v$VERSION" "$RELEASE_DIR/checksums.txt" \
        --repo "$REPO" \
        --clobber
    echo "  ✅ Uploaded checksums.txt"
fi

# Clean up temporary file
rm -f "$TEMP_NOTES"

echo ""
echo "🎉 GitHub release v$VERSION created successfully!"
echo "🔗 https://github.com/$REPO/releases/tag/v$VERSION"
