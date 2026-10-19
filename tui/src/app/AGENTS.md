# tui/src/app – TUI application state and logic

This directory holds the state, key handling and session pipelines of the `rakumux` TUI. The TUI is
one client of the `rakumux-ptyd` daemon: everything it knows about sessions comes from
`session_core::client::SessionApi`, and other TUIs, CLI calls and agents may change the same
sessions at any time. Rendering lives in `tui/src/ui.rs`, `components/`, `terminal_view/`, `layout/`
and `help/`; nothing here draws.

Architecture: the root `AGENTS.md`. The module map below describes the screen, layouts, keys,
new-session flow, and code boundaries.

## Module map

- `app.rs` (in `tui/src`): facade. Owns `App` (state slices below, `Arc<dyn SessionApi>`, config,
  updater state) and dispatches events by mode and focus.
- `types.rs`: `AppMode`, `AppResult`, dialog structs, shared enums.
- `config.rs`: `~/.rakumux/config.json` load/save: API keys, AI provider and model, and
  `sessions { rakumux_key (prefix_key read for older configs), cycle_keys,
  cycle_keys_in_session, welcome_seen, phone_offer_seen, layout, icons, sidebar_width,
  stacked_list_percent,
  session_order }`. Retired
  keys (`selected_projects`) are dropped on load, so the next save leaves them out.
  `sessions.ai_activity_summaries` defaults off; enabling it requires privacy confirmation.
  The daemon owns generation and caching. Clients only render `SessionView.activity_summary`.
- `folders.rs`: the folders the daemon knows (`Folders`: `known`
  `ProjectView`s, the launch folder, `FolderSupport`), the sync loader (`add_project` the launch
  folder, `projects`, then `subscribe { all }` + `list_all` with `sessions.all`; else
  `subscribe { scopes }` + `list_in` of the known folders; else `list(Some(launch))` on
  `unsupported`), `compare_folders` (launch folder first, then name, then path),
  `App::folder_label`, `App::current_folder` (the current section, retained on the folder filter
  and "+ New session" rows; a single-folder filter takes precedence; without folder context,
  the launch folder), `App::show_folders` (sets the filter; an
  older daemon resyncs for a folder it isn't watching) and the Folders modal's actions (filter
  rows, All folders, show one, tick and show several, add or create a typed folder).
  `handlers/folders.rs` routes its keys.
- `path_complete.rs`: the folders that complete a typed path (`suggest`: folders and links to
  them, dot-folders only once the name starts with `.`, case-insensitive only when nothing else
  matches, capped reads; `expand` for `~` and relative paths). The Folders modal lists them as
  rows (`PickerRow::Suggestion`): `↑`/`↓` choose, Tab or `→` completes, Enter adds, a click
  completes.
- `session_list.rs`: every listed session (`Listed::All`, or the folders an older daemon listed)
  and the folder filter (`set_folder_filter`; empty = All folders), applied locally with the
  text filter in `visible()`. `rows()` starts with the folder filter (`Selection::Folders`; Enter
  opens the Folders modal), puts a selectable header row per folder when more than one is shown
  (`Selection::FolderHeader`; the launch folder first, then by name, then path; with no filter
  the folders that have sessions, with one the filter's, empty ones included), and ends with "+
  New session". The order never changes by itself: pinned sessions lead their folder, both parts
  follow the user's order (`sessions.session_order`), and sessions without a place go last,
  oldest first. `move_session` moves one within its folder and its pinned or unpinned part
  (`J`/`K`, and the drag in `sessions.rs`: `App::drag`, `SessionDrag`); `group_target` is
  `{`/`}`.
- `screen.rs`: screen geometry for a size, layout, focus, zoom and the dragged pane sizes
  (`layout::PaneSizes`: `sidebar_width`, `stacked_list_percent`). No header: the panes start at
  the top row and the footer (mode badge, messages, Help/Settings keys) is always exactly the
  last row, zoom included, so modes, selection and messages never move a pane. Phones
  (`layout::is_phone`, under 80 columns, `App::phone`) show the list alone in Rakumux mode and
  the session under a tab bar in session mode (no title row; `App::list_layout` is Sidebar there).
  Narrow Sidebar windows (80–99 columns) stack the list above the detail in Rakumux mode
  (`Screen::stacked`, the divider a row) and give the session everything above the footer in
  session mode; `terminal_size` is always the session-mode pane.
- Terminal size: `sync_terminal_size` (attach, own pane change) and `claim_terminal_size` (the
  user did something: key, click, paste, focus-in; at most once a second) send `Resize`
  (controller) or `Fit` (observer) when the session's size isn't this window's; the daemon
  ignores a `Fit` while the controller typed in the last 5 s. Neither runs while no session is
  on screen (a phone's list). Nobody fits back on its own.
- Mouse (`sessions.rs::handle_mouse`): clicks go to the targets the last draw recorded
  (`App::hits`, `Hit`): list rows, the folder filter, the Folders modal's rows, and the footer's
  mode badge (`Hit::ModeBadge`: `enter_rakumux_mode` / `return_to_session`) and key hints
  (`Hit::FooterKey`: what the keys do). The divider drags side to side, or up and down when
  stacked. A click on the previewed session (its title row or terminal) goes in. On a phone a
  tap on a session row (or "+ New session") opens it, and a tap on a tab switches session in
  session mode.
- `constants.rs`: timings and UI constants. `feature_flags.rs`: code-only flags (`auth: false`).
- State slices (`state/`):
  - session list: every folder's `SessionView`s in the user's order, the folder filter, the
    text filter, selection, the last selected folder section, the nesting-guard session;
  - detail: the active attach stream, its `TerminalView`, current role (observer/controller),
    detail tab (Terminal / Info), saved scrollback for dead sessions;
  - focus: list (or tab bar) / session / dialog, zoom, layout (`layout::SessionLayout`).
- `sessions.rs`: pipelines over `SessionApi`: create, rename, delete, kill, revive, touch; attach,
  detach, promote and demote; applying `EventEnvelope`s to the list; re-listing after a reconnect;
  triggering AI naming (`crate::naming`) when a generated-name session first goes idle or done.
- `handlers/`: key handling by mode (`Focus::List` = Rakumux mode, `Focus::Session` = session
  mode):
  - Rakumux mode (`handlers/list.rs`): arrows and Vim keys (`j`/`k`, `gg`/`G`, `Ctrl-d`/`u`,
    `{`/`}` folders), `gg` via `App::pending_key`, `d`/`dd` opens delete confirmation, Enter/`i`
    go in, Esc (cancel first) or the
    Rakumux key back to `App::came_from`, Tab to `App::recent.previous`, `1`-`9`, and the action
    letters (`n`, `r`, `p`, `J`/`K` move, `x`, `z`, `P`, `/`, `?`, `,`, `R` confirms a UI restart,
    `q`, `\`);
  - session mode (`handlers/session.rs`): the Rakumux key enters Rakumux mode
    (`App::enter_rakumux_mode`), the cycle keys only with `cycle_keys_in_session`, then
    `terminal_view::key_to_bytes` / `mouse_to_bytes` / `paste_to_bytes` to the attach stream;
  - own Promote/Demote go through `Attachment::change_role`, so their `RoleChanged` answers aren't
    taken for another client's takeover;
  - new session (`new_session.rs`) creates immediately with a generated name in
    `App::current_folder`, selects it and enters its login shell. There is no naming dialog.
    A hidden destination folder is added to the filter; input cannot reach the old session while creation/attach is pending.
  - dialogs: the Folders modal (`AppMode::Folders`, `handlers/folders.rs`), rename, delete confirmation, upgrade, daemon-restart consent,
    and the Settings and Help modals (`AppMode::Modal`, `help::ModalState::handle_key`; `,` or `o`
    opens Settings, `?` Help, in Rakumux mode). Settings' Keys tab runs the selected
    `help::KeyAction` (`handlers/settings.rs::run_action`); the General tab's API key, provider and
    model dialogs go back to it. The welcome tour (`AppMode::Welcome(WelcomeState)`,
    `handlers/dialogs.rs::handle_welcome_key`, `components/welcome.rs`) opens from `main.rs` while
    `sessions.welcome_seen` is false, and from `t` in Help (`ModalAction::Tour`) or the Keys tab;
    `App::close_welcome` saves its "Don't show this on startup" box (on every card).
  - Phone access (`phone_access.rs`, over `crate::remote_access::PhoneAccess`, a fake
    `FakePhone` in tests): `App::start` checks it in a job (`AppEvent::PhoneChecked`); the
    one-time offer (`AppMode::PhoneOffer`) waits until Rakumux mode has no dialog
    (`App::offer_phone_access`, also on every tick) and saves `sessions.phone_offer_seen` as it
    opens; the pairing dialog (`AppMode::Pairing`) turns it on, shows the QR code and turns it off
    (`AppEvent::PhonePairing`, `PhoneOff`); from Settings' Phone access row it goes back there.
  - the prefix commands work in both panes (`handlers/list.rs` routes them too).

## Data flow

```
crossterm events ─┐
attach stream ────┼─ tokio::select! (main.rs) → App → handlers/* → sessions.rs → SessionApi
event stream ─────┤                                      │                         │
timers ───────────┘                                      └─ state slices ◄──────────┘
                                                              │
                                    ui.rs + components/ + layout/ + help/ + terminal_view/
```

- `SessionApi` calls are blocking; run them off the UI task (`spawn_blocking` or a worker thread)
  and feed results back as messages.
- The attach and event streams deliver through blocking channels; a pump thread forwards them into
  tokio channels for the `select!`.
- Redraw at most ~60 fps, immediately after input.

## Rules

- Talk to the daemon only through `SessionApi`. No socket code, no SQLite, no PTYs, no `Command`
  spawning for session work, no git, no tmux.
- Never assume this TUI is the only client: the list is driven by events, a session can be renamed,
  deleted or taken over (`role_changed`) by someone else, and every pipeline must handle
  `not_found` / `session_not_live` gracefully.
- Select = attach as observer; Enter = promote; leaving session focus = demote. Only the
  controller's pane size is sent to the PTY.
- `RAKUMUX_SESSION_ID` set → that session is "current" and must not be attached (nesting guard).
- Shortcut text shown to the user (footer hints, the Keys tab, Help) is generated from the
  configured keys.
- Errors from the daemon are `ApiError { code, message }`; show the message, branch on the code.

## When to edit what

- New key in list or session focus: `handlers/` → a pipeline in `sessions.rs` (or a dialog) → state.
- Session operations and event handling: `sessions.rs`.
- New config option: `config.rs` (serde default so older files still load) + a `SettingsItem` row
  on the Settings modal's General tab (`App::modal_context`, `handlers/settings.rs`).
- New action: a `help::KeyAction` row (label, keys) and its case in `handlers/settings.rs::run_action`.
- New dialog: its struct in `types.rs`, keys in `handlers/`, rendering in `components/`.
- Layout geometry or the tab bar: `tui/src/layout`. Settings and Help modals: `tui/src/help`.
- Terminal rendering or key encoding: `tui/src/terminal_view`.

## Testing

- `cd tui && cargo test -p tui`.
- Program against a fake `SessionApi` (in-memory sessions; `AttachStream::pair` and
  `EventStream::from_receiver` for streams). Never start a real daemon in unit tests; if a test needs
  files, set `RAKUMUX_HOME` to a temp dir.
- The fake (`testing.rs`) records `list_all`, `subscribe all` and `subscribe <scopes>`;
  `FakeApi::without_all` is a daemon without `sessions.all`, `FakeApi::without_folders` one from
  before folders.
- Cover list ordering and filtering (text and folder), event application, key routing (prefix, cycle keys, promote/
  demote, zoom, nesting guard), dialogs, and ratatui `TestBackend` renders at 80×24 and 160×48 for
  empty, live, exited and dead states in both layouts, at phone sizes (45×40, 40×20) for the
  list-then-session layout, taps and dialogs, and at narrow-window sizes (90×40, 90×20) for the
  stacked layout.
