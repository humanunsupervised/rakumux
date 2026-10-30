#!/bin/bash
# Offline tests of publish-npm.sh --from-release: a stubbed curl serves fixture
# GitHub releases and npm registry answers, a stubbed npm records publishes.
# Runs on a copy of install/ in a temporary directory: never touches the
# network, the checkout's install/npm or a registry.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMP="$(mktemp -d)"
trap 'rm -rf "$TEMP"' EXIT

unset GITHUB_ACTIONS ACTIONS_ID_TOKEN_REQUEST_URL ACTIONS_ID_TOKEN_REQUEST_TOKEN \
    NPM_TOKEN NPM_OTP NPM_PUBLISH_OTP GH_TOKEN GITHUB_TOKEN
REAL_NPM="$(command -v npm)"
export REAL_NPM FIXTURE="$TEMP/fixture" TEST_LOG="$TEMP/log"
mkdir -p "$TEMP/bin" "$FIXTURE/api" "$FIXTURE/assets" "$TEST_LOG"

# A copy of what publish-npm.sh reads, without generated files.
WORK="$TEMP/root"
mkdir -p "$WORK/install/release-notes"
cp "$ROOT/LICENSE" "$WORK/"
cp -R "$ROOT/install/scripts" "$WORK/install/"
cp -R "$ROOT/install/npm" "$WORK/install/"
rm -rf "$WORK/install/npm/resources" "$WORK/install/npm/dist" \
    "$WORK/install/npm/LICENSE" "$WORK/install/npm/THIRD_PARTY_NOTICES"
printf '# Rakumux 1.2.3\n\nFixture notes.\n' > "$WORK/install/release-notes/1.2.3.md"
PUBLISH="$WORK/install/scripts/publish-npm.sh"

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# Fixture packages, the same for every release.
for platform in macos-intel macos-arm64 linux-x64; do
    package="$TEMP/packages/rakumux-$platform"
    mkdir -p "$package/shell"
    for binary in rakumux rakumux-ptyd rakumux-api rakumux-mcp; do
        printf '#!/bin/sh\necho %s\n' "$binary" > "$package/$binary"
    done
    printf '#!/bin/bash\n' > "$package/rakumux.sh"
    printf '#!/bin/bash\n' > "$package/rakumux-local.sh"
    printf '#!/bin/bash\n' > "$package/shell/gwr_load.sh"
    printf 'Fixture notices for %s\n' "$platform" > "$package/THIRD_PARTY_NOTICES"
    tar -czf "$TEMP/packages/rakumux-$platform.tar.gz" -C "$TEMP/packages" "rakumux-$platform"
done

# release VERSION STATE [ASSET...]: a GitHub release with those assets
# (default: all), STATE published, prerelease or draft.
release() {
    local version="$1" state="$2" dir="$FIXTURE/assets/v$1" assets name
    shift 2
    if [[ $# -eq 0 ]]; then
        set -- checksums.txt rakumux-macos-intel.tar.gz rakumux-macos-arm64.tar.gz rakumux-linux-x64.tar.gz
    fi
    mkdir -p "$dir"
    assets=""
    for name in "$@"; do
        if [[ "$name" == checksums.txt ]]; then continue; fi
        cp "$TEMP/packages/$name" "$dir/"
        printf '%s  %s\n' "$(sha256_of "$dir/$name")" "$name" >> "$dir/checksums.txt"
    done
    for name in "$@"; do assets="$assets{\"name\":\"$name\"},"; done
    printf '{"tag_name":"v%s","draft":%s,"prerelease":%s,"assets":[%s]}\n' "$version" \
        "$([[ "$state" == draft ]] && echo true || echo false)" \
        "$([[ "$state" == prerelease ]] && echo true || echo false)" \
        "${assets%,}" > "$FIXTURE/api/v$version.json"
}

cat > "$TEMP/bin/curl" <<'EOF'
#!/bin/bash
set -eu
out="" format="" url="" fail=false config=false
printf '%s\n' "$*" >> "$TEST_LOG/curl-argv"
while [[ $# -gt 0 ]]; do
    case "$1" in
        -o) out="$2"; shift 2 ;;
        -w) format="$2"; shift 2 ;;
        -H|--retry) shift 2 ;;
        --config) config=true; shift 2 ;;
        -fsSL) fail=true; shift ;;
        -*) shift ;;
        *) url="$1"; shift ;;
    esac
done
if [[ "$config" == true ]]; then cat >> "$TEST_LOG/curl-config"; fi
file=""
case "$url" in
    https://api.github.com/repos/humanunsupervised/rakumux/releases/tags/*)
        file="$FIXTURE/api/${url##*/}.json" ;;
    https://github.com/humanunsupervised/rakumux/releases/download/*)
        file="$FIXTURE/assets/${url#https://github.com/humanunsupervised/rakumux/releases/download/}"
        printf '%s\n' "$url" >> "$TEST_LOG/downloads" ;;
    https://registry.npmjs.org/*) status="${NPM_TEST_REGISTRY:-404}" ;;
    *) exit 6 ;;
esac
if [[ -n "$file" ]]; then
    if [[ -f "$file" ]]; then status=200; else status=404; fi
fi
if [[ "$status" == 200 && -n "$file" && -n "$out" ]]; then cp "$file" "$out"; fi
if [[ "$fail" == true && "$status" != 200 ]]; then exit 22; fi
if [[ -n "$format" ]]; then printf '%s' "$status"; fi
EOF
cat > "$TEMP/bin/npm" <<'EOF'
#!/bin/bash
printf '%s\n' "$*" >> "$TEST_LOG/npm"
case "$1" in
    --version) printf '%s\n' "${NPM_TEST_VERSION:-11.21.1}" ;;
    pack) exec "$REAL_NPM" "$@" ;;
    publish) exit 0 ;;
    *) exit 99 ;;
esac
EOF
chmod +x "$TEMP/bin/"*
export PATH="$TEMP/bin:$PATH"

reset_logs() { rm -f "$TEST_LOG"/*; }
expect_failure() {
    local pattern="$1"
    shift
    if "$@" > "$TEMP/output" 2>&1; then
        printf 'Unexpected success: %s\n' "$*" >&2
        exit 1
    fi
    if ! grep -q -- "$pattern" "$TEMP/output"; then
        printf 'Expected "%s" from: %s\n' "$pattern" "$*" >&2
        cat "$TEMP/output" >&2
        exit 1
    fi
}
# absent PATTERN FILE...: fails when a file has the pattern (`! grep` would
# not stop the test under set -e).
absent() {
    if grep -q -- "$1" "${@:2}" 2>/dev/null; then
        printf 'Unexpected "%s" in %s\n' "$1" "${*:2}" >&2
        exit 1
    fi
}
no_publish() { absent '^publish' "$TEST_LOG/npm"; }

release 1.2.3 published
release 1.2.4 published
printf 'tampered\n' >> "$FIXTURE/assets/v1.2.4/rakumux-macos-arm64.tar.gz"
release 1.2.5 published checksums.txt rakumux-macos-arm64.tar.gz rakumux-linux-x64.tar.gz
release 1.2.6 draft
release 1.3.0 prerelease
release 2.0.0-rc.1 published

# A dry run stages the release's packages: no credentials, no registry.
reset_logs
bash "$PUBLISH" 1.2.3 --from-release --dry-run > "$TEMP/output"
grep -q 'staged and packed for --tag latest, not published' "$TEMP/output"
grep -q '"version": "1.2.3"' "$WORK/install/npm/package.json"
for platform in macos-intel macos-arm64 linux-x64; do
    [[ -x "$WORK/install/npm/resources/prebuilt/$platform/rakumux-mcp" ]]
done
grep -qx 'Fixture notices for linux-x64' "$WORK/install/npm/THIRD_PARTY_NOTICES"
cmp -s "$ROOT/LICENSE" "$WORK/install/npm/LICENSE"
[[ "$(wc -l < "$TEST_LOG/downloads")" -eq 4 ]]
absent 'registry.npmjs.org' "$TEST_LOG/curl-argv"
absent 'Authorization' "$TEST_LOG/curl-config"
no_publish

# A token reaches GitHub as curl config on stdin, never as an argument or output.
reset_logs
GH_TOKEN=fixture-secret-token bash "$PUBLISH" 1.2.3 --from-release --dry-run > "$TEMP/output"
grep -qx 'header = "Authorization: Bearer fixture-secret-token"' "$TEST_LOG/curl-config"
absent 'fixture-secret-token' "$TEST_LOG/curl-argv" "$TEMP/output"

expect_failure 'Checksum mismatch for rakumux-macos-arm64.tar.gz' bash "$PUBLISH" 1.2.4 --from-release --dry-run
expect_failure 'has no rakumux-macos-intel.tar.gz' bash "$PUBLISH" 1.2.5 --from-release --dry-run
expect_failure 'still a draft' bash "$PUBLISH" 1.2.6 --from-release --dry-run
expect_failure 'no published release v9.9.9' bash "$PUBLISH" 9.9.9 --from-release --dry-run
expect_failure 'Invalid version' bash "$PUBLISH" v1.2.3 --from-release --dry-run

# Prereleases go to next, never latest, and fail before downloading.
bash "$PUBLISH" 1.3.0 --from-release --dry-run > "$TEMP/output"
grep -q 'for --tag next' "$TEMP/output"
bash "$PUBLISH" 2.0.0-rc.1 --from-release --dry-run > "$TEMP/output"
grep -q 'for --tag next' "$TEMP/output"
reset_logs
expect_failure 'never latest' bash "$PUBLISH" 1.3.0 --from-release --tag=latest --dry-run
[[ ! -e "$TEST_LOG/downloads" ]]

# A version already on npm is left alone: nothing downloaded or published.
reset_logs
NPM_TEST_REGISTRY=200 bash "$PUBLISH" 1.2.3 --from-release > "$TEMP/output"
grep -q 'already on npm; nothing to publish' "$TEMP/output"
[[ ! -e "$TEST_LOG/downloads" ]]
no_publish
expect_failure 'registry answered 500' env NPM_TEST_REGISTRY=500 bash "$PUBLISH" 1.2.3 --from-release

# GitHub Actions with id-token: write publishes through trusted publishing.
oidc=(env GITHUB_ACTIONS=true ACTIONS_ID_TOKEN_REQUEST_URL=https://token.example.invalid)
reset_logs
"${oidc[@]}" bash "$PUBLISH" 1.2.3 --from-release --tag=latest > "$TEMP/output"
grep -qx 'publish --access public --tag latest' "$TEST_LOG/npm"
absent '^whoami' "$TEST_LOG/npm"
grep -q '^::notice::Published rakumux@1.2.3 to npm under latest' "$TEMP/output"
reset_logs
"${oidc[@]}" bash "$PUBLISH" 1.3.0 --from-release --tag=next > "$TEMP/output"
grep -qx 'publish --access public --tag next' "$TEST_LOG/npm"
reset_logs
"${oidc[@]}" NPM_TEST_REGISTRY=200 bash "$PUBLISH" 1.2.3 --from-release > "$TEMP/output"
grep -q '^::notice::rakumux@1.2.3 is already on npm' "$TEMP/output"
no_publish
reset_logs
expect_failure 'needs 11.5.1 or newer' "${oidc[@]}" NPM_TEST_VERSION=10.9.3 bash "$PUBLISH" 1.2.3 --from-release
no_publish

printf 'npm release staging and publishing tests passed.\n'
