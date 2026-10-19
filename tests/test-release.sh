#!/bin/bash
# Offline release-entry tests: no network, tags, pushes or package publishing.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TEMP="$(mktemp -d)"
trap 'rm -rf "$TEMP"' EXIT
mkdir "$TEMP/bin"
export RELEASE_TEST_LOG="$TEMP/operations.log"
export RELEASE_TEST_SHA=0123456789012345678901234567890123456789
export PATH="$TEMP/bin:$PATH"

cat >"$TEMP/bin/git" <<'EOF'
#!/bin/bash
set -eu
case "$1" in
    status) printf '%s' "${RELEASE_TEST_STATUS:-}" ;;
    remote) printf '%s\n' "${RELEASE_TEST_REMOTE:-git@github.com:humanunsupervised/rakumux.git}" ;;
    rev-parse) printf '%s\n' "$RELEASE_TEST_SHA" ;;
    show-ref) exit "${RELEASE_TEST_LOCAL_TAG_STATUS:-1}" ;;
    ls-remote)
        case "$3" in
            refs/heads/main) printf '%s\trefs/heads/main\n' "${RELEASE_TEST_MAIN:-$RELEASE_TEST_SHA}" ;;
            refs/tags/*) printf '%s' "${RELEASE_TEST_REMOTE_TAG:-}" ;;
            *) exit 2 ;;
        esac
        ;;
    tag|push) printf '%s\n' "$*" >> "$RELEASE_TEST_LOG" ;;
    *) exit 2 ;;
esac
EOF
cat >"$TEMP/bin/curl" <<'EOF'
#!/bin/bash
set -eu
output=""
while [[ $# -gt 0 ]]; do
    case "$1" in --output) output="$2"; shift 2 ;; *) shift ;; esac
done
printf '%s\n' "${RELEASE_TEST_BODY:-{\"tag_name\":\"v0.1.4\"}}" > "$output"
printf '%s' "${RELEASE_TEST_HTTP:-200}"
EOF
for name in npm brew gh; do
    printf '#!/bin/bash\nexit 99\n' > "$TEMP/bin/$name"
done
chmod +x "$TEMP/bin/"*

expect_failure() {
    if "$@" >"$TEMP/output" 2>&1; then
        printf 'Unexpected release success: %s\n' "$*" >&2
        exit 1
    fi
}

bash "$ROOT/release.sh" 0.1.0 --dry-run >"$TEMP/output"
[[ ! -e "$RELEASE_TEST_LOG" ]]
grep -q 'Dry run' "$TEMP/output"

expect_failure env RELEASE_TEST_STATUS=' M source.rs' bash "$ROOT/release.sh" 0.1.0
expect_failure env RELEASE_TEST_REMOTE='git@github.com:private/internal.git' bash "$ROOT/release.sh" 0.1.0
expect_failure env RELEASE_TEST_MAIN=not-pushed bash "$ROOT/release.sh" 0.1.0
expect_failure env RELEASE_TEST_REMOTE_TAG=already-exists bash "$ROOT/release.sh" 0.1.0
expect_failure env RELEASE_TEST_LOCAL_TAG_STATUS=0 bash "$ROOT/release.sh" 0.1.0
expect_failure bash "$ROOT/release.sh" 0.1.0 --draft --prerelease
expect_failure bash "$ROOT/release.sh" --unknown-option
expect_failure bash "$ROOT/release.sh" 0.1.1 --targets=unsupported
expect_failure bash "$ROOT/release.sh" 0.1.1 --targets=
expect_failure bash "$ROOT/release.sh" 0.1.1 --targets=linux-x64,linux-x64
expect_failure bash "$ROOT/release.sh" 0.1.1 --targets=linux-x64,
[[ ! -e "$RELEASE_TEST_LOG" ]]

bash "$ROOT/release.sh" 0.1.0 --draft >"$TEMP/output"
grep -q 'Release-Mode: draft' "$RELEASE_TEST_LOG"
grep -q 'push origin refs/tags/v0.1.0' "$RELEASE_TEST_LOG"

bash "$ROOT/release.sh" 0.1.1 --target=linux-x64 --dry-run >"$TEMP/output"
grep -q 'Targets: linux-x64' "$TEMP/output"
bash "$ROOT/release.sh" 0.1.1 --targets=linux-x64,macos-arm64 --dry-run >"$TEMP/output"
grep -q 'Targets: linux-x64,macos-arm64' "$TEMP/output"
[[ "$(bash "$ROOT/install/scripts/release-targets.sh" linux-x64)" == '{"include":[{"os":"ubuntu-24.04","platform":"linux-x64","target":"x86_64-unknown-linux-musl"}]}' ]]

[[ "$(bash "$ROOT/install/scripts/get-next-version.sh" patch)" == '0.1.5' ]]
[[ "$(RELEASE_TEST_HTTP=404 bash "$ROOT/install/scripts/get-next-version.sh" patch)" == '0.1.0' ]]
expect_failure env RELEASE_TEST_HTTP=403 bash "$ROOT/install/scripts/get-next-version.sh" patch
expect_failure env RELEASE_TEST_BODY='{"tag_name":"invalid"}' bash "$ROOT/install/scripts/get-next-version.sh" patch
printf 'Binary release entry-point and version-selection tests passed.\n'
