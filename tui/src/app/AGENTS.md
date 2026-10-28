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
- Settings is one page (settings, then every action; values and keys pinned right), sized from
  its rows, at most 80% of the screen's height; `→` shows the selected row's note in a popover
  (the rows never shift) and the wheel moves the selection. Help keeps a reading width and tabs. All dialogs leave the footer row visible.
  The detail has no title row: the footer names the session in view (`session: ● <name>`, the
  mark its list row has). The footer is a full-width bar with that at its left, then messages,
  and the mode label (`RAKUMUX MODE`, `SESSION MODE`, `TERMINAL MODE`) in its centre: orange with dark text in
  Rakumux mode (`f ≡ Feed`, with `●N` new entries, `FooterKey::Feed` → `App::toggle_feed`;
  `? Help` and `, Settings` buttons in a darker orange, and `Sign in` while signed out,
  `FooterKey::SignIn` → `App::start_sign_in`), muted blue in session and
  terminal input modes (a `Ctrl-\` button); the label and buttons are hit targets.
- A session the user exits from inside (controller, session mode) is deleted on its `exited`
  stream event (`sessions.rs::on_stream_event`); one ending in the background stays `exited`.
- `upgrade.rs`: `U` / Settings' Update action, behind `updater::UpdateService` (a fake in tests).
  Jobs run off the UI thread; a generation drops cancelled/stale results. Installation needs
  Enter and cannot be started twice. Success reloads only the UI through the updated launcher.
  Cache writes use `App::paths`, never the real user's home in tests.
- `config.rs`: `~/.rakumux/config.json` load/save: API keys, AI provider and model (edited in the
  file; Settings doesn't show them), and
  `sessions { rakumux_key (prefix_key read for older configs), cycle_keys,
  cycle_keys_in_session, welcome_seen, phone_offer_seen, layout, icons, sidebar_width,
  stacked_list_percent, feed_width,
  session_order }`. Retired
  keys (`selected_projects`) are dropped on load, so the next save leaves them out.
- `folders.rs`: the folders the daemon knows (`Folders`: `known`
  `ProjectView`s, the launch folder, `FolderSupport`), the sync loader (`add_project` the launch
  folder, `projects`, then `subscribe { all }` + `list_all` with `sessions.all`; else
  `subscribe { scopes }` + `list_in` of the known folders; else `list(Some(launch))` on
  `unsupported`), `compare_folders` (launch folder first, then name, then path),
  `App::folder_label`, `App::current_folder` (the current section, retained on the folder filter
  and "+ New session" rows; a single-folder filter takes precedence under the Folders tab; without
  folder context, the launch folder), `App::show_folders` (sets the filter and makes the Folders tab
  active; an older daemon resyncs for a folder it isn't watching) and the Folders modal's
  actions (filter
  rows, All folders, show one, tick and show several, add or create a typed folder).
  `handlers/folders.rs` routes its keys.
- `path_complete.rs`: the folders that complete a typed path (`suggest`: folders and links to
  them, dot-folders only once the name starts with `.`, case-insensitive only when nothing else
  matches, capped reads; `expand` for `~` and relative paths). The Folders modal lists them as
  rows (`PickerRow::Suggestion`): `↑`/`↓` choose, Tab or `→` completes, Enter adds, a click
  completes.
- `grouping.rs`: the list tabs, pure derivations over `SessionView` (no drawing). `Grouping`
  (Status, Folders, App: `ALL` is the top row's order, `step` for ←/→, no wrap; the default,
  where every start opens, is Status), `AppKey::of` (agent kind, else a
  non-shell foreground program while live, else a non-shell `launch.argv[0]`, else `shell`;
  lowercase; dead sessions keep their agent), `StatusGroup::of` (like the
  status marks, `components::format::status_glyph`: waiting for you, unread (a `done` turn not yet looked at),
  working, waiting for response by activity phase, running, exited, dead, unknown, done (`idle`);
  display order = enum order; `saved_name` keeps the old `status:done`/`status:idle` keys), `Section`
  (an app or a status) and `sort_sections` (agents first, then programs, then `shell`, by name).
  `is_shell` is the one shell list (`format.rs` uses it too).
- `feed/`: the agent feed (`FeedStore`: state only, entries newest first by `finished_at`,
  capped at 500, the selection kept by id, the reply's scroll, `reading` (the selected reply
  read whole in the cards' place), the service's generation and revision, and the ids that were
  on screen: `mark_seen`, `unseen` counts the rest that finished after the window started) and
  `pane.rs`, which puts it on screen. `feed_open` (shown; this window's, hidden at every start,
  nothing opens it by itself) and `feed_focused` (it has Rakumux mode's keys: `feed_has_keys`;
  `feed_lost_keys` when session mode or the terminal panel takes them) go to the layout as
  `PaneSizes::feed` (`layout::FeedShown`), and `Screen::feed` says where it is: docked on the
  right (Master detail, Grid; `Screen::feed_divider`, `drag_feed_divider` saves
  `sessions.feed_width`), the session's place on narrower windows in Rakumux mode, the body
  under the tab bar while the Tabs layout's Feed tab is selected (`feed_tab_shown`;
  `feed_columns` puts the reply beside the cards from 100 columns), or the screen on a phone
  while it has the keys. `feed_shown` is whether it's on screen. `toggle_feed` (`f`, the
  footer's `FooterKey::Feed`, Settings' `KeyAction::Feed`): shown with the keys, the keys, or
  hidden; `unfocus_feed` (Esc, a click beside it, a grouping picked) gives the list the keys,
  hiding it where it would leave the screen; `feed_back` is the feed's Esc. Every change runs
  `feed_moved`: the session on screen takes its new pane size (`sync_terminal_size`), Grid's
  previews follow their cards. It polls `AccountAccess::feed` off the UI thread every 2 s while
  on screen, and every 10 s as a background read while the account's Feed switch is on, so
  `feed_unseen` can count for the footer and the Feed tab (again at once while `more`). Its keys
  (`feed_key`: ←/→ and `h` do nothing, `l` is Enter, ↑/↓ scroll a reply being read) and Enter
  (`open_feed_entry`: read the reply whole; from there, or beside the cards, a reply whose
  session is listed here goes into it under the grouping the sessions last had, one from
  another machine says where it runs). A reply on screen whole brings its whole turn
  (`AccountAccess::feed_turn`, off the UI thread, the rest asked for at once while `next`;
  `FeedStore` keeps the last 20 turns, `TurnState`), drawn in order by
  `components::feed::turn_lines` (`›` prompts, quiet reasoning, `●` tool calls with the first
  lines of their output). `components/feed.rs::draw_feed` draws it into `Screen::feed`: a band
  (`≡ Feed`, the accent while it has the keys, `‹` back while reading) over the cards or the
  reply; `components/session_tabs.rs` draws the Feed tab.
- `list_tabs.rs`: `App::tab_label` (the active tab names its filter, inactive ones say All
  folders / App / Status), `filter_label` (the Tabs layout's selector, Grid's header),
  `switch_tab`/`show_tab` (both give the list the keys from the feed), `open_tab_picker` (Folders: the Folders modal) and the App and
  Status modals (`AppMode::Sections(SectionPicker)`, keys in `handlers/sections.rs`): rows All
  then `picker_sections` (sessions' apps or every status but unknown, plus the filter's), typing
  narrows by name, Tab ticks (Space too while nothing is typed: status names have spaces), Enter
  shows (`show_sections`, which makes that tab active), Esc closes.
- `session_list.rs`: every listed session (`Listed::All`, or the folders an older daemon listed),
  the active tab (`grouping`, `set_grouping`) and one filter per tab: the folder filter
  (`set_folder_filter`; empty = All folders) and the App/Status filters (`set_section_filter`),
  of which only the active tab's applies, locally with the text filter in `visible()`. `rows()`
  starts with the tabs (`Selection::Folders`; ←/→ switch them, Enter opens the active tab's
  modal), puts a selectable header row per folder when more than one is shown
  (`Selection::FolderHeader`; the launch folder first, then by name, then path; with no filter
  the folders that have sessions, with one the filter's, empty ones included) or under App and
  Status per app or status (`Selection::SectionHeader`, same rules; Enter opens that tab's
  modal), and ends with "+ New session". Pinned sessions come first, in the pinned section
  (`ListRow::PinnedHeader`, `Selection::PinnedHeader`; the list counts as grouped whenever one
  is shown) under every tab, in its own order (`section_order["pinned"]`, `PINNED_ORDER`), the
  active tab's filter applied; the groups below leave them out. The order never changes by
  itself: sessions follow the user's order (`sessions.session_order`), and sessions without a
  place go last, oldest first; inside an app or status, folder by folder. Under App
  and Status the selected session is kept shown when it stops matching the filter (`kept`), so
  a status change never takes away the session being worked in; filter and tab changes
  (`refilter`) don't keep it. `tab_order` is what a row of session tabs shows (`ShownAs::Tabs`:
  the Tabs layout, a session under tabs on a phone or from Grid): `visible()`'s sessions in the
  Folders tab's order under every tab (pinned first), so reading a session or a status change
  never moves a tab; `move_tab` moves one there as under Folders. `App::shown_as` says which
  order is on screen, and cycling, `1`-`9`, `↑`/`↓`, `J`/`K`, tab clicks and drags follow it. `move_session` moves one within the pinned section, or its
  folder and its app or status (`J`/`K`, and the drag in `sessions.rs`: `App::drag`,
  `SessionDrag`); `group_target` is `{`/`}` over any headers. A folder filter only sets
  `current_folder` while the Folders tab is active.
- `screen.rs`: screen geometry for a size, layout, focus, zoom and the dragged pane sizes
  (`layout::PaneSizes`: `sidebar_width`, `stacked_list_percent`, `feed_width`) and the feed
  (`PaneSizes::feed`, `Screen::feed`; see `feed/`). No header: the panes start at
  the top row and the footer (session, messages, mode label, Help/Settings keys) is always exactly the
  last row, zoom included, so modes, selection and messages never move a pane. Phones
  (`layout::is_phone`, under 80 columns, `App::phone`) show the list alone in Rakumux mode
  (or Grid cards when Grid is selected) and the session under a tab bar in session mode
  (`App::list_layout` is Sidebar there except for Grid).
  Narrow Sidebar windows (80–99 columns) stack the list above the detail in Rakumux mode
  (`Screen::stacked`, the divider a row) and give the session everything above the footer in
  session mode; `terminal_size` is always the session-mode pane. Side by side the list can be
  hidden (`App::toggle_list`: `b`, Settings, the list's `« Hide list` row, `Hit::HideList`;
  `PaneSizes::list_hidden`, this window's only): `Screen::rail` (`layout::RAIL_COLS`) replaces
  the list and divider, and a click on it (`Hit::ShowList`) shows the list again.
- `top.rs`: Top (`AppMode::Top(TopView)`, `t` or Settings → Top; drawn by
  `components/top.rs`). Asks `SessionApi::resources` off the UI thread (`AppEvent::Resources`,
  checked against the opening's generation) on ticks about every second, the second sample half a
  second after the first; keeps only the last two samples, for rates (`Usage`: % of one CPU,
  memory, bytes a second), and forgets them on close. `TopView::rows` is pure: this window, the
  server, other windows, other clients, then sessions sorted by `TopSort` with their process
  trees (`tree_order`), folded per session; `top_rows` names sessions from the list (name, then
  `AppKey` and folder). The selection is kept by `RowKey` as rows change; Enter `go_into`s the
  row's session.
- `grid.rs` and `grid/tests.rs`: Grid's visible-page observer previews, generation checks,
  cancellation and bounded output mailboxes. `layout/grid.rs` computes up to six cards; the
  folder/text filters and pinned/user order come from `SessionList::visible`. Only the visible
  cards attach, always as `Role::Observer`, with zero scrollback; saved reads are capped.
  Offscreen cards, layout changes, activation, disconnect and dropping the app cancel previews.
  The preview lease can only detach: never Promote, Resize, Fit, input or Touch. Bell/clipboard
  events and terminal queries have no external effect. Grid browsing has no hidden detail
  attachment or `Screen::terminal`, so size claims cannot escape through the detail path.
  Cards render cropped/panned at the terminal's real size. Enter/tap uses normal activation;
  the Rakumux key returns to Grid. Arrows/h/j/k/l select, PgUp/PgDn page, n creates immediately.
  Settings > Layout or `v` cycles Master detail (`sidebar`, still default), Grid
  (`grid`) and Tabs (`tabs`); `v` is forwarded normally in session mode.
- Terminal size: `sync_terminal_size` (attach, own pane change) and `claim_terminal_size` (the
  user did something: key, click, paste, focus-in; at most once a second) send `Resize`
  (controller) or `Fit` (observer) when the session's size isn't this window's; the daemon
  ignores a `Fit` while the controller typed in the last 5 s. Neither runs while no session is
  on screen (a phone's list). Nobody fits back on its own.
- Mouse (`sessions.rs::handle_mouse`): clicks go to the targets the last draw recorded
  (`App::hits`, `Hit`): list rows, the list tabs (`Hit::ListTab`: activate one, or open the
  active one's modal; the band beside them, `Hit::FolderSelector`, opens it too), the Folders,
  App and Status modals' rows (`Hit::FolderRow`, `Hit::SectionRow`), and the footer's
  mode label (`Hit::ModeBadge`: `enter_rakumux_mode` / `return_to_session`) and key hints
  (`Hit::FooterKey`: what the keys do), the list's `« Hide list` row and the hidden list's rail
  (`Hit::HideList`, `Hit::ShowList`: `toggle_list`, in either mode), and the feed (`Hit::FeedEntry`
  selects a card, a second click opens it; `Hit::FeedBack`, `Hit::FeedReply`, `Hit::FeedPane`, the
  Tabs layout's `Hit::FeedTab`): a click on it gives it the keys, a click beside it (not the
  footer) gives them to the list. The divider drags side to side, or up and down when
  stacked; the line left of the docked feed (`Screen::feed_divider`) drags side to side.
  Clicking "+ New session" creates and enters its shell immediately at any width,
  using the current folder; pending creation ignores repeated clicks.
  A click on the previewed session's terminal goes in. On a phone a
  tap on a session row (or "+ New session") opens it, and a tap on a tab switches session in
  session mode.
- `constants.rs`: timings and UI constants. `feature_flags.rs`: code-only flags (`auth: false`).
- State slices (`state/`):
  - terminal panel (`terminal_panel.rs`): a temporary daemon-owned PTY under the main detail,
    not a session. `SessionApi::open_terminal`/`attach_terminal` use the `terminal.pane`
    capability; never `create`, `revive`, `delete`, list entries or saved session output.
    `Focus::Terminal` owns key/paste/mouse/cursor routing; no input leaks to the agent. T or
    Settings opens/focuses it, the Rakumux key demotes, Esc returns, T/× hides without ending
    its shell. Hiding keeps the stream and passive mirror alive, so a hidden exit still closes
    and cleans up the pane. Exit/EOF releases the lease and clears its id; reopening is a fresh
    terminal. Closing/reloading the UI drops the lease and ends only its temporary shell.
    Open/reattach run as jobs with checked epochs and one pending operation. An opaque `PaneId`
    cannot alias a session or a different pane after a crash. Reconnects only attach an existing
    pane as observer, never create replacements. Missing capability is an explicit restart
    message, not a hidden-session fallback. History has a cell budget;
    emulators are passive. Only the focused controller resizes. Private input includes loading
    and live panel keys. `Screen::with_terminal_panel` owns its split and short-window fallback.
  - hook activity: the daemon supplies normalized `SessionView.activity`; the UI renders
    phases/counts. `is_animating` keeps busy markers ticking without PTY output. Provider
    adapters belong in root `integrations/`, not app handlers. Right-side badges distinguish
    working, idle, unread reply (`Lifecycle::Done`) and needs input. Observer previews never
    acknowledge unread replies. Activation, or a completion arriving in the foreground open
    conversation at the bottom of its history, acknowledges through `SessionApi::touch`.
  - session list: every folder's `SessionView`s in the user's order, the folder filter, the
    text filter, selection, the last selected folder section, the nesting-guard session;
  - detail: the active attach stream, its `TerminalView`, current role (observer/controller),
    detail tab (Terminal / Info), saved scrollback for dead sessions;
  - focus: list (or tab bar) / main session / terminal panel / dialog, zoom, layout (`layout::SessionLayout`).
- `sessions.rs`: pipelines over `SessionApi`: create, rename, delete, kill, revive, touch; attach,
  detach, promote and demote; applying `EventEnvelope`s to the list; re-listing after a reconnect;
  triggering AI naming (`crate::naming`) when a generated-name session first goes idle or done.
- `handlers/`: key handling by target (`Focus::List` = Rakumux mode, `Focus::Session` = main
  session, `Focus::Terminal` = utility shell):
  - Rakumux mode (`handlers/list.rs`): arrows and Vim keys (`j`/`k`, `gg`/`G`, `Ctrl-d`/`u`,
    `{`/`}` groups; ←/→, `h`/`l` on the tabs row switch tabs; `f` the feed: shown with the keys,
    the keys, hidden; `feed_key` while it has them),
    `gg` via `App::pending_key`,
    `d`/`dd` opens delete confirmation, `b` hides or shows the list, Enter/`i`
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
  - dialogs: the Folders modal (`AppMode::Folders`, `handlers/folders.rs`), the App and Status
    modals (`AppMode::Sections`, `handlers/sections.rs`), rename, delete confirmation, upgrade, daemon-restart consent,
    and the Settings and Help modals (`AppMode::Modal`, `help::ModalState::handle_key`; `,` or `o`
    opens Settings, `?` Help, in Rakumux mode). Settings runs the selected
    `help::KeyAction` (`handlers/settings.rs::run_action`). The welcome tour (`AppMode::Welcome(WelcomeState)`,
    `handlers/dialogs.rs::handle_welcome_key`, `components/welcome.rs`) opens from `main.rs` while
    `sessions.welcome_seen` is false, and from `t` in Help (`ModalAction::Tour`) or Settings;
    `App::close_welcome` saves its "Don't show this on startup" box (on every card).
  - Phone access (`phone_access.rs`, over `crate::remote_access::PhoneAccess`, a fake
    `FakePhone` in tests): `App::start` checks it in a job (`AppEvent::PhoneChecked`); the
    one-time offer (`AppMode::PhoneOffer`) waits until Rakumux mode has no dialog
    (`App::offer_phone_access`, also on every tick) and saves `sessions.phone_offer_seen` as it
    opens; the pairing dialog (`AppMode::Pairing`) turns it on, shows the QR code and turns it off
    (`AppEvent::PhonePairing`, `PhoneOff`). Settings has no phone row (`rakumux remote`).
  - The account (`account.rs`, over `crate::account::AccountAccess`, a fake `FakeAccount` in
    tests): checked once the daemon connects (`AppEvent::AccountChecked`), which starts the
    service only for a saved account. Settings → Account opens `AppMode::Account`: Enter asks for
    a code (`AppEvent::SignIn`), its status is polled every second, Esc abandons it, `o` signs
    out. Signed in with AI session titles on (`App::titles_on`), the service names sessions
    itself; the visible live sessions' status is read every 2 s (`AppEvent::Statuses`,
    generation-checked) for which have a title being made (`App::title_refreshing`). `retitle`
    asks for a new one (`AccountAccess::summarize`, `AppEvent::TitleAsked`): the ↻ that
    hovering a live row shows right after its name (`Hit::TitleRefresh`, a 2-column slot kept
    after the name so it never moves the text, a spinner while one is made) or `s`
    (`retitle_selected`; a footer message when signed out or titles are off). Rows show no
    description or last message. Local AI naming (`auto_name.rs`) waits while titles are on.
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
- Shortcut text shown to the user (footer buttons, Settings, Help) is generated from the
  configured keys.
- Errors from the daemon are `ApiError { code, message }`; show the message, branch on the code.

## When to edit what

- New key in list or session focus: `handlers/` → a pipeline in `sessions.rs` (or a dialog) → state.
- Session operations and event handling: `sessions.rs`.
- New config option: `config.rs` (serde default so older files still load) + a `SettingsItem` row
  in the Settings modal (`App::modal_context`, `handlers/settings.rs`).
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
- Cover list ordering and filtering (text, folder, app and status, per tab), event application
  (a status change moving a session, the selected one kept), key routing (prefix, cycle keys, promote/
  demote, zoom, nesting guard), dialogs, and ratatui `TestBackend` renders at 80×24 and 160×48 for
  empty, live, exited and dead states in both layouts, at phone sizes (45×40, 40×20) for the
  list-then-session layout, taps and dialogs, and at narrow-window sizes (90×40, 90×20) for the
  stacked layout.
