# shellcheck shell=bash

check_dependencies() {
  info "Checking dependencies..."

  if ! command -v cargo >/dev/null 2>&1; then
    error "Rust/Cargo not found. Please install Rust: https://rustup.rs/"
    exit 1
  fi

  if ! command -v cc >/dev/null 2>&1 && \
     ! command -v clang >/dev/null 2>&1 && \
     ! command -v gcc >/dev/null 2>&1; then
    error "C compiler not found. Bundled SQLite requires a native C toolchain."
    error "  macOS: xcode-select --install"
    error "  Ubuntu/Debian: sudo apt install build-essential"
    error "  RHEL/CentOS: sudo yum groupinstall 'Development Tools'"
    exit 1
  fi

  success "Dependencies check passed"
}
