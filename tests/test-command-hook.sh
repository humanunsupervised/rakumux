#!/usr/bin/env bash
# Tests for shell/orchestra-command-hook.sh: per-session history logging and
# orchestra_rename_session. Runs interactive bash (and zsh when installed)
# against a temporary ORCHESTRA_HOME; touches nothing else.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOOK="$(cd "$SCRIPT_DIR/.." && pwd)/shell/orchestra-command-hook.sh"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

failures=0
pass() { printf 'ok   %s\n' "$1"; }
fail() {
  printf 'FAIL %s\n' "$1"
  failures=$((failures + 1))
}

# run_shell <shell> <session id or empty> <script>
run_shell() {
  local shell="$1" id="$2" script="$3"
  local -a env_args=(HOME="$TMP/home" ORCHESTRA_HOME="$TMP/orchestra" PATH="$PATH" ORCHESTRA_BIN="$TMP/fake-orchestra")
  if [[ -n "$id" ]]; then
    env_args+=(ORCHESTRA_SESSION_ID="$id")
  fi
  case "$shell" in
    bash)
      env -i "${env_args[@]}" bash --norc --noprofile -i -c "source '$HOOK'; $script" </dev/null >"$TMP/out" 2>"$TMP/err" || true
      ;;
    zsh)
      # preexec only runs for commands read by the interactive shell itself.
      printf 'source %q\n%s\nexit\n' "$HOOK" "$script" |
        env -i "${env_args[@]}" zsh -f -i >"$TMP/out" 2>"$TMP/err" || true
      ;;
  esac
}

commands_logged() {
  cut -f2- "$1"
}

mkdir -p "$TMP/home"
cat >"$TMP/fake-orchestra" <<EOF
#!/usr/bin/env bash
printf '%s\n' "\$@" >"$TMP/rename-args"
EOF
chmod +x "$TMP/fake-orchestra"

shells=(bash)
if command -v zsh >/dev/null 2>&1; then
  shells+=(zsh)
else
  printf 'skip zsh (not installed)\n'
fi

for sh in "${shells[@]}"; do
  rm -rf "$TMP/orchestra"
  id="01TEST$(printf '%s' "$sh" | tr '[:lower:]' '[:upper:]')"
  log="$TMP/orchestra/history/$id.log"

  run_shell "$sh" "$id" $'true first\ntrue first\n: && true second\norchestra_rename_session ignored-by-log'
  if [[ -f "$log" ]]; then
    pass "$sh: history goes to \$ORCHESTRA_HOME/history/<id>.log"
  else
    fail "$sh: no history file at $log"
    continue
  fi
  if head -n1 "$log" | grep -Eq $'^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9:]{8}[+-][0-9]{4}\t'; then
    pass "$sh: lines are <timestamp>\\t<command>"
  else
    fail "$sh: unexpected line format: $(head -n1 "$log")"
  fi
  logged="$(commands_logged "$log")"
  if grep -qx 'true first' <<<"$logged" && grep -qx 'true second' <<<"$logged"; then
    pass "$sh: commands are logged"
  else
    fail "$sh: missing commands in log: $logged"
  fi
  if [[ "$(grep -cx 'true first' <<<"$logged")" == 1 ]]; then
    pass "$sh: consecutive duplicates are logged once"
  else
    fail "$sh: duplicate commands logged: $logged"
  fi
  if grep -q 'orchestra_rename_session' <<<"$logged"; then
    fail "$sh: hook's own functions were logged"
  else
    pass "$sh: hook's own functions are not logged"
  fi

  rm -rf "$TMP/orchestra"
  run_shell "$sh" "" 'true outside'
  if [[ -d "$TMP/orchestra/history" ]]; then
    fail "$sh: logged outside an Orchestra session"
  else
    pass "$sh: nothing logged outside an Orchestra session"
  fi

  run_shell "$sh" "../escape" 'true bad-id'
  if [[ -e "$TMP/orchestra/escape.log" || -d "$TMP/orchestra/history" ]]; then
    fail "$sh: unsafe session id was used as a path"
  else
    pass "$sh: unsafe session ids are ignored"
  fi

  rm -f "$TMP/rename-args"
  run_shell "$sh" "$id" 'orchestra_rename_session fixing the auth bug'
  if [[ -f "$TMP/rename-args" && "$(cat "$TMP/rename-args")" == $'rename\nfixing the auth bug' ]]; then
    pass "$sh: orchestra_rename_session calls \$ORCHESTRA_BIN rename \"<name>\""
  else
    fail "$sh: unexpected rename call: $(cat "$TMP/rename-args" 2>/dev/null || echo none)"
  fi

  rm -f "$TMP/rename-args"
  run_shell "$sh" "" 'orchestra_rename_session new name'
  if [[ ! -f "$TMP/rename-args" ]] && grep -q 'inside an Orchestra session' "$TMP/err"; then
    pass "$sh: orchestra_rename_session refuses outside a session"
  else
    fail "$sh: orchestra_rename_session ran outside a session"
  fi
done

if ((failures > 0)); then
  printf '%d failure(s)\n' "$failures"
  exit 1
fi
printf 'all command hook tests passed\n'
