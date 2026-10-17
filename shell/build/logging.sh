# shellcheck shell=bash

info() { printf "${BLUE:-}[INFO]${NC:-} %s\n" "$*"; }
success() { printf "${GREEN:-}[SUCCESS]${NC:-} %s\n" "$*"; }
warn() { printf "${YELLOW:-}[WARN]${NC:-} %s\n" "$*"; }
error() { printf "${RED:-}[ERROR]${NC:-} %s\n" "$*"; }
