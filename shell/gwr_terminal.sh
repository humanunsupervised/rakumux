# shellcheck shell=bash

# Scan the global options that precede the subcommand.
# Sets GWR_SUBCOMMAND (first positional argument, empty when the TUI starts)
# and GWR_HAS_PTYD_PATH (true when the caller passed --ptyd-path).
gwr_scan_global_args() {
    GWR_SUBCOMMAND=""
    GWR_HAS_PTYD_PATH=false

    while [[ $# -gt 0 ]]; do
        case "$1" in
            --ptyd-path)
                GWR_HAS_PTYD_PATH=true
                shift 2 || return 0
                ;;
            --ptyd-path=*)
                GWR_HAS_PTYD_PATH=true
                shift
                ;;
            --services-path)
                shift 2 || return 0
                ;;
            --)
                return 0
                ;;
            -*)
                shift
                ;;
            *)
                GWR_SUBCOMMAND="$1"
                return 0
                ;;
        esac
    done
}

# Window title for the launch folder: "orchestra: /<folder name>".
gwr_terminal_title_text() {
    local folder
    folder="$(basename "$(pwd -P)")"
    folder="${folder// /-}"
    if [[ -z "$folder" || "$folder" == "/" ]]; then
        folder="root"
    fi
    printf 'orchestra: /%s' "$folder"
}

# Set the terminal window title when a writable controlling terminal exists.
gwr_set_terminal_title() {
    local tty_device
    tty_device="$(tty 2>/dev/null || true)"
    if [[ -z "$tty_device" || ! -w "$tty_device" ]]; then
        return 0
    fi

    printf '\033]0;%s\007' "$(gwr_terminal_title_text)" >"$tty_device" 2>/dev/null || true
}
