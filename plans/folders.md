# Folders: every session, filtered by folder (rev 1)

Replaces the user-facing concept "project" with "folder". Supersedes the project parts of
`plans/projects-api-mcp.md` (selection, switcher, naming); the API/MCP design there still holds.

## 1. Why

People run agents in several worktrees of one repository, each its own folder. With projects the
TUI showed the launch folder's sessions and you ticked other projects to see theirs, so sessions
in a new worktree were easy to miss. A session already belongs to the folder it was started in
(its scope); the product should simply say "folder", show every session by default and let you
narrow the list by folder.

## 2. Decisions

1. **Language.** "Folder" replaces "project" everywhere a person or an agent reads it: TUI, Help,
   welcome tour, README, website, CLI output and help, API and MCP descriptions. A session's
   folder is the directory it was started in. The daemon protocol keeps its names (`scope`,
   `ScopeRoot`, `project_list` / `project_add` / `project_remove`, `ProjectView`, capability
   `projects`) so a running daemon keeps working; docs map them ("folders; `project_*` on the
   wire").
2. **All folders by default.** The session list shows every session the daemon has (live, exited,
   dead), grouped under a header row per folder when more than one folder is shown. The launch
   folder's group comes first, then the stable group order of the session list (no reordering by
   activity).
3. **The top item is a folder filter**: `▤ All folders ▾`, `▤ <folder> ▾` or `▤ 3 folders ▾`
   (Tabs: the same selector at the left of the tab bar). `Enter` on it, `P` or a click opens the
   Folders modal.
4. **Folders modal** (the project switcher, reworked):
   - First row **All folders** (ticked when there's no filter), then every folder that has
     sessions: name, path, git branch, live/total. Folders without sessions aren't listed unless
     they're in the current filter.
   - Type to filter the rows. `Enter` on a folder shows just that folder; `Tab`/`Space` tick
     several and `Enter` shows the ticked ones; `Enter` on All folders clears the filter.
   - A typed path offers `+ Add folder …` with the folder autocomplete (`app/path_complete.rs`);
     choosing it shows that (empty) folder so new sessions can start there.
   - `Ctrl-x` (forget) goes; `orchestra folders rm` keeps it on the command line.
5. **The filter isn't saved.** Every start shows All folders. `sessions.selected_projects` in
   config.json is ignored and dropped on the next save.
6. **New sessions** go in the current folder: the selected session's folder; on a folder header,
   that folder; else the filter's only folder; else the launch folder. The dialog shows it
   (`in ▤ folder`).
7. **Agents don't change**: a session caller sees only its own folder.
8. **Daemon, additive**: `all: bool` on `session_list` and `subscribe` (user callers only;
   session callers get `out_of_scope`), capability `sessions.all`, client method
   `SessionApi::list_all`. The TUI lists and subscribes with `all` so sessions in folders it has
   never seen (a new worktree, a session an agent started) appear live. With an older daemon the
   TUI uses `project_list` plus `list_in`/`subscribe(scopes)` and offers the usual restart notice.
9. **CLI**: `orchestra folders [--json]`, `orchestra folders add|rm <dir>`, `orchestra ls
   --folder DIR`; `projects` and `--project` stay as hidden aliases. `orchestra ls` still defaults
   to the current folder (`--all` for every folder). Output says "folder".
10. **API and MCP** (unreleased, so renamed now, with aliases where cheap):
    - MCP tool `list_folders` replaces `list_projects`.
    - `folder` replaces the `project` parameter and field in tool input and output; `project` is
      still accepted as input.
    - API `GET /v1/folders`; `/v1/projects` stays as an alias.
    - The `folder` query and body fields accept `project` too. OpenAPI is updated and the
      descriptions say folder.

## 3. Work packages

| | Package | Owner | Files |
|---|---|---|---|
| A | Contract: `all` on `SessionList`/`SubscribeRequest`, `SessionApi::list_all`, capability name | coordinator, on main first | `session-core` |
| B | Daemon: `all` for user callers, `out_of_scope` for sessions, capability, tests | agent | `ptyd/src/{server,control}` |
| C | TUI: filter model, Folders modal, list and top item, new-session rule, config, Help, welcome tour, tests; rename `projects` code to `folders` (modules, types, hits) | agent, after the drag-reorder work is merged | `tui/src` (not `cli/`) |
| D | CLI, API, MCP naming and aliases, OpenAPI, `api/README.md`, `mcp/README.md` | agent | `tui/src/cli`, `api/`, `mcp/` |
| E | Root docs: AGENTS.md, tui/README.md, landing, manual tests, this plan's status | C owns AGENTS.md and tui/README.md; D notes its parts; coordinator does landing and the final pass | |

B and D can start once A is on main. C starts after the drag-reorder merge (it reworks the same
list code).

## 4. Testing

- Daemon: `all` lists and streams sessions of every scope, session callers are refused, older
  requests (no `all`) behave as before.
- TUI: All folders at start with group headers; the modal's All folders, single folder, several
  folders, typed path; new-session folder rule; an older daemon without `sessions.all`.
- CLI/API/MCP: `folders`, `--folder`, aliases, `list_folders`, `project` accepted as input.
- Manual: two worktrees of one repository with sessions in each; both show under All folders; the
  filter narrows; a session started by another client in a third folder appears live.

## 5. Status

- rev 1 (2026-10-16): plan.
- [x] B (2026-10-16): the daemon lists and streams every scope with `all` for user callers
  (subscriptions match any scope, so scopes created later are included), refuses session callers
  with `out_of_scope` (audited), and announces `sessions.all`.
- [x] C (2026-10-16): the TUI lists and subscribes with `all` (older daemons: `project_list` then
  `list_in`/`subscribe(scopes)`, and the restart notice names "new folders live") and filters by
  folder locally: the folder filter (`▤ All folders ▾`, `▤ <folder> ▾`, `▤ 3 folders ▾`, not
  saved; `sessions.selected_projects` is dropped on the next save) and the Folders modal (All
  folders, folders with sessions, type to filter, Enter shows one, Tab/Space tick several, a
  typed path adds a folder and shows it; `Ctrl-x` forget is gone). Group headers are ordered
  launch folder, then name, then path, and are selectable rows: on a header, `n` and Enter start
  a session in that folder. With ticks, Enter on an unticked folder shows the ticked folders and
  that one. A session created in, or returned to in, a folder the filter hides adds that folder to
  the filter. TUI code says folder (`app/folders.rs`, `FolderPicker`, `Selection::Folders`,
  `Selection::FolderHeader`, `Hit::FolderRow`, `KeyAction::NextFolder`/`FilterFolders`), and so do
  Help, the Keys tab, the welcome tour, `tui/README.md`, `AGENTS.md` and the manual tests.
