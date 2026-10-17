#!/bin/bash

###############################################################################
# build.sh - Build script for Orchestra shell entrypoints and tui
###############################################################################

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$SCRIPT_DIR"

# Export color vars for sourced modules
export RED='\033[0;31m'
export GREEN='\033[0;32m'
export YELLOW='\033[1;33m'
export BLUE='\033[0;34m'
export NC='\033[0m'

# Source modularized helpers
source "$SCRIPT_DIR/shell/build/load.sh"
load

# Parse command line arguments
BUILD_TYPE="release"
INSTALL_GLOBAL=false
TEST_ONLY=false

while [[ $# -gt 0 ]]; do
  case $1 in
    -d|--debug)
      BUILD_TYPE="debug"
      shift
      ;;
    -i|--install)
      INSTALL_GLOBAL=true
      shift
      ;;
    -t|--test-only)
      TEST_ONLY=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      error "Unknown option: $1"
      usage
      exit 1
      ;;
  esac
done

build_main() {
  info "Starting build process..."

  check_dependencies

  if [[ "$TEST_ONLY" == "true" ]]; then
    rust_test
    return 0
  fi

  rust_tui "$BUILD_TYPE"

  if [[ "$INSTALL_GLOBAL" == "true" ]]; then
    install_global
  fi

  success "Build process completed!"

  info ""
  info "You can now run Orchestra from this checkout:"
  info "  ./orchestra.sh                       # TUI for the current folder"
  info "  ./orchestra.sh ls                    # CLI subcommands"
  info "  shell/orchestra-local.sh             # Same, from any folder (alias it as orchestra-local)"
  info "  ./tui/target/<profile>/orchestra-mcp  # MCP server for agents (see mcp/README.md)"
}

build_main "$@"
