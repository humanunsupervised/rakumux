# shellcheck shell=bash

# Binary lookup for orchestra.sh. Install layouts:
#   release package / npm dist / Homebrew libexec: orchestra, orchestra-ptyd next to orchestra.sh
#   source checkout: tui/target/{release,debug}/{tui,orchestra-ptyd}

gwr_is_executable_file() {
    [[ -n "$1" && -f "$1" && -x "$1" ]]
}

# Print the path of the `tui` binary (the `orchestra` program).
# Order: $GW_TUI_BIN, next to orchestra.sh, source checkout builds, PATH.
# $GW_TUI_BIN counts only with a session daemon next to it (or $GW_PTYD_BIN
# set): sessions started by Orchestra 0.5.x still export it, pointing at an
# old binary that has no daemon, and it must not win over this install.
gwr_find_binary() {
    if gwr_is_executable_file "${GW_TUI_BIN:-}"; then
        if gwr_is_executable_file "${GW_PTYD_BIN:-}" \
            || gwr_is_executable_file "$(dirname "$GW_TUI_BIN")/orchestra-ptyd"; then
            echo "$GW_TUI_BIN"
            return 0
        fi
    fi

    local candidates=(
        "$SCRIPT_DIR/orchestra"
        "$SCRIPT_DIR/tui/target/release/tui"
        "$SCRIPT_DIR/tui/target/debug/tui"
        "$(command -v tui 2>/dev/null || true)"
    )

    local path
    for path in "${candidates[@]}"; do
        if gwr_is_executable_file "$path"; then
            echo "$path"
            return 0
        fi
    done

    return 1
}

# Print the path of the `orchestra-ptyd` session daemon.
# Order: $GW_PTYD_BIN, next to the tui binary ($1), next to orchestra.sh, PATH.
gwr_find_ptyd_binary() {
    local tui_binary="${1:-}"

    if gwr_is_executable_file "${GW_PTYD_BIN:-}"; then
        echo "$GW_PTYD_BIN"
        return 0
    fi

    local candidates=()
    if [[ -n "$tui_binary" ]]; then
        candidates+=("$(dirname "$tui_binary")/orchestra-ptyd")
    fi
    candidates+=(
        "$SCRIPT_DIR/orchestra-ptyd"
        "$(command -v orchestra-ptyd 2>/dev/null || true)"
    )

    local path
    for path in "${candidates[@]}"; do
        if gwr_is_executable_file "$path"; then
            echo "$path"
            return 0
        fi
    done

    return 1
}

# Print the version reported by the tui binary ($1), e.g. "0.6.0".
gwr_binary_version() {
    local output
    output="$("$1" --version 2>/dev/null)" || return 1
    output="${output##* }"
    [[ -n "$output" ]] || return 1
    printf '%s\n' "$output"
}

gwr_should_check_updates() {
    # Check if we should run update check (once per day)
    local last_check_file="${HOME}/.cache/orchestra/last_update_check"
    local current_time
    current_time=$(date +%s)
    local last_check=0

    if [[ -f "$last_check_file" ]]; then
        last_check=$(cat "$last_check_file" 2>/dev/null || echo 0)
    fi

    # Check if more than 24 hours have passed (86400 seconds)
    local time_diff=$((current_time - last_check))
    if [[ $time_diff -gt 86400 ]]; then
        mkdir -p "$(dirname "$last_check_file")"
        echo "$current_time" >"$last_check_file"
        return 0
    fi

    return 1
}

gwr_check_updates_silent() {
    # Silent update check that doesn't interfere with normal operation
    if gwr_should_check_updates; then
        "$SCRIPT_DIR/shell/gwr/check-updates.sh" 2>/dev/null | grep -E "(⚠️|newer version|Update instructions)" || true
    fi
}
