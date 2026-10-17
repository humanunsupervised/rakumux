# shellcheck shell=bash

gwr_show_usage() {
    cat <<EOF
orchestra - terminal sessions for coding agents

Usage: orchestra [global options] [command]

With no command, opens the Orchestra TUI for the current folder. Sessions belong
to the folder they were started in and keep running in the background (owned by
the orchestra-ptyd daemon) after you quit.

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
  --ptyd-path <PATH>      Use this orchestra-ptyd binary

Launcher options:
  -h, --help              Show this help
  --version               Show the version
  --check-updates, --update
                          Check for a newer release

Environment:
  GW_TUI_BIN              Path to the orchestra (tui) binary
  GW_PTYD_BIN             Path to the orchestra-ptyd binary
  ORCHESTRA_HOME          Data directory (default: ~/.orchestra)

Keys: open Help inside the TUI.

Examples:
  orchestra                         # Open the TUI here
  orchestra new --name dev -- pnpm dev
  orchestra ls
  orchestra attach dev
EOF
}
