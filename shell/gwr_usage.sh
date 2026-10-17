# shellcheck shell=bash

gwr_show_usage() {
    cat <<EOF
rakumux - terminal sessions for coding agents

Usage: rakumux [global options] [command]

With no command, opens the Rakumux TUI for the current folder. Sessions belong
to the folder they were started in and keep running in the background (owned by
the rakumux-ptyd daemon) after you quit.

Commands:
  new [--name N] [--cwd P] [--cmd C]... [--detach] [-- ARGV...]
                          Create a session (and attach unless --detach)
  ls [--json] [--dead] [--folder DIR]... [--all]
                          List sessions of this folder, of the given folders,
                          or of every folder (--dead: include dead)
  attach <name|id>        Full-screen attach (Ctrl-\\ d to detach)
  rename [<name|id>] <new name>
                          Rename a session (defaults to the current session)
  pin|unpin <name|id>     Pin a session to the top of its folder, or unpin it
  kill <name|id> [--tree] Terminate a session
  rm <name|id>            Delete a session
  folders [--json] | folders add|rm <dir>
                          List folders, add one, or forget one without sessions
  daemon status|stop|restart|logs
                          Manage the session daemon
  session list|get|read|wait|report
                          JSON interface for agents and scripts

Global options:
  -d, --debug             Enable debug output
  --ptyd-path <PATH>      Use this rakumux-ptyd binary

Launcher options:
  -h, --help              Show this help
  --version               Show the version
  --check-updates, --update
                          Check for a newer release

Environment:
  GW_TUI_BIN              Path to the rakumux (tui) binary
  GW_PTYD_BIN             Path to the rakumux-ptyd binary
  RAKUMUX_HOME          Data directory (default: ~/.rakumux)

Keys: open Help inside the TUI.

Examples:
  rakumux                         # Open the TUI here
  rakumux new --name dev -- pnpm dev
  rakumux ls
  rakumux attach dev
EOF
}
