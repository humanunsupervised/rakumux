# shellcheck shell=bash

# The binaries the workspace builds: `tui` (the orchestra program),
# `orchestra-ptyd` (the session daemon), `orchestra-api` (the external HTTP
# API, ../api) and `orchestra-mcp` (the MCP server for agents, ../mcp).
ORCHESTRA_BINARIES=(tui orchestra-ptyd orchestra-api orchestra-mcp)

# Build every workspace binary. A plain `cargo build` in tui/ would only build
# the root `tui` package.
rust_tui() {
  local build_type="${1:-release}"
  local profile_dir="debug"
  local cargo_args=(build --workspace)

  if [[ "$build_type" == "release" ]]; then
    cargo_args+=(--release)
    profile_dir="release"
  fi

  info "Building ${ORCHESTRA_BINARIES[*]} (${build_type})..."
  ( cd "$SCRIPT_DIR/tui" && cargo "${cargo_args[@]}" )

  local binary
  for binary in "${ORCHESTRA_BINARIES[@]}"; do
    if [[ ! -x "$SCRIPT_DIR/tui/target/$profile_dir/$binary" ]]; then
      error "Build finished but $binary is missing: ./tui/target/$profile_dir/$binary"
      exit 1
    fi
  done
  success "Built ${ORCHESTRA_BINARIES[*]} in ./tui/target/$profile_dir/"
}

rust_test() {
  info "Running workspace tests..."
  ( cd "$SCRIPT_DIR/tui" && cargo test --workspace )
  success "Workspace tests passed"
}
