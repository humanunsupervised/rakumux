#!/usr/bin/env bash

# Rakumux command history hook
#
# Source this file from your shell profile (bash or zsh):
#
#   source /path/to/rakumux/shell/rakumux-command-hook.sh
#
# Inside a Rakumux session (RAKUMUX_SESSION_ID is set) every command you
# run is appended to
#
#   ${RAKUMUX_HOME:-$HOME/.rakumux}/history/<session id>.log
#
# as "<timestamp>\t<command>" lines. Rakumux reads this history, together
# with the visible screen, to suggest names for auto-named sessions. Outside
# Rakumux sessions nothing is installed or logged.
#
# On Bash a DEBUG trap logs each command before it runs; on Zsh a preexec
# hook does.
#
# It also defines `rakumux_rename_session <new name>`, which renames the
# current session.

rakumux_rename_session() {
  if [[ -z "$*" ]]; then
    printf 'Usage: rakumux_rename_session <new name>\n' >&2
    return 2
  fi
  if [[ -z "${RAKUMUX_SESSION_ID-}" ]]; then
    printf 'rakumux_rename_session must be run inside a Rakumux session\n' >&2
    return 1
  fi
  "${RAKUMUX_BIN:-rakumux}" rename "$*"
}

if [[ -n "${RAKUMUX_PROMPT_HOOK_INSTALLED-}" ]]; then
  return 0 2>/dev/null || exit 0
fi
RAKUMUX_PROMPT_HOOK_INSTALLED=1

# True inside a Rakumux session whose id is safe to use as a file name.
_rakumux_in_session() {
  [[ -n "${RAKUMUX_SESSION_ID-}" && "$RAKUMUX_SESSION_ID" =~ ^[A-Za-z0-9_-]+$ ]]
}

_rakumux_capture_command() {
  local cmd=""
  if [[ -n "${ZSH_VERSION-}" ]]; then
    # fc -ln -1 returns the most recent command without line numbers
    cmd="$(fc -ln -1 2>/dev/null)"
  else
    cmd="$(history 1 2>/dev/null)"
    cmd="${cmd#*[0-9] }"
  fi
  printf '%s' "$cmd"
}

rakumux_log_command() {
  _rakumux_in_session || return 0

  local command="${1-}"
  if [[ -z "$command" ]]; then
    command="$(_rakumux_capture_command)"
  fi
  # One line per command.
  command="${command//$'\n'/ }"
  command="${command#"${command%%[![:space:]]*}"}"
  command="${command%"${command##*[![:space:]]}"}"
  if [[ -z "$command" ]]; then
    return 0
  fi

  case "$command" in
    rakumux_*|_rakumux_*)
      return 0
      ;;
  esac

  # Avoid logging duplicate consecutive commands within the same shell
  if [[ "$command" == "${RAKUMUX_LAST_COMMAND-}" ]]; then
    return 0
  fi
  RAKUMUX_LAST_COMMAND="$command"

  # Same path as session_core::paths::Layout::history_file.
  local dir="${RAKUMUX_HOME:-$HOME/.rakumux}/history"
  if [[ ! -d "$dir" ]]; then
    mkdir -p "$dir" 2>/dev/null || return 0
  fi
  local file="$dir/$RAKUMUX_SESSION_ID.log"
  printf '%s\t%s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$command" >>"$file" 2>/dev/null || true
}

_rakumux_install_hook_bash() {
  _rakumux_bash_preexec_hook() {
    if [[ "${RAKUMUX_DEBUG_TRAP_ACTIVE-}" == "1" ]]; then
      return
    fi
    RAKUMUX_DEBUG_TRAP_ACTIVE=1
    local cmd="${BASH_COMMAND-}"
    RAKUMUX_DEBUG_TRAP_ACTIVE=0
    rakumux_log_command "$cmd"
  }

  local existing_debug
  existing_debug="$(trap -p DEBUG 2>/dev/null | sed -n "s/^trap -- '\(.*\)' DEBUG$/\1/p")"
  if [[ "$existing_debug" == *"_rakumux_bash_preexec_hook"* ]]; then
    return
  fi

  if [[ -n "$existing_debug" ]]; then
    trap "_rakumux_bash_preexec_hook; $existing_debug" DEBUG
  else
    trap '_rakumux_bash_preexec_hook' DEBUG
  fi
}

_rakumux_install_hook_zsh() {
  _rakumux_zsh_preexec_hook() {
    rakumux_log_command "$1"
  }

  if ! command -v add-zsh-hook >/dev/null 2>&1; then
    autoload -Uz add-zsh-hook 2>/dev/null || true
  fi
  if command -v add-zsh-hook >/dev/null 2>&1; then
    add-zsh-hook preexec _rakumux_zsh_preexec_hook
  else
    # Fallback: append to preexec_functions manually
    if [[ -z "${preexec_functions[(r)_rakumux_zsh_preexec_hook]}" ]]; then
      preexec_functions+=(_rakumux_zsh_preexec_hook)
    fi
  fi
}

_rakumux_install_hook() {
  case "$-" in
    *i*) ;;
    *)
      return
      ;;
  esac

  # Only shells running inside a Rakumux session log history.
  _rakumux_in_session || return

  if [[ -n "${ZSH_VERSION-}" ]]; then
    _rakumux_install_hook_zsh
  else
    _rakumux_install_hook_bash
  fi
}

_rakumux_install_hook
