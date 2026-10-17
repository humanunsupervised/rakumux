# shellcheck shell=bash

# Install `tui`, `rakumux-ptyd`, `rakumux-api` and `rakumux-mcp` into
# ~/.cargo/bin. rakumux.sh finds the daemon next to the tui binary, and
# rakumux-mcp and rakumux-api find each other and the daemon the same way,
# so all four must land in the same directory.
install_global() {
  info "Installing tui, rakumux-ptyd, rakumux-api and rakumux-mcp with cargo install..."
  (
    cd "$SCRIPT_DIR/tui" &&
      cargo install --locked --path . &&
      cargo install --locked --path crates/ptyd &&
      cargo install --locked --path ../api &&
      cargo install --locked --path ../mcp
  )
  success "Binaries installed (cargo install root, usually ~/.cargo/bin)"
}
