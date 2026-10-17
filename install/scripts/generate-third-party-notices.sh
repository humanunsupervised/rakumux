#!/bin/bash

###############################################################################
# generate-third-party-notices.sh - Build THIRD_PARTY_NOTICES from Cargo.lock
#
# Lists the license of every crate linked into the shipped binaries (tui,
# orchestra-ptyd, orchestra-api, orchestra-mcp) for all release targets, with
# each license text in full, using cargo-about and the config/template in
# install/licenses/.
#
# Fails when a crate's license is not in install/licenses/about.toml's accepted
# list or can't be determined.
#
# Usage: generate-third-party-notices.sh [OUTPUT]
#        (default OUTPUT: release/THIRD_PARTY_NOTICES)
# Requires: cargo-about (cargo install cargo-about --locked --features cli)
###############################################################################

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUTPUT="${1:-$PROJECT_ROOT/release/THIRD_PARTY_NOTICES}"
CONFIG="$PROJECT_ROOT/install/licenses/about.toml"
TEMPLATE="$PROJECT_ROOT/install/licenses/THIRD_PARTY_NOTICES.hbs"
MANIFEST="$PROJECT_ROOT/tui/Cargo.toml"

# Workspace crates without `publish = false` (cargo-about's private filter misses
# them). They're Orchestra's own code, so a missing license is expected.
OWN_CRATES_PATTERN="^(tui)$"

# Crates that must appear: one dependency of each shipped binary, so a change
# that drops a binary from the scan fails loudly.
# orchestra-api links only crates the others link too (hyper, tokio, serde).
REQUIRED_CRATES=("ratatui" "portable-pty" "rmcp")

error() { printf '\033[0;31m[NOTICES]\033[0m %s\n' "$*" >&2; }
info() { printf '\033[0;34m[NOTICES]\033[0m %s\n' "$*" >&2; }

if ! cargo about --version >/dev/null 2>&1; then
    error "cargo-about is not installed."
    error "Install it with: cargo install cargo-about --locked --features cli"
    exit 1
fi

mkdir -p "$(dirname "$OUTPUT")"
log_file="$(mktemp)"
trap 'rm -f "$log_file"' EXIT

info "Generating $OUTPUT from tui/Cargo.lock..."
if ! cargo about generate --workspace --locked \
    --manifest-path "$MANIFEST" \
    --config "$CONFIG" \
    --output-file "$OUTPUT" \
    "$TEMPLATE" 2>"$log_file"; then
    cat "$log_file" >&2
    error "cargo about generate failed (see above)."
    exit 1
fi

# cargo-about only warns when it can't determine a license. For a third-party
# crate that would silently drop its notice, so treat it as an error.
unresolved=()
while IFS= read -r crate; do
    [[ -z "$crate" ]] && continue
    if [[ ! "$crate" =~ $OWN_CRATES_PATTERN ]]; then
        unresolved+=("$crate")
    fi
done < <(grep "unable to synthesize license expression for '" "$log_file" |
    sed "s/.*license expression for '\([^ ']*\) .*/\1/" | sort -u)

if [[ ${#unresolved[@]} -gt 0 ]]; then
    cat "$log_file" >&2
    error "License could not be determined for: ${unresolved[*]}"
    error "Add a [<crate>.clarify] entry to install/licenses/about.toml (see cargo-about docs)."
    exit 1
fi

for crate in "${REQUIRED_CRATES[@]}"; do
    if ! grep -q -- "- $crate " "$OUTPUT"; then
        error "$crate is missing from $OUTPUT; the scan no longer covers every shipped binary."
        exit 1
    fi
done

component_count="$(grep -c '^  - ' "$OUTPUT" || true)"
info "Wrote $OUTPUT ($component_count component entries)"
