# shellcheck shell=bash

gwr_info() { printf "${GW_COLOR_BLUE:-}[INFO]${GW_COLOR_RESET:-} %s\n" "$*" >&2; }
gwr_success() { printf "${GW_COLOR_GREEN:-}[SUCCESS]${GW_COLOR_RESET:-} %s\n" "$*" >&2; }
gwr_warn() { printf "${GW_COLOR_YELLOW:-}[WARN]${GW_COLOR_RESET:-} %s\n" "$*" >&2; }
gwr_error() { printf "${GW_COLOR_RED:-}[ERROR]${GW_COLOR_RESET:-} %s\n" "$*" >&2; }
