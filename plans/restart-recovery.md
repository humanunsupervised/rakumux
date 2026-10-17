# One server, restarts that keep sessions, recovery after a crash (rev 1)

## 1. What went wrong

On 2026-10-16 the user's sessions "lost their programs" when opened from Termius on a phone. Two
causes:

1. **Two servers over one registry.** The runtime directory (and so `service.json`, the lock and the
   socket) was `$XDG_RUNTIME_DIR/orchestra` when that variable was set, else `~/.orchestra/run`.
   Tailscale SSH logins set it; mosh logins don't. The phone (SSH) and the Mac (mosh) each found no
   daemon in "their" directory and started one. Both used `~/.orchestra/orchestra.sqlite3`, so each
   listed the other's sessions as dead, and Enter revived them as a second, empty shell: the same
   session ran twice, once per daemon.
2. **Restarts ended every program.** Restarting the daemon (the TUI's "older server" offer, or
   `orchestra daemon restart`) closed every PTY. Revive started the login shell, but no resume
   command was known for Claude Code (only agents that report one themselves had any), so the user
   got a bare prompt.

## 2. Decisions

1. **One daemon per Orchestra home, found the same way from every login.** The runtime directory is
   `<home>/run`, nothing else. Clients still look in the legacy `$XDG_RUNTIME_DIR/orchestra` (and
   `/run/user/<uid>/orchestra`) when no daemon answers in `<home>/run`, for daemons earlier builds
   started; a starting daemon refuses to run while a legacy one holds its lock.
2. **Start only when none runs.** The TUI and CLI connect to the running daemon and spawn one only
   when nothing answers. `orchestra --new-server` replaces the running daemon (in place, so sessions
   keep running) or starts one. `orchestra daemon start` starts one if none runs.
3. **Restart in place.** `daemon_restart {binary?}` (capability `daemon.restart`; also SIGUSR2 with
   the daemon's own executable): the daemon checks the target (`binary handoff-versions` must list
   the state version it writes), stops accepting, answers `ok`, hands over the control layer's
   memory (tokens, spawn environments, conversation bindings) and every running terminal (PTY
   master descriptor, screen checkpoint, history rows, size, offsets, title, cwd), writes the state
   to an unlinked 0600 file and `exec`s the target with `serve --handoff-fd <fd>`. Same pid, so the
   programs stay its children; the listening socket and the lock are inherited too, so clients that
   connect meanwhile wait in the backlog instead of starting another daemon. If the `exec` fails it
   tries its own executable, then resumes in the same process.
   - Child waiters only peek (`waitid(WNOWAIT)`) and the actor reaps, so a program that exits during
     the switch is reaped (with its exit code) by the new image.
   - Output written during the switch waits in the PTY.
   - Older daemons can't hand over: restarting them still ends their sessions (the CLI asks first).
4. **Recovery after an unplanned end.** The daemon keeps `~/.orchestra/running.json` (the running
   sessions) current. A start after a crash, a `kill -9`, a reboot or SIGTERM revives those sessions
   with their resume commands. `orchestra daemon stop` (`daemon_shutdown`) removes the file: sessions
   the user ended stay ended.
5. **Resume commands kept current** (`ptyd/src/resume.rs`, every 5 s of output or when the
   foreground process changes):
   - a known agent: the command that resumes the same conversation (`known_agent`, run on revive);
   - any other program: its command line (`last_command`), typed at the prompt on revive but not
     run, because running an arbitrary command twice may not be safe (a migration, a deploy);
   - a shell back at its prompt for 15 s: none (the delay keeps a reboot, which kills the program a
     moment before the daemon, from erasing it);
   - an agent's own report (`reported_by_agent`) wins while an agent runs; startup commands always
     run on revive.

## 3. Agents

Implemented (the three the user works with):

| Agent | Conversation found from | Resume command |
|---|---|---|
| Claude Code | `<CLAUDE_CONFIG_DIR or ~/.claude>/sessions/<pid>.json` (`sessionId`), written by the running process | `claude <flags> --resume <id>`; flags kept: permissions (`--dangerously-skip-permissions`, `--permission-mode`), `--model`, `--effort`, `--agent`, `--add-dir`, `--mcp-config`, `--settings`, `--allowedTools`, … |
| Codex | the rollout file (`$CODEX_HOME/sessions/…/rollout-…-<id>.jsonl`) the process or its native child has open (Linux); else the newest `cli` thread of the folder in `$CODEX_HOME/state_<n>.sqlite` | `codex resume <flags> <id>`; flags kept: `--model`, `--profile`, `--sandbox`, `--ask-for-approval`, `--full-auto`, `--dangerously-bypass-approvals-and-sandbox`/`--yolo`, `--search`, `-c` |
| OpenCode | the conversation its Orchestra plugin bound to the session (`session_bind`); else the newest top-level session of the folder in `<XDG_DATA_HOME>/opencode/opencode.db` | `opencode <flags> -s <id>`; flags kept: `--model`, `--agent` |

The database fallbacks read other programs' private schemas read-only; when they change, the
lookup finds nothing and the agent's command line is recorded instead (typed, not run). Two Codex
or OpenCode sessions in one folder without the precise source can resume the same conversation.

Next, by how widely they're used (to verify against each CLI before building):

| Agent | Detected today | Resume to use |
|---|---|---|
| Gemini CLI | yes (`gemini`) | `gemini --resume <id>`; chats under `~/.gemini/tmp/<project hash>/chats/` |
| Cursor CLI | yes (`cursor-agent`) | `cursor-agent --resume <chat id>` |
| GitHub Copilot CLI | no | `copilot --resume <id>` |
| Pi | yes (`pi`) | `pi --session <file>` or `--continue`; sessions under `~/.pi/agent/sessions/<folder>/` |
| Qwen Code | yes (`qwen`) | follows Gemini CLI's |
| Amp | no | `amp threads continue <id>` |
| Aider | yes (`aider`) | `aider --restore-chat-history` (per folder, no ids) |

A precise alternative for every agent with hooks (Claude Code, Codex, Gemini CLI): an opt-in
`orchestra agents install-hooks` that reports `session report --resume` at session start. It edits
the user's agent config, so it needs consent.

## 4. Status

- rev 1 (2026-10-16): implemented: layout, legacy lookup and lock check; `daemon_restart`, handoff,
  SIGUSR2; `running.json` and revival; resume plans for Claude Code, Codex, OpenCode and the last
  command; CLI `daemon start`, in-place `daemon restart`, `orchestra --new-server`; the TUI's restart
  offer in place, a faster first reconnect, and going back into the session being typed in.
