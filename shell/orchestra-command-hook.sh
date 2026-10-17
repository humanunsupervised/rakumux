#!/usr/bin/env bash

# Orchestra command history hook
#
# Source this file from your shell profile (bash or zsh):
#
#   source /path/to/orchestra/shell/orchestra-command-hook.sh
#
# Inside an Orchestra session (ORCHESTRA_SESSION_ID is set) every command you
# run is appended to
#
#   ${ORCHESTRA_HOME:-$HOME/.orchestra}/history/<session id>.log
#
# as "<timestamp>\t<command>" lines. Orchestra reads this history, together
# with the visible screen, to suggest names for auto-named sessions. Outside
# Orchestra sessions nothing is installed or logged.
#
# On Bash a DEBUG trap logs each command before it runs; on Zsh a preexec
# hook does.
#
# It also defines `orchestra_rename_session <new name>`, which renames the
# current session.

orchestra_rename_session() {
  if [[ -z "$*" ]]; then
    printf 'Usage: orchestra_rename_session <new name>\n' >&2
    return 2
  fi
  if [[ -z "${ORCHESTRA_SESSION_ID-}" ]]; then
    printf 'orchestra_rename_session must be run inside an Orchestra session\n' >&2
    return 1
  fi
  "${ORCHESTRA_BIN:-orchestra}" rename "$*"
}

if [[ -n "${ORCHESTRA_PROMPT_HOOK_INSTALLED-}" ]]; then
  return 0 2>/dev/null || exit 0
fi
ORCHESTRA_PROMPT_HOOK_INSTALLED=1

# True inside an Orchestra session whose id is safe to use as a file name.
_orchestra_in_session() {
  [[ -n "${ORCHESTRA_SESSION_ID-}" && "$ORCHESTRA_SESSION_ID" =~ ^[A-Za-z0-9_-]+$ ]]
}

_orchestra_capture_command() {
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

orchestra_log_command() {
  _orchestra_in_session || return 0

  local command="${1-}"
  if [[ -z "$command" ]]; then
    command="$(_orchestra_capture_command)"
  fi
  # One line per command.
  command="${command//$'\n'/ }"
  command="${command#"${command%%[![:space:]]*}"}"
  command="${command%"${command##*[![:space:]]}"}"
  if [[ -z "$command" ]]; then
    return 0
  fi

  case "$command" in
    orchestra_*|_orchestra_*)
      return 0
      ;;
  esac

  # Avoid logging duplicate consecutive commands within the same shell
  if [[ "$command" == "${ORCHESTRA_LAST_COMMAND-}" ]]; then
    return 0
  fi
  ORCHESTRA_LAST_COMMAND="$command"

  # Same path as session_core::paths::Layout::history_file.
  local dir="${ORCHESTRA_HOME:-$HOME/.orchestra}/history"
  if [[ ! -d "$dir" ]]; then
    mkdir -p "$dir" 2>/dev/null || return 0
  fi
  local file="$dir/$ORCHESTRA_SESSION_ID.log"
  printf '%s\t%s\n' "$(date '+%Y-%m-%dT%H:%M:%S%z')" "$command" >>"$file" 2>/dev/null || true
}

_orchestra_install_hook_bash() {
  _orchestra_bash_preexec_hook() {
    if [[ "${ORCHESTRA_DEBUG_TRAP_ACTIVE-}" == "1" ]]; then
      return
    fi
    ORCHESTRA_DEBUG_TRAP_ACTIVE=1
    local cmd="${BASH_COMMAND-}"
    ORCHESTRA_DEBUG_TRAP_ACTIVE=0
    orchestra_log_command "$cmd"
  }

  local existing_debug
  existing_debug="$(trap -p DEBUG 2>/dev/null | sed -n "s/^trap -- '\(.*\)' DEBUG$/\1/p")"
  if [[ "$existing_debug" == *"_orchestra_bash_preexec_hook"* ]]; then
    return
  fi

  if [[ -n "$existing_debug" ]]; then
    trap "_orchestra_bash_preexec_hook; $existing_debug" DEBUG
  else
    trap '_orchestra_bash_preexec_hook' DEBUG
  fi
}

_orchestra_install_hook_zsh() {
  _orchestra_zsh_preexec_hook() {
    orchestra_log_command "$1"
  }

  if ! command -v add-zsh-hook >/dev/null 2>&1; then
    autoload -Uz add-zsh-hook 2>/dev/null || true
  fi
  if command -v add-zsh-hook >/dev/null 2>&1; then
    add-zsh-hook preexec _orchestra_zsh_preexec_hook
  else
    # Fallback: append to preexec_functions manually
    if [[ -z "${preexec_functions[(r)_orchestra_zsh_preexec_hook]}" ]]; then
      preexec_functions+=(_orchestra_zsh_preexec_hook)
    fi
  fi
}

_orchestra_install_hook() {
  case "$-" in
    *i*) ;;
    *)
      return
      ;;
  esac

  # Only shells running inside an Orchestra session log history.
  _orchestra_in_session || return

  if [[ -n "${ZSH_VERSION-}" ]]; then
    _orchestra_install_hook_zsh
  else
    _orchestra_install_hook_bash
  fi
}

_orchestra_install_hook
