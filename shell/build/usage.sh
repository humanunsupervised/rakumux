# shellcheck shell=bash

usage() {
  cat << EOF
Usage: $0 [options]

Build the Orchestra binaries from this source checkout: tui (the orchestra
program), orchestra-ptyd (the session daemon), orchestra-api (the external
HTTP API) and orchestra-mcp (the MCP server for coding agents).

Options:
  -d, --debug         Build in debug mode (default: release)
  -i, --install       Also cargo install the binaries (~/.cargo/bin)
  -t, --test-only     Run the workspace tests instead of building
  -h, --help          Show this help

Examples:
  $0                  # Build release binaries
  $0 --debug          # Build debug binaries
  $0 --install        # Build and install globally
  $0 --test-only      # Run cargo test --workspace

The built binaries are:
  - Release: ./tui/target/release/{tui,orchestra-ptyd,orchestra-api,orchestra-mcp}
  - Debug:   ./tui/target/debug/{tui,orchestra-ptyd,orchestra-api,orchestra-mcp}

./orchestra.sh finds them automatically.
EOF
}
