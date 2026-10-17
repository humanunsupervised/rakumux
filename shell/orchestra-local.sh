#!/bin/bash

# Orchestra local development command.
# Usage: orchestra-local [--root <path>] [--worktree <name>] [extra args]
#
# - Default root is $HOME/repos/orchestrator (override via ORCHESTRA_LOCAL_ROOT
#   or --root).
# - Worktree flag resolves to <root>/worktrees/<name>/orchestra.sh for convenience.

set -euo pipefail

print_usage() {
  cat <<'EOF'
Usage: orchestra-local [--root <path>] [--worktree <name>] [args...]

Launch the local Orchestra TUI directly from your cloned repository.

Options:
  --root <path>       Path to the orchestrator repo (default: $HOME/repos/orchestrator)
                      or the value of $ORCHESTRA_LOCAL_ROOT if set.
  -w, --worktree <n>  Use the worktree wrapper at <root>/worktrees/<n>/orchestra.sh
  -h, --help          Show this help and exit.

Any remaining arguments are forwarded to the local orchestra.sh script.

Environment:
  ORCHESTRA_LOCAL_ROOT  Overrides the default repo path.
EOF
}

main() {
  local root="${ORCHESTRA_LOCAL_ROOT:-$HOME/repos/orchestrator}"
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
    echo "orchestra-local: repository not found at $root" >&2
    echo "Set ORCHESTRA_LOCAL_ROOT or use --root <path> to point to your clone." >&2
    exit 1
  fi

  local script_path="$root/orchestra.sh"
  if [[ -n "$worktree" ]]; then
    script_path="$root/worktrees/$worktree/orchestra.sh"
  fi

  if [[ ! -f "$script_path" ]]; then
    echo "orchestra-local: unable to locate $script_path" >&2
    exit 1
  fi

  # This launcher is explicitly for local development; ignore installed binary
  # overrides (inherited from an installed wrapper or an Orchestra session) so
  # orchestra.sh resolves the TUI and daemon binaries from the selected checkout.
  unset GW_TUI_BIN GW_PTYD_BIN

  if [[ ${#args[@]} -gt 0 ]]; then
    exec bash "$script_path" "${args[@]}"
  else
    exec bash "$script_path"
  fi
}

main "$@"
