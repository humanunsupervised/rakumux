# Manual Test Instructions

Rakumux manages **sessions**: a folder plus a terminal, owned by one background daemon
(`rakumux-ptyd`) per user. The TUI, the `rakumux` CLI and agents running inside sessions are all
clients of that daemon. There is no tmux, no git integration and no shell services layer; `tmux`,
`git`, `jq` and `python3` are not needed at runtime.

Every session belongs to the folder it was started in (its scope). The TUI shows the sessions of
all folders, narrowed by its folder filter (`P`); `rakumux ls` shows the folder it's run in.
Key bindings and layout are documented in [AGENTS.md](../AGENTS.md) and the in-app Help.

Use a throwaway Rakumux home so these tests don't touch your real sessions:

```bash
export RAKUMUX_HOME="$(mktemp -d)"
cd tui && cargo build --workspace && cd ..
mkdir -p /tmp/orch-a /tmp/orch-b
# Two worktrees of one repository, for the folder checks (setup only; Rakumux never runs git).
git init -q /tmp/orch-repo && git -C /tmp/orch-repo commit -q --allow-empty -m init
git -C /tmp/orch-repo worktree add -q ../orch-wt-a -b a
git -C /tmp/orch-repo worktree add -q ../orch-wt-b -b b
```

Launch from a test folder with the local build: `cd /tmp/orch-a && /path/to/rakumux/rakumux.sh`
(or `rakumux-local`, see `shell/rakumux-local.sh`).

## 1. First run and the daemon

- [ ] With no daemon running, `rakumux` starts one in the background and shows the TUI without
      noticeable delay; there is no header: the list's folder filter (or, in Tabs, the selector
      at the left of the tab bar) shows `▤ All folders ▾`, and the footer shows `⠋ connecting`
      after the badge until the daemon answers (`✕ daemon unavailable` if it can't start).
- [ ] On first start (no `welcome_seen` in `$RAKUMUX_HOME/config.json`) the welcome tour
      opens: six cards (the second is "Folders and sessions", before the two modes), `←`/`→`
      (`h`/`l`, Tab) move, Enter goes on, Esc closes. Every card shows "Don't show this on
      startup" and Space ticks it on any card. Closing without it ticked shows the tour again
      next start; ticked (closed with Esc on any card, or Enter on the last), it doesn't. `t` in
      Help (and "Welcome tour" in Settings, Keys) replays it, with the box as saved; unticking it there brings it back on startup. It fits at 80×24 and on a phone (45×40).
- [ ] With no sessions at all, the right pane says "No sessions yet" with `n` to start one in
      the launch folder.
- [ ] `rakumux daemon status` reports pid, version, uptime and session count.
- [ ] `rakumux daemon logs` shows `$RAKUMUX_HOME/logs/ptyd.log`.
- [ ] Works outside a git repository and with `tmux` not on `PATH`.

## 2. Creating sessions

- [ ] `n` opens the new-session dialog: only a name (optional), with the folder it goes in shown
      underneath (`in ▤ <name>  <path>`, in the chosen icon style). Enter creates, Esc
      cancels; Tab and the arrow keys don't leave the name.
- [ ] A blank name gets a generated name; the session is selected and focused as controller, and
      runs the login shell in its folder (`pwd`).
- [ ] The folder `n` uses: the selected session's; on a folder header, that folder (Enter there
      opens the dialog too); else the one folder the filter shows; else the launch folder (All
      folders on the folder filter or "+ New session"). A session made in a folder the filter
      hides adds that folder to the filter.
- [ ] Other folders and commands still work from the CLI: `rakumux new --cwd DIR --cmd 'echo
      ready'` and `rakumux new -- htop` (a folder outside the current folder is rejected with a
      clear error).

## 3. The live terminal pane

- [ ] Selecting a session shows its live output (observer); moving the selection switches streams.
- [ ] `Enter` focuses the session (controller): keys, paste and mouse go to the program.
- [ ] Full-screen programs render correctly: `vim`, `less`, `htop`, an AI agent (`claude`,
      `codex`, …), colours and wide characters included.
- [ ] Resizing the terminal resizes the session; full-screen programs redraw.
- [ ] Two modes: in a session every key reaches the program (try `Ctrl-b`, `Esc`, `Tab`,
      `Option+↑`) except `Ctrl-\`, which switches to Rakumux mode (the footer's bottom-left
      badge: orange RAKUMUX, SESSION in a session; on its right `? help  , settings`, after
      `Ctrl-\` in a session; switching modes, moving the selection or a message appearing never
      moves the panes). There: `j`/`k` preview other sessions in full colour (live output still
      moving; no colour or contrast change when you go in, faded only under Help or
      Settings); on the folder filter, a folder header and + New session the last session stays
      in view, in full colour (never one the folder filter hides);
      the list is never faded; `Esc` returns to the
      one you came from (after clearing a filter or a pending `g`), `Enter` goes into the
      selected one, `Tab` the one before, `1`-`9`, `z` full screen, `d` or `dd` delete (confirm), `x`
      close (confirm), `\` sends `Ctrl-\` to the session. Quick `Ctrl-\ Esc` never reports
      "Another client took control".
- [ ] Below 100 columns (Sidebar) Rakumux mode stacks the list above the selected session; in
      session mode the session fills the screen (see section 10).

## 4. Lifecycle

- [ ] Quit the TUI (`q`) and reopen it: sessions are still running and output produced meanwhile
      is visible.
- [ ] `R` in Rakumux mode (also Settings → Restart Rakumux) shows a restart warning that
      sessions keep running. `y`/Enter reloads the UI and latest build; `n`/Esc cancels. Repeating
      `R` doesn't confirm. Daemon and session PIDs stay unchanged. `R` still types in sessions and filters.
- [ ] A session whose program exits shows the exit code and "Enter to restart".
- [ ] `rakumux daemon stop`, then reopen: sessions show as dead with their saved scrollback;
      `Enter` revives them in the same folder.
- [ ] `rakumux daemon restart` with a busy program in one session (e.g. `top`, or an agent mid
      answer) and a TUI in session mode: the CLI says "restarted in place … kept running", the
      program's pid is unchanged, its screen is intact, and the TUI is back in session mode.
- [ ] `rakumux --new-server`: the same, then the TUI opens with the message in the footer; with no
      server running it starts one.
- [ ] `kill -9` the daemon with Claude Code (or Codex, OpenCode) in one session and `npm run dev`
      typed in another: the TUI reconnects, both sessions are running again, Claude Code reopened
      the same conversation with the same flags, and `npm run dev` waits at the prompt, not run.
- [ ] Log in another way (SSH and mosh, or Termius on a phone): `rakumux daemon status` shows the
      same pid from both.
- [ ] `r` renames (names are unique per scope); `d` and `dd` both open delete confirmation.
      A second `d` never confirms; `y` or Enter confirms, `n` or Esc cancels.
- [ ] Session descriptions show the app name before the branch. App colours stay consistent
      when selecting another session or switching modes; branch and state keep their usual colours.
- [ ] Navigate to another folder's session or header, then to "+ New session": the dialog's folder
      and the created session stay in that section's folder, not the launch folder. Check `n`, Enter,
      mouse selection, empty folder headers, and single- and multi-folder filters.

## 5. Several clients and folders

- [ ] Two TUIs show the same live list; output appears in both; only the one that pressed `Enter`
      last is controller.
- [ ] Start the TUI in `/tmp/orch-repo` and create a session there. From another terminal (same
      `RAKUMUX_HOME`) run `rakumux new --detach --cwd /tmp/orch-wt-a` and the same for
      `/tmp/orch-wt-b`: both appear live under All folders, each under a header (name, `⎇ a` /
      `⎇ b`, `1/1 live`), with `orch-repo` first and the others by name. No restart or refresh
      needed.
- [ ] `{`/`}` jump between folder groups; `↑`/`↓` stop on the headers.
- [ ] `P` (or Enter on, or a click on, `▤ All folders ▾`) opens the Folders modal: All folders
      ticked first, then every folder with sessions (name, branch, live/total, path); folders
      without sessions aren't listed. Typing filters the rows. Enter on `orch-wt-a` shows only
      it (`▤ orch-wt-a  ⎇ a ▾`, no headers); reopen, `Tab`/`Space` tick `orch-wt-a` and
      `orch-wt-b`, Enter shows both (`▤ 2 folders ▾`, with headers); reopen, Enter on All folders
      shows everything again. Esc closes without changes. The keys line matches the row
      (`⏎ show all`, `⏎ show`, `⏎ show 2 folders`, `Tab/Space tick`).
- [ ] Quit and start again: the list starts on All folders (the filter isn't saved).
- [ ] Typing `/tmp/or` lists "+ Create folder /tmp/or" and, below it, `/tmp/orch-a/`,
      `/tmp/orch-b/` and the others (folders only; dot-folders only after a `.`), with the first
      folder chosen and the keys line showing `↑↓ select`. `Tab` (or `→` at the end) puts the
      chosen folder in as `/tmp/orch-a/`, and its own folders are listed next; `↓` then `Tab`
      picks another. `Enter` on a folder adds it (`+ Add folder`) and shows it alone: "No
      sessions in orch-a yet", and `n` starts one there. `~/` lists your home folder's folders,
      shown as `~/…`. A click on a folder completes it.
- [ ] With an older daemon (no `sessions.all`), the TUI offers a restart naming "new folders
      live"; declined, it still lists the folders the daemon knew.
- [ ] Running `rakumux` inside one of its own sessions marks that session as current and refuses
      to attach to it (no infinite mirror).

## 6. Command line

- [ ] `rakumux new --name demo --detach` creates a session without attaching; `rakumux ls`
      lists it (`--json` for JSON, `--dead` includes dead sessions, `--all` lists every folder,
      `--folder <dir>` another one).
- [ ] `rakumux pin demo` marks it with `^` in `rakumux ls` and lists it first; `rakumux
      unpin demo` undoes it. In a git checkout the GIT column shows the branch (and the worktree
      in a linked worktree), and follows `git checkout` within a few seconds.
- [ ] `rakumux folders` lists the folders; `rakumux folders add <dir>` adds one,
      `rakumux folders rm <dir>` removes it only once it has no sessions.
- [ ] `rakumux attach demo` takes over the terminal; `Ctrl-\ d` detaches, `Ctrl-\ >`/`<`
      switch sessions, `Ctrl-\ n` creates one. Also try it over SSH.
- [ ] `rakumux rename demo demo2`, `rakumux kill demo2`, `rakumux rm demo2` behave as named.
- [ ] Inside a session: `rakumux session list`, `rakumux session read <id>` and
      `rakumux session report --state working --text "running tests"` work; `rakumux kill`
      on another session is refused with a "later release" message.

## 7. Command history and AI naming

- [ ] Add `source /path/to/rakumux/shell/rakumux-command-hook.sh` to `~/.bashrc` or
      `~/.zshrc`. In a new session, run a few commands: they appear as `<timestamp>\t<command>`
      lines in `$RAKUMUX_HOME/history/<session id>.log`. Outside sessions nothing is logged.
- [ ] `rakumux_rename_session my new name` inside a session renames it.
- [ ] Configure an Anthropic or OpenAI key in the API keys dialog (or export `ANTHROPIC_API_KEY` /
      `OPENAI_API_KEY`). Create a session with a blank name, run something meaningful
      (e.g. `cargo test`), and let it go idle: the session gets a descriptive name
      (`running_cargo_tests`-style, underscores only).
- [ ] With no key configured, sessions keep their generated names and nothing errors.

## 8. Shell wrapper

- [ ] With the `rakumux` wrapper function from `AGENTS.md` installed, quitting with a session
      selected can `cd` the calling shell into that session's folder, and the wrapper never hangs
      (`rakumux --help`, `rakumux --version`, quitting immediately).

## 9. List order

- [ ] Create three sessions with `n`: each new one appears at the bottom of its folder, just
      above "+ New session". Type in one, go in and out of the others, and let an agent work:
      no session moves. Tabs keep the same order.
- [ ] In Rakumux mode `J` moves the selected session down one place and `K` up; it stops at
      the ends of its folder, and a pinned session stays above the others (and an unpinned one
      below them). Inside a session `J` and `K` reach the program.
- [ ] Press a session row, drag it up or down and release: it moves as the pointer crosses the
      other rows, with a blue bar on its left while held. It stays in its folder when dragged
      over another folder's sessions. A click without moving only selects; clicking the
      session pane still goes in; the divider still resizes the list.
- [ ] In the Tabs layout, drag a tab left or right: it moves the same way.
- [ ] Quit and start again: the order is kept (`sessions.session_order` in
      `$RAKUMUX_HOME/config.json`). The first start still selects the most recently used session.

## 10. Phone

Phone terminals (Blink, Termius, a web terminal) are about 40–60 columns by 30–60 rows in
portrait, 15–25 rows with the keyboard up, and send taps as clicks. Try it on a phone, or size
a desktop terminal (or `tmux new -x 45 -y 40`) and click instead of tapping. Use the Sidebar
layout and create two or three sessions that print something.

- [ ] At 45×40 (either layout), Rakumux mode shows only the list: the folder filter, the
      sessions and + New session, full screen. `j`/`k` move the selection; nothing else changes.
- [ ] Tap a session row: you're in (SESSION badge), the session fills the screen under a row of
      tabs, sized to it. Tap another tab: that session, still in session mode.
- [ ] `Ctrl-\`, or a tap on the SESSION badge: back to the list (not the session). Tap the
      RAKUMUX badge: back into the session you came from (nothing happens when there's none).
      Tap `? help` and `, settings` in either mode; in session mode the footer shows
      `Ctrl-\ rakumux  Ctrl-\ ? help`, and tapping `Ctrl-\ rakumux` switches like the badge.
- [ ] At 40×20 (the keyboard up) the same works.
- [ ] At 90×40 (a narrow window, not a phone) the list is on top and the selected session below;
      drag the line between them up and down: the list grows and shrinks, keeping the folder
      filter and two sessions and four rows of the session. Quit and start again: the share is
      kept (`sessions.stacked_list_percent`), so at 90×20 the session still gets its part.
- [ ] A phone and a desktop window on the same session: go into it on the phone (it takes the
      phone's size), then press a key or click in the desktop window (or switch to it): the
      session takes the desktop's size within a second, unless you typed on the phone in the
      last 5 seconds; type on the phone and it takes the phone's size back. Moving through the
      phone's list resizes nothing.
- [ ] At 45×40 and 40×20 open each dialog: new session (its hint "blank: a name is picked for
      you" whole), rename, delete and close (confirm), the Folders modal, Help, Settings (every
      Layout and Icons choice shows, under its label), the API key, AI provider and OpenAI model
      dialogs, the welcome tour (every card whole, with its keys). Nothing that matters is cut
      off and every key line shows whole.
- [ ] At 100×30 and wider the list and the session are side by side, as before.
