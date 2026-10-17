# shellcheck shell=bash

load() {
  source "$SCRIPT_DIR/shell/build/logging.sh"
  source "$SCRIPT_DIR/shell/build/dependencies.sh"
  source "$SCRIPT_DIR/shell/build/rust.sh"
  source "$SCRIPT_DIR/shell/build/install.sh"
  source "$SCRIPT_DIR/shell/build/usage.sh"
}
