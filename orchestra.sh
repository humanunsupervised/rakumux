#!/bin/bash

###############################################################################
# orchestra.sh - Orchestra launcher
# ---------------------------------------------------------------------------
# Every channel runs this script for every argument: npm (bin/orchestra.js),
# Homebrew (bin/orchestra), the release install.sh and source checkouts.
#
# It resolves the `tui` binary (the orchestra program) and the orchestra-ptyd
# session daemon, exports ORCHESTRA_BIN (this launcher, for agents and hooks)
# and execs the binary with `--ptyd-path` followed by the caller's arguments.
# With no subcommand the binary opens the TUI; otherwise it runs the CLI.
#
# Overrides: GW_TUI_BIN (tui binary), GW_PTYD_BIN (daemon binary).
###############################################################################

set -o errexit
set -o pipefail
set -o nounset

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$SCRIPT_DIR/shell/gwr_load.sh"
gwr_load_core

case "${1:-}" in
    -h | --help)
        gwr_show_usage
        exit 0
        ;;
    --check-updates | --update)
        current_version=""
        if binary_path=$(gwr_find_binary); then
            current_version="$(gwr_binary_version "$binary_path" || true)"
        fi
        export ORCHESTRA_CURRENT_VERSION="$current_version"
        exec "$SCRIPT_DIR/shell/gwr/check-updates.sh"
        ;;
esac

if ! BINARY_PATH=$(gwr_find_binary); then
    gwr_error "orchestra binary (tui) not found."
    gwr_error ""
    gwr_error "Reinstall Orchestra (npm or Homebrew), or in a source checkout run: ./build.sh"
    gwr_error ""
    gwr_error "Search paths:"
    if [[ -n "${GW_TUI_BIN:-}" ]]; then
        gwr_error "  • $GW_TUI_BIN (from GW_TUI_BIN, not executable)"
    fi
    gwr_error "  • $SCRIPT_DIR/orchestra"
    gwr_error "  • $SCRIPT_DIR/tui/target/release/tui"
    gwr_error "  • $SCRIPT_DIR/tui/target/debug/tui"
    gwr_error "  • tui (in PATH)"
    exit 1
fi

if [[ "${1:-}" == "--version" ]]; then
    exec "$BINARY_PATH" --version
fi

gwr_scan_global_args "$@"

PTYD_ARGS=()
if [[ "$GWR_HAS_PTYD_PATH" != "true" ]]; then
    if ! PTYD_PATH=$(gwr_find_ptyd_binary "$BINARY_PATH"); then
        gwr_error "orchestra-ptyd (session daemon) not found."
        gwr_error ""
        gwr_error "Reinstall Orchestra (npm or Homebrew), or in a source checkout run: ./build.sh"
        gwr_error ""
        gwr_error "Search paths:"
        if [[ -n "${GW_PTYD_BIN:-}" ]]; then
            gwr_error "  • $GW_PTYD_BIN (from GW_PTYD_BIN, not executable)"
        fi
        gwr_error "  • $(dirname "$BINARY_PATH")/orchestra-ptyd (next to $BINARY_PATH)"
        gwr_error "  • $SCRIPT_DIR/orchestra-ptyd"
        gwr_error "  • orchestra-ptyd (in PATH)"
        exit 1
    fi
    PTYD_ARGS=(--ptyd-path "$PTYD_PATH")
fi

export ORCHESTRA_BIN="$SCRIPT_DIR/$(basename "${BASH_SOURCE[0]}")"

# Terminal title and the daily update check only for the TUI; subcommands (often
# run by agents and hooks through $ORCHESTRA_BIN) stay quiet and fast.
if [[ -z "$GWR_SUBCOMMAND" ]]; then
    gwr_set_terminal_title
    gwr_check_updates_silent </dev/null >/dev/null 2>&1 &
fi

# ${arr[@]+...}: an empty array is "unbound" under nounset in bash 3.2 (macOS /bin/bash).
exec "$BINARY_PATH" ${PTYD_ARGS[@]+"${PTYD_ARGS[@]}"} "$@"
