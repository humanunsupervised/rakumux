#!/bin/bash
# release-scripts/sign-draft.sh, offline, against a fake gh and throwaway keys;
# never the network or GitHub. Needs minisign to make the signatures (skipped
# without it, except in CI).
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SIGN="$ROOT/release-scripts/sign-draft.sh"
KEYS_SCRIPT="$ROOT/release-scripts/release-keys.sh"
if ! command -v minisign >/dev/null 2>&1; then
    if [[ -n "${CI:-}" ]]; then
        echo 'minisign is needed to test signing drafts' >&2
        exit 1
    fi
    echo 'sign-draft tests skipped: minisign is not installed.'
    exit 0
fi
TEMP="$(mktemp -d)"
trap 'rm -rf "$TEMP"' EXIT
unset RAKUMUX_SESSION_ID RAKUMUX_SESSION_TOKEN RAKUMUX_SOCKET GH_TOKEN GITHUB_TOKEN GITHUB_OUTPUT
export GITHUB_REPOSITORY=humanunsupervised/rakumux
mkdir "$TEMP/tmp"
export TMPDIR="$TEMP/tmp"

fail() {
    printf 'sign-draft test failed: %s\n' "$*" >&2
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

# Throwaway keys: `ci` is listed, the backup too, the stranger isn't.
mkdir "$TEMP/keys"
for name in ci backup stranger; do
    minisign -G -W -f -p "$TEMP/keys/$name.pub" -s "$TEMP/keys/$name.key" >/dev/null
done
public() { tail -1 "$TEMP/keys/$1.pub"; }
printf '# test keys\n%s backup\n%s ci # the CI key\n' "$(public backup)" "$(public ci)" >"$TEMP/keys/release-keys.pub"
printf '%s backup\n' "$(public backup)" >"$TEMP/keys/before-ci.pub"
export RAKUMUX_RELEASE_KEYS="$TEMP/keys/release-keys.pub"

# `named` finds a key by its name, nothing for one not listed.
[[ "$("$KEYS_SCRIPT" named ci)" == "$(public ci)" ]] || fail "named ci"
[[ "$("$KEYS_SCRIPT" named backup)" == "$(public backup)" ]] || fail "named backup"
[[ -z "$("$KEYS_SCRIPT" named nobody)" ]] || fail "named nobody"
[[ -z "$(RAKUMUX_RELEASE_KEYS="$TEMP/keys/before-ci.pub" "$KEYS_SCRIPT" named ci)" ]] || fail "named ci before it was added"

# A fake gh: GH/TAG holds each release's state and assets.
GH="$TEMP/gh"
mkdir -p "$GH" "$TEMP/fake"
export GH
cat >"$TEMP/fake/gh" <<'EOF'
#!/bin/bash
set -eu
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
        release="$GH/${args[2]}"
        mkdir -p "$dir"
        for file in "$release/assets/"*; do
            if [[ -z "$pattern" || "${file##*/}" == "$pattern" ]]; then cp "$file" "$dir/"; fi
        done
        # The draft's checksums.txt changing after the first read.
        if [[ -f "$release/change-after-read" ]]; then
            echo tampered >>"$release/assets/checksums.txt"
            rm "$release/change-after-read"
        fi ;;
    "release upload") cp "${args[3]}" "$GH/${args[2]}/assets/" ;;
    "release edit")
        printf '%s\n' "${args[*]:3}" >"$GH/${args[2]}/edited"
        case " ${args[*]} " in *" --draft=false "*) echo false >"$GH/${args[2]}/draft" ;; esac ;;
    api*)
        case "${args[1]}" in
            repos/humanunsupervised/rakumux/git/ref/tags/*)
                tag="${args[1]##*/}"
                [[ -f "$GH/$tag/mode" ]] || exit 1
                if [[ -f "$GH/$tag/lightweight" ]]; then echo "commit abc"; else echo "tag object-$tag"; fi ;;
            repos/humanunsupervised/rakumux/git/tags/object-*)
                tag="${args[1]##*/object-}"
                mode="$(cat "$GH/$tag/mode")"
                if [[ -n "$mode" ]]; then
                    printf 'Rakumux %s\n\nRelease-Mode: %s\n' "$tag" "$mode"
                else
                    printf 'Rakumux %s\n' "$tag"
                fi ;;
            *) exit 4 ;;
        esac ;;
    *) exit 5 ;;
esac
EOF
chmod +x "$TEMP/fake/gh"
export PATH="$TEMP/fake:$PATH"

# A draft release of TAG in MODE: one package and its checksums.
draft() {
    local tag="$1" mode="$2" dir="$GH/$1"
    mkdir -p "$dir/assets"
    echo true >"$dir/draft"
    printf '%s' "$mode" >"$dir/mode"
    printf 'package of %s\n' "$tag" >"$dir/assets/rakumux-linux-x64.tar.gz"
    printf '%s  rakumux-linux-x64.tar.gz\n' "$(sha256_of "$dir/assets/rakumux-linux-x64.tar.gz")" >"$dir/assets/checksums.txt"
}
key() { cat "$TEMP/keys/$1.key"; }
signed() { [[ -f "$GH/$1/assets/checksums.txt.minisig" ]]; }

# A stable release: signed by the CI key, the signature checks out, published
# as the latest, and the workflow's outputs say so.
draft v1.0.0 stable
export GITHUB_OUTPUT="$TEMP/outputs"
: >"$GITHUB_OUTPUT"
RAKUMUX_SIGNING_KEY="$(key ci)" "$SIGN" v1.0.0 >"$TEMP/output"
unset GITHUB_OUTPUT
signed v1.0.0 || fail "no signature uploaded"
"$KEYS_SCRIPT" verify "$GH/v1.0.0/assets/checksums.txt" "$GH/v1.0.0/assets/checksums.txt.minisig" v1.0.0 >/dev/null ||
    fail "the signature doesn't verify for its tag"
expect_failure "not a valid signature" "$KEYS_SCRIPT" verify "$GH/v1.0.0/assets/checksums.txt" "$GH/v1.0.0/assets/checksums.txt.minisig" v9.9.9
[[ "$(cat "$GH/v1.0.0/draft")" == false ]] || fail "not published"
grep -q -- '--draft=false --latest' "$GH/v1.0.0/edited" || fail "not published as the latest: $(cat "$GH/v1.0.0/edited")"
grep -qx 'published=true' "$TEMP/outputs" || fail "no published output"
grep -qx 'mode=stable' "$TEMP/outputs" || fail "no mode output"
[[ -z "$(ls -A "$TMPDIR")" ]] || fail "left files behind: $(ls -A "$TMPDIR")"

# A prerelease stays off `latest`; a `draft` release is signed and stays a draft.
draft v1.1.0-rc.1 prerelease
RAKUMUX_SIGNING_KEY="$(key ci)" "$SIGN" v1.1.0-rc.1 >/dev/null
grep -q -- '--prerelease --latest=false' "$GH/v1.1.0-rc.1/edited" || fail "prerelease flags"
draft v1.2.0 draft
export GITHUB_OUTPUT="$TEMP/outputs2"
: >"$GITHUB_OUTPUT"
RAKUMUX_SIGNING_KEY="$(key ci)" "$SIGN" v1.2.0 >/dev/null
unset GITHUB_OUTPUT
signed v1.2.0 || fail "a draft-mode release isn't signed"
[[ "$(cat "$GH/v1.2.0/draft")" == true ]] || fail "a draft-mode release was published"
! grep -q 'published=true' "$TEMP/outputs2" || fail "a draft-mode release said published"

# What it refuses, signing nothing.
refused() {
    local tag="$1" pattern="$2"
    shift 2
    expect_failure "$pattern" env RAKUMUX_SIGNING_KEY="$(key ci)" "$SIGN" "$tag" "$@"
    if [[ "$pattern" != "already has" ]]; then
        signed "$tag" && fail "$tag was signed despite: $pattern"
    fi
    [[ "$(cat "$GH/$tag/draft" 2>/dev/null || echo true)" == true || "$pattern" == "already published" ]] ||
        fail "$tag was published despite: $pattern"
}
expect_failure "not a release tag" env RAKUMUX_SIGNING_KEY=x "$SIGN" latest
draft v2.0.0 stable
expect_failure "no signing key" env -u RAKUMUX_SIGNING_KEY "$SIGN" v2.0.0
signed v2.0.0 && fail "signed without a key"
expect_failure "has no release" env RAKUMUX_SIGNING_KEY="$(key ci)" "$SIGN" v2.9.9

draft v2.0.1 stable; echo false >"$GH/v2.0.1/draft"
refused v2.0.1 "already published"
draft v2.0.2 stable; echo sig >"$GH/v2.0.2/assets/checksums.txt.minisig"
refused v2.0.2 "already has"
draft v2.0.3 stable; touch "$GH/v2.0.3/lightweight"
refused v2.0.3 "isn't an annotated"
draft v2.0.4 ""
refused v2.0.4 "no Release-Mode"
draft v2.0.5 stable; echo tampered >"$GH/v2.0.5/assets/rakumux-linux-x64.tar.gz"
refused v2.0.5 "doesn't match checksums"
draft v2.0.6 stable; echo extra >"$GH/v2.0.6/assets/extra.bin"
refused v2.0.6 "doesn't list"
draft v2.0.7 stable; echo "$(printf 'a%.0s' {1..64})  unexpected.bin" >>"$GH/v2.0.7/assets/checksums.txt"
refused v2.0.7 "unexpected file"
draft v2.0.8 stable; touch "$GH/v2.0.8/change-after-read"
refused v2.0.8 "changed while signing"

# A key the release keys don't list: nothing uploaded.
draft v3.0.0 stable
expect_failure "isn't one of" env RAKUMUX_SIGNING_KEY="$(key stranger)" "$SIGN" v3.0.0
signed v3.0.0 && fail "a stranger's signature was uploaded"
[[ "$(cat "$GH/v3.0.0/draft")" == true ]] || fail "a stranger's signature was published"

# --trusted-by: the newest published release must list the key too (the
# release that adds the key can't be signed by it).
draft v3.1.0 stable
expect_failure "doesn't trust" env RAKUMUX_SIGNING_KEY="$(key ci)" "$SIGN" v3.1.0 --trusted-by "$TEMP/keys/before-ci.pub"
signed v3.1.0 && fail "signed with a key the last release doesn't trust"
RAKUMUX_SIGNING_KEY="$(key backup)" "$SIGN" v3.1.0 --trusted-by "$TEMP/keys/before-ci.pub" >/dev/null
signed v3.1.0 || fail "the trusted key didn't sign"
draft v3.2.0 stable
RAKUMUX_SIGNING_KEY="$(key ci)" "$SIGN" v3.2.0 --trusted-by "$TEMP/keys/release-keys.pub" >/dev/null
signed v3.2.0 || fail "a key the last release trusts didn't sign"

[[ -z "$(ls -A "$TMPDIR")" ]] || fail "left files behind: $(ls -A "$TMPDIR")"
echo 'sign-draft tests passed.'
