# shellcheck shell=bash

# Load the launcher modules used by rakumux.sh. Every module is self-contained:
# no git, tmux, jq or services layer is needed to launch Rakumux.
gwr_load_core() {
    source "$SCRIPT_DIR/shell/gwr_colors.sh"
    source "$SCRIPT_DIR/shell/gwr_logging.sh"
    source "$SCRIPT_DIR/shell/gwr_binary.sh"
    source "$SCRIPT_DIR/shell/gwr_terminal.sh"
    source "$SCRIPT_DIR/shell/gwr_usage.sh"
}
