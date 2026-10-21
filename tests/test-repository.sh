#!/bin/bash
# Source-tree checks only. Never starts a daemon or accesses user session data.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

fail() { printf 'Repository check failed: %s\n' "$*" >&2; exit 1; }

node tests/public-source-boundary.mjs

[[ ! -d mobile ]] || fail "mobile source belongs in the separate repository"
grep -q 'Version 2.0, January 2004' LICENSE || fail "missing Apache-2.0 license"
grep -q 'license = "Apache-2.0"' tui/Cargo.toml || fail "wrong Cargo license"
grep -q '"license": "Apache-2.0"' install/npm/package.json || fail "wrong npm license"
grep -q 'license "Apache-2.0"' install/homebrew/rakumux.rb.template || fail "wrong Homebrew license"

for file in rakumux.sh shell/rakumux-local.sh shell/rakumux-command-hook.sh \
    install/npm/bin/rakumux.js install/npm/bin/rakumux-api.js \
    install/npm/bin/rakumux-mcp.js "integrations/open-code/rakumux-session/runtime.js"; do
    [[ -f "$file" ]] || fail "missing $file"
done

legacy_pattern='Orchestra|ORCHESTRA|orchestra([^a-z]|$)|Orchestrator|ORCHESTRATOR|orchestrator'
if git grep -I -n -E "$legacy_pattern" -- . ':!tests/test-repository.sh'; then
    fail "old project branding remains"
else
    status=$?
    [[ "$status" == 1 ]] || fail "branding search failed with status $status"
fi

while IFS= read -r -d '' file; do
    [[ -f "$file" ]] || continue
    case "$file" in
        *.sh) bash -n "$file" ;;
        install/npm/*.js|install/npm/bin/*.js) node --check "$file" ;;
    esac
done < <(git ls-files --cached --others --exclude-standard -z)

if [[ -f install/npm/LICENSE ]]; then
    cmp -s LICENSE install/npm/LICENSE || fail "staged npm license is stale; restage the package"
fi

printf 'Repository branding, licensing, and script checks passed.\n'
