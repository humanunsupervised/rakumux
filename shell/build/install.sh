# shellcheck shell=bash

# Install `tui`, `orchestra-ptyd`, `orchestra-api` and `orchestra-mcp` into
# ~/.cargo/bin. orchestra.sh finds the daemon next to the tui binary, and
# orchestra-mcp and orchestra-api find each other and the daemon the same way,
# so all four must land in the same directory.
install_global() {
  info "Installing tui, orchestra-ptyd, orchestra-api and orchestra-mcp with cargo install..."
  (
    cd "$SCRIPT_DIR/tui" &&
      cargo install --locked --path . &&
      cargo install --locked --path crates/ptyd &&
      cargo install --locked --path ../api &&
      cargo install --locked --path ../mcp
  )
  success "Binaries installed (cargo install root, usually ~/.cargo/bin)"
}
