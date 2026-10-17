#!/bin/bash

# Rakumux local development command.
# Usage: rakumux-local [--root <path>] [--worktree <name>] [extra args]
#
# - Default root is $HOME/repos/rakumux (override via RAKUMUX_LOCAL_ROOT
#   or --root).
# - Worktree flag resolves to <root>/worktrees/<name>/rakumux.sh for convenience.

set -euo pipefail

print_usage() {
  cat <<'EOF'
Usage: rakumux-local [--root <path>] [--worktree <name>] [args...]

Launch the local Rakumux TUI directly from your cloned repository.

Options:
  --root <path>       Path to the rakumux repo (default: $HOME/repos/rakumux)
                      or the value of $RAKUMUX_LOCAL_ROOT if set.
  -w, --worktree <n>  Use the worktree wrapper at <root>/worktrees/<n>/rakumux.sh
  -h, --help          Show this help and exit.

Any remaining arguments are forwarded to the local rakumux.sh script.

Environment:
  RAKUMUX_LOCAL_ROOT  Overrides the default repo path.
EOF
}

main() {
  local root="${RAKUMUX_LOCAL_ROOT:-$HOME/repos/rakumux}"
  local worktree=""
  local args=()

  while [[ $# -gt 0 ]]; do
    case $1 in
      --root)
        [[ $# -lt 2 ]] && { echo "--root requires a value" >&2; exit 1; }
        root="$2"
        shift 2
        ;;
      -w|--worktree)
        [[ $# -lt 2 ]] && { echo "--worktree requires a value" >&2; exit 1; }
        worktree="$2"
        shift 2
        ;;
      -h|--help)
        print_usage
        return 0
        ;;
      --)
        shift
        args+=("$@")
        break
        ;;
      *)
        args+=("$1")
        shift
        ;;
    esac
  done

  if [[ ! -d "$root" ]]; then
    echo "rakumux-local: repository not found at $root" >&2
    echo "Set RAKUMUX_LOCAL_ROOT or use --root <path> to point to your clone." >&2
    exit 1
  fi

  local script_path="$root/rakumux.sh"
  if [[ -n "$worktree" ]]; then
    script_path="$root/worktrees/$worktree/rakumux.sh"
  fi

  if [[ ! -f "$script_path" ]]; then
    echo "rakumux-local: unable to locate $script_path" >&2
    exit 1
  fi

  # This launcher is explicitly for local development; ignore installed binary
  # overrides (inherited from an installed wrapper or a Rakumux session) so
  # rakumux.sh resolves the TUI and daemon binaries from the selected checkout.
  unset GW_TUI_BIN GW_PTYD_BIN

  if [[ ${#args[@]} -gt 0 ]]; then
    exec bash "$script_path" "${args[@]}"
  else
    exec bash "$script_path"
  fi
}

main "$@"
