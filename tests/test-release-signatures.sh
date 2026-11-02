#!/bin/bash
# Release signatures, offline: install.sh's checks (with minisign, with
# OpenSSL 3, and with neither), release-keys.sh, and sign-release.sh against
# a fake gh. Throwaway keys made here; never the network, GitHub or a real
# install. Needs minisign to make the signatures (skipped without it, except
# in CI).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if ! command -v minisign >/dev/null 2>&1; then
    if [[ -n "${CI:-}" ]]; then
        echo 'minisign is needed to test release signatures' >&2
        exit 1
    fi
    echo 'Release signature tests skipped: minisign is not installed.'
    exit 0
fi
TEMP="$(mktemp -d)"
trap 'rm -rf "$TEMP"' EXIT
unset RAKUMUX_SESSION_ID RAKUMUX_SESSION_TOKEN RAKUMUX_SOCKET RAKUMUX_VERSION GH_TOKEN GITHUB_TOKEN
export TMPDIR="$TEMP"
KEYS_SCRIPT="$ROOT/install/scripts/release-keys.sh"
INSTALLER="$ROOT/landing/public/install.sh"

fail() {
    printf 'Release signature test failed: %s\n' "$*" >&2
    exit 1
}
expect_failure() {
    local pattern="$1"
    shift
    if "$@" >"$TEMP/output" 2>&1; then
        cat "$TEMP/output" >&2
        fail "unexpected success: $*"
    fi
    grep -q -- "$pattern" "$TEMP/output" || {
        cat "$TEMP/output" >&2
        fail "expected \"$pattern\" from: $*"
    }
}
sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d' ' -f1
    else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# The installer's copy of the check is release-keys.sh's, and its keys are
# install/release-keys.pub's.
block() { sed -n '/^# BEGIN release signature check/,/^# END release signature check/p' "$1"; }
[[ -n "$(block "$KEYS_SCRIPT")" ]] || fail "release-keys.sh has no signature check block"
cmp -s <(block "$KEYS_SCRIPT") <(block "$INSTALLER") ||
    fail "install.sh's signature check differs from release-keys.sh's"
expected_keys="$(RAKUMUX_RELEASE_KEYS= bash "$KEYS_SCRIPT" list | tr '\n' ' ' | sed 's/ *$//')"
grep -qx "RELEASE_KEYS=\"$expected_keys\"" "$INSTALLER" ||
    fail "install.sh's RELEASE_KEYS aren't install/release-keys.pub's; run install/scripts/release-keys.sh sync"

# Throwaway keys: the main and the backup are the release keys, the stranger isn't.
mkdir "$TEMP/keys"
for name in main backup stranger; do
    minisign -G -W -f -p "$TEMP/keys/$name.pub" -s "$TEMP/keys/$name.key" >/dev/null
done
public() { tail -1 "$TEMP/keys/$1.pub"; }
printf '# test keys\n%s main\n%s backup\n' "$(public main)" "$(public backup)" >"$TEMP/keys/release-keys.pub"
export RAKUMUX_RELEASE_KEYS="$TEMP/keys/release-keys.pub"
: >"$TEMP/keys/none.pub"
bash "$KEYS_SCRIPT" check >/dev/null
printf '%s\n%s\n' "$(public main)" "$(public main)" >"$TEMP/keys/twice.pub"
expect_failure 'twice' env RAKUMUX_RELEASE_KEYS="$TEMP/keys/twice.pub" bash "$KEYS_SCRIPT" check
expect_failure 'no release keys yet' env RAKUMUX_RELEASE_KEYS="$TEMP/keys/none.pub" bash "$KEYS_SCRIPT" check

# sign_with KEY FILE TAG [COMMENT]: FILE.minisig as sign-release.sh makes it.
sign_with() {
    local comment="${4:-rakumux-release tag:$3 file:checksums.txt timestamp:1}"
    minisign -S -s "$TEMP/keys/$1.key" -m "$2" -x "$2.minisig" -t "$comment" >/dev/null
}

# Fixture releases, as GitHub serves them. The package's own install.sh
# records which release it is.
FIXTURE="$TEMP/releases"
# A plain case: macOS's bash 3.2 can't parse one inside $( ).
case "$(uname -s)-$(uname -m)" in
    Darwin-arm64) platform=macos-arm64 ;;
    Darwin-x86_64) platform=macos-intel ;;
    *) platform=linux-x64 ;;
esac
package() {
    local tag="$1" dir="$FIXTURE/$1"
    mkdir -p "$dir/build/rakumux-$platform"
    printf '#!/bin/bash\nmkdir -p "$PREFIX"\necho %s > "$PREFIX/installed"\n' "$tag" >"$dir/build/rakumux-$platform/install.sh"
    chmod +x "$dir/build/rakumux-$platform/install.sh"
    tar -czf "$dir/rakumux-$platform.tar.gz" -C "$dir/build" "rakumux-$platform"
    rm -rf "$dir/build"
    printf '%s  %s\n' "$(sha256_of "$dir/rakumux-$platform.tar.gz")" "rakumux-$platform.tar.gz" >"$dir/checksums.txt"
}
package v0.1.51 && sign_with main "$FIXTURE/v0.1.51/checksums.txt" v0.1.51
package v0.1.52 && sign_with backup "$FIXTURE/v0.1.52/checksums.txt" v0.1.52
package v0.1.53 # unsigned
# v0.1.51's packages and signature, moved onto another release.
mkdir "$FIXTURE/v0.1.54" && cp "$FIXTURE/v0.1.51/"* "$FIXTURE/v0.1.54/"
# Another package, and its checksum, under v0.1.51's signature.
package v0.1.55 && cp "$FIXTURE/v0.1.51/checksums.txt.minisig" "$FIXTURE/v0.1.55/"
package v0.1.56 && sign_with stranger "$FIXTURE/v0.1.56/checksums.txt" v0.1.56
package v0.1.57 && sign_with main "$FIXTURE/v0.1.57/checksums.txt" v0.1.57 'untrusted text'
package v0.1.3 # released before signing
LATEST=v0.1.52

# A fake curl: GitHub's latest-release redirect and release downloads.
mkdir "$TEMP/fake"
cat >"$TEMP/fake/curl" <<EOF
#!/bin/bash
out="" format="" url=""
while [[ \$# -gt 0 ]]; do
    case "\$1" in
        -o) out="\$2"; shift 2 ;;
        -w) format="\$2"; shift 2 ;;
        -*) shift ;;
        *) url="\$1"; shift ;;
    esac
done
case "\$url" in
    https://github.com/humanunsupervised/rakumux/releases/latest)
        printf 'https://github.com/humanunsupervised/rakumux/releases/tag/$LATEST' ;;
    https://github.com/humanunsupervised/rakumux/releases/download/*)
        file="$FIXTURE/\${url#https://github.com/humanunsupervised/rakumux/releases/download/}"
        [[ -f "\$file" ]] || exit 22
        cp "\$file" "\$out" ;;
    *) exit 6 ;;
esac
EOF
chmod +x "$TEMP/fake/curl"

# The installer with the test keys in place of the real ones.
keys_line="RELEASE_KEYS=\"$(public main) $(public backup)\""
awk -v line="$keys_line" '/^RELEASE_KEYS="/ { print line; next } { print }' "$INSTALLER" >"$TEMP/install.sh"
awk '/^RELEASE_KEYS="/ { print "RELEASE_KEYS=\"\""; next } { print }' "$INSTALLER" >"$TEMP/install-no-keys.sh"

# A PATH holding only what the installer runs, with or without minisign and openssl.
toolbox() {
    local dir="$TEMP/path-$1" tool path
    shift
    mkdir -p "$dir"
    ln -sf "$TEMP/fake/curl" "$dir/curl"
    for tool in bash env uname tar gzip awk sed mktemp head tail wc tr od cat rm cut grep mkdir \
        shasum sha256sum chmod dirname cp "$@"; do
        path="$(command -v "$tool" 2>/dev/null)" || continue
        ln -sf "$path" "$dir/$tool"
    done
    echo "$dir"
}
block "$KEYS_SCRIPT" >"$TEMP/check.sh"
openssl_works() {
    command -v openssl >/dev/null 2>&1 || return 1
    # shellcheck source=/dev/null
    (source "$TEMP/check.sh" && rakumux_openssl_works)
}
modes="minisign none"
if openssl_works; then
    modes="minisign openssl none"
else
    echo "openssl here can't check Ed25519 signatures ($(openssl version 2>/dev/null || echo 'not installed')); its path isn't tested"
fi

# install MODE VERSION [SCRIPT]: run the installer as curl | bash would.
install() {
    local mode="$1" version="$2" script="${3:-$TEMP/install.sh}" path prefix="$TEMP/prefix"
    rm -rf "$prefix"
    case "$mode" in
        minisign) path="$(toolbox minisign minisign)" ;;
        openssl) path="$(toolbox openssl openssl)" ;;
        none) path="$(toolbox none)" ;;
    esac
    PATH="$path" PREFIX="$prefix" RAKUMUX_VERSION="$version" bash <"$script"
}
installed() { [[ "$(cat "$TEMP/prefix/installed" 2>/dev/null)" == "$1" ]]; }

for mode in $modes; do
    install "$mode" "" >"$TEMP/output" 2>&1 || { cat "$TEMP/output" >&2; fail "$mode: latest"; }
    installed "$LATEST" || fail "$mode: the latest release ($LATEST) wasn't installed"
    install "$mode" 0.1.51 >"$TEMP/output" 2>&1 || { cat "$TEMP/output" >&2; fail "$mode: v0.1.51"; }
    installed v0.1.51 || fail "$mode: v0.1.51 wasn't installed"
    if [[ "$mode" == none ]]; then
        grep -q "signature wasn't checked: install minisign" "$TEMP/output" || fail "none: no notice"
    else
        grep -q 'Signature verified: v0.1.51' "$TEMP/output" || fail "$mode: not verified"
    fi
    # A release since signing began is refused without a signature, however it's installed.
    expect_failure 'has no signature' install "$mode" 0.1.53
    [[ ! -e "$TEMP/prefix/installed" ]] || fail "$mode: an unsigned release was installed"
    # One made before is installed, saying so.
    install "$mode" 0.1.3 >"$TEMP/output" 2>&1 || { cat "$TEMP/output" >&2; fail "$mode: v0.1.3"; }
    installed v0.1.3 && grep -q 'released before releases were signed' "$TEMP/output" ||
        fail "$mode: v0.1.3"
    if [[ "$mode" != none ]]; then
        for version in 0.1.54 0.1.55 0.1.56 0.1.57; do
            expect_failure "isn't a valid Rakumux release signature" install "$mode" "$version"
            [[ ! -e "$TEMP/prefix/installed" ]] || fail "$mode: v$version was installed"
        done
    fi
done
# An old unsigned release made the latest one again (as a compromised GitHub
# account could) isn't installed: only one asked for by version is.
LATEST=v0.1.3
sed -i.bak 's#releases/tag/v0.1.52#releases/tag/v0.1.3#' "$TEMP/fake/curl"
for mode in $modes; do
    expect_failure 'has no signature' install "$mode" ""
    [[ ! -e "$TEMP/prefix/installed" ]] || fail "$mode: an unsigned latest release was installed"
done
# An installer without keys (before the maintainer adds them) says so.
install none 0.1.51 "$TEMP/install-no-keys.sh" >"$TEMP/output" 2>&1
installed v0.1.51 && grep -q 'this installer has no release keys' "$TEMP/output" || fail "no keys"

# release-keys.sh verify, with each tool.
for mode in $modes; do
    [[ "$mode" == none ]] && continue
    run() { PATH="$(toolbox "keys-$mode" "$mode")" bash "$KEYS_SCRIPT" verify "$@"; }
    run "$FIXTURE/v0.1.51/checksums.txt" "$FIXTURE/v0.1.51/checksums.txt.minisig" v0.1.51 >/dev/null
    run "$FIXTURE/v0.1.52/checksums.txt" "$FIXTURE/v0.1.52/checksums.txt.minisig" v0.1.52 >/dev/null
    expect_failure 'not a valid signature' run "$FIXTURE/v0.1.54/checksums.txt" "$FIXTURE/v0.1.54/checksums.txt.minisig" v0.1.54
    expect_failure 'not a valid signature' run "$FIXTURE/v0.1.56/checksums.txt" "$FIXTURE/v0.1.56/checksums.txt.minisig" v0.1.56
done

# sign-release.sh against a fake gh: GH holds each release's state and assets.
GH="$TEMP/gh"
mkdir -p "$GH"
cat >"$TEMP/fake/gh" <<'EOF'
#!/bin/bash
set -eu
printf '%s\n' "$*" >>"$GH/log"
repo="" dir="" pattern="" jq="" args=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --repo) repo="$2"; shift 2 ;;
        --dir) dir="$2"; shift 2 ;;
        --pattern) pattern="$2"; shift 2 ;;
        --jq) jq="$2"; shift 2 ;;
        --json) shift 2 ;;
        *) args+=("$1"); shift ;;
    esac
done
[[ "${args[0]}" == api || "$repo" == humanunsupervised/rakumux ]] || exit 3
case "${args[0]} ${args[1]}" in
    "release view")
        release="$GH/${args[2]}"
        [[ -d "$release" ]] || exit 1
        case "$jq" in
            .isDraft) cat "$release/draft" ;;
            '.assets[].name') ls "$release/assets" ;;
        esac ;;
    "release download")
        mkdir -p "$dir"
        for file in "$GH/${args[2]}/assets/"*; do
            if [[ -z "$pattern" || "${file##*/}" == "$pattern" ]]; then cp "$file" "$dir/"; fi
        done ;;
    "release upload") cp "${args[3]}" "$GH/${args[2]}/assets/" ;;
    "release edit")
        printf '%s\n' "${args[*]:3}" >"$GH/${args[2]}/edited"
        case " ${args[*]} " in *" --draft=false "*) echo false >"$GH/${args[2]}/draft" ;; esac ;;
    api*)
        case "${args[1]}" in
            repos/humanunsupervised/rakumux/git/ref/tags/*)
                tag="${args[1]##*/}"
                [[ -f "$GH/$tag/mode" ]] || exit 1
                echo "tag object-$tag" ;;
            repos/humanunsupervised/rakumux/git/tags/object-*)
                tag="${args[1]##*/object-}"
                case "$jq" in
                    .message) printf 'Rakumux %s\n\nRelease-Mode: %s\n\nRelease-Targets: linux-x64\n' "$tag" "$(cat "$GH/$tag/mode")" ;;
                    .object.sha) echo 0123456789abcdef0123456789abcdef01234567 ;;
                esac ;;
            *) exit 4 ;;
        esac ;;
    *) exit 5 ;;
esac
EOF
chmod +x "$TEMP/fake/gh"
export GH
# draft TAG MODE: a draft as the workflow leaves it.
draft() {
    rm -rf "${GH:?}/$1"
    mkdir -p "$GH/$1/assets"
    echo true >"$GH/$1/draft"
    echo "$2" >"$GH/$1/mode"
    package "$1-src" >/dev/null
    cp "$FIXTURE/$1-src/"* "$GH/$1/assets/"
}
sign() { PATH="$TEMP/fake:$PATH" bash "$ROOT/install/scripts/sign-release.sh" "$@"; }
uploaded() { [[ -f "$GH/$1/assets/checksums.txt.minisig" ]]; }

draft v0.2.0 stable
sign v0.2.0 --key "$TEMP/keys/main.key" --yes >"$TEMP/output"
uploaded v0.2.0 || fail "no signature uploaded"
bash "$KEYS_SCRIPT" verify "$GH/v0.2.0/assets/checksums.txt" "$GH/v0.2.0/assets/checksums.txt.minisig" v0.2.0 >/dev/null
grep -qx -- '--draft=false --latest' "$GH/v0.2.0/edited" || fail "not published as the latest release"
grep -q 'Published v0.2.0' "$TEMP/output"
# The signed release installs.
mkdir "$FIXTURE/v0.2.0" && cp "$GH/v0.2.0/assets/"* "$FIXTURE/v0.2.0/"
install minisign 0.2.0 >/dev/null 2>&1 && installed v0.2.0-src || fail "the signed release didn't install"

draft v0.2.1-rc.1 prerelease
sign 0.2.1-rc.1 --key "$TEMP/keys/backup.key" --yes >/dev/null
grep -qx -- '--draft=false --prerelease --latest=false' "$GH/v0.2.1-rc.1/edited" || fail "not a prerelease"
draft v0.2.2 draft
sign v0.2.2 --key "$TEMP/keys/main.key" --yes >"$TEMP/output"
uploaded v0.2.2 && [[ ! -e "$GH/v0.2.2/edited" ]] || fail "a --draft release was published"

# Refused, with nothing uploaded or published.
draft v0.2.3 stable
expect_failure "isn't one of install/release-keys.pub" sign v0.2.3 --key "$TEMP/keys/stranger.key" --yes
expect_failure 'already published' sign v0.2.0 --key "$TEMP/keys/main.key" --yes
echo false >"$GH/v0.2.3/draft"
expect_failure 'already published' sign v0.2.3 --key "$TEMP/keys/main.key" --yes
draft v0.2.3 stable
printf 'tampered' >>"$GH/v0.2.3/assets/rakumux-$platform.tar.gz"
expect_failure "doesn't match checksums.txt" sign v0.2.3 --key "$TEMP/keys/main.key" --yes
draft v0.2.3 stable
printf 'extra' >"$GH/v0.2.3/assets/rakumux-other.tar.gz"
expect_failure "doesn't list" sign v0.2.3 --key "$TEMP/keys/main.key" --yes
draft v0.2.3 stable
echo '' >"$GH/v0.2.3/mode"
expect_failure 'no Release-Mode' sign v0.2.3 --key "$TEMP/keys/main.key" --yes
draft v0.2.3 stable
expect_failure 'no secret key' sign v0.2.3 --key "$TEMP/keys/missing.key" --yes
expect_failure 'no release v9.9.9' sign v9.9.9 --key "$TEMP/keys/main.key" --yes
expect_failure 'not a release tag' sign latest --yes
expect_failure 'no release keys yet' env RAKUMUX_RELEASE_KEYS="$TEMP/keys/none.pub" \
    PATH="$TEMP/fake:$PATH" bash "$ROOT/install/scripts/sign-release.sh" v0.2.3 --key "$TEMP/keys/main.key" --yes
uploaded v0.2.3 && fail "a refused release was signed"
[[ ! -e "$GH/v0.2.3/edited" ]] || fail "a refused release was published"

echo "Release signature tests passed ($modes)."
