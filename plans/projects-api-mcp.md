# Projects, Pinning, Git Context, External API and MCP

Status: accepted (rev 3: OpenCode 2.x identity through conversation bindings) · Date: 2026-10-13 · Branch: `sessions-refactor` · Extends
`plans/sessions-refactor.md` (rev 7). Supersedes, where they conflict: the single-folder scope of
`sessions-refactor.md` §4, the "no git" rule for read-only display, and `plans/agent-mcp.md` §3.1/§3.4
(MCP inside the `tui` binary, no HTTP).

## Requirements (from the user, 2026-10-13)

1. **Projects.** A project is a directory Rakumux works in: a git repository, a git worktree or any
   folder. Sessions still belong to exactly one directory, now called its **project**; a project
   holds many sessions. The session list gets a **drop-down project selector at the top**. The user
   can select **one or more projects** and sees the sessions of every selected project, so sessions
   across several repositories can be managed from one window.
2. **Pinning.** Sessions can be pinned in the list.
3. **Git context in the list.** Each session row's second line shows the session's **git branch** and,
   when the session is in a linked worktree, the **worktree**. No directory (the project name already
   says where it is).
4. **External API.** A folder `api/` at the repository root: a service exposing HTTP endpoints for
   interacting with sessions on a running Rakumux server (the daemon).
5. **MCP.** A folder `mcp/` at the repository root: an MCP server that coding agents can install to
   interact with sessions and projects. It drives the external API.
6. First API/MCP function set:
   - query sessions by id and by name;
   - create sessions;
   - read a session's content;
   - rename a session.

## Decisions

| Topic | Decision |
|---|---|
| Project identity | The canonical directory path, i.e. today's `ScopeRoot`. The wire keeps the `scope` field names (protocol stays v1). The product, help, API and MCP say "project". |
| Which projects exist | A daemon-side **project registry** (`projects` table: path, added_at, last_used_at). A project is added when a session is created in it, when the TUI starts in it, or explicitly (`ProjectAdd`). `ProjectRemove` forgets a project that has no sessions. |
| Selection | TUI state, saved as `sessions.selected_projects` in `~/.rakumux/config.json`. The folder Rakumux starts in is always added to the selection at startup. |
| List with several projects | Sessions are grouped under project header rows, which can't be selected. Inside a group: pinned first, then the layout's usual order. With one project selected there's no header. |
| Selector | The project item, the first row of the Sidebar list (a padded band; or the left end of the Tabs bar): `▾ orchestrator` / `▾ 3 projects`. Enter on it, `P` or a click opens the project switcher, a modal: typing filters the projects (name, folder, branch), Enter switches to the one under the cursor, Tab ticks several and Enter then shows the ticked ones; a typed folder path (`/`, `~`, `.`) that isn't a project is offered as "+ Add" (or "+ Create folder", confirmed with `y`), and Enter adds it and shows it; `Ctrl-x` forgets an empty project; Esc closes. |
| New sessions | The new-session dialog gets a **Project** field. It defaults to the selected session's project, or the only selected project. (Later removed: the dialog asks only for a name and shows the current project under it; see `sessions-refactor.md` §8.) |
| Pinning | `SessionView.pinned`, stored in the registry. New op `SessionPin { id, pinned }`. `p` toggles a pin in the TUI. Pinned sessions show a mark and sort first in their group. |
| Git context | The daemon computes `SessionView.git: Option<GitInfo>` from the session's live cwd by **reading `.git` files** (no `git` binary, no git commands). It walks up to `.git`. A directory is the main worktree; a `gitdir:` file pointing into `…/worktrees/<name>` is a linked worktree. HEAD gives the branch, or a short SHA when detached. The daemon recomputes on cwd change and on its periodic poll (HEAD changes on checkout), and sends `Updated` when the value changes. |
| Subtext | `⎇ branch · worktree <name> · agent/program · state`. The reported text follows when there's room. The cwd is no longer shown. |
| Agents and projects | Session callers (agents) stay confined to **their own project**, as before. Only user callers (the TUI, the CLI, the API without a session identity) see several projects. |
| OpenCode 2.x identity | (rev 3, 2026-10-13; `agent-mcp.md` N6 brought forward to 0.6.0) OpenCode 2.x runs MCP servers in a service shared by every conversation, so tool calls carry only `_meta["ai.opencode/sessionID"]`. An OpenCode TUI plugin (`mcp/opencode/rakumux-session/tui.js`, a directory under `<config>/plugins/`, which 2.x loads without config) runs inside the Rakumux session and calls `rakumux session bind --agent opencode <ids>` for the conversation it shows and its subagents. New op `SessionBind { agent, conversations }` (session callers, binds to self; memory only; newest wins; cleared on exit) and `Auth::Delegated { token, agent, conversation }` (user token, resolved to the running session that bound it; capability `auth.delegated`). The API takes `X-Rakumux-Agent` + `X-Rakumux-Agent-Conversation` (401 `details.agent_conversation: unbound`, 501 `unsupported`). The MCP sends a tagged call as the conversation, ignoring any env identity; when unbound it acts as the user, refusing changes if an env identity is set. OpenCode 1.x needs nothing: its MCP servers run in each TUI with the session's env. |
| Agents creating sessions | (rev 2, user decision 2026-10-13) Session callers **may create sessions in their own project**. Naming another project is `out_of_scope`; so is a `cwd` outside it. The daemon enforces `control::Limits` on these creates only (`limit_exceeded`, `details: {limit, max}`): 4 running children per session, depth 2, 8 running agent-created sessions per project, 10 creates per session per minute. `rakumux new` inside a session needs `--detach` (sessions can't attach). Everything else on other sessions stays `permission_denied` until stage B. |
| External API | `api/`: Rust crate `rakumux-api` (binary). HTTP/1.1 + JSON on **127.0.0.1 only**. It is a thin adapter over `session_core::client`, and the daemon still decides everything. It is a workspace member through `package.workspace = "../tui"`. |
| API discovery and auth | When the API starts it writes `<RAKUMUX_HOME or ~/.rakumux>/api.json` (`{url, token, pid, version}`, mode 0600). Every request needs `Authorization: Bearer <token>`. `Host` must be loopback, and requests with a browser `Origin` are refused. |
| Identity through the API | Optional headers `X-Rakumux-Session-Id` / `X-Rakumux-Session-Token`. With them, the API opens a daemon client **as that session**, so the daemon's agent policy and scope confinement apply. Without them it acts as the user. |
| API lifetime | `rakumux-api` runs in the foreground, or with `--detach`. The MCP starts it on demand, the way the CLI starts the daemon. |
| MCP | `mcp/`: Rust crate `rakumux-mcp` (binary). stdio transport, built on `rmcp` pinned to an exact version. It calls the HTTP API with `reqwest`. It forwards `RAKUMUX_SESSION_ID`/`RAKUMUX_SESSION_TOKEN` from its environment as the identity headers. Terminal text goes back inside a labelled, nonce-delimited "untrusted output" block (`agent-mcp.md` §4.8). |
| Shipping | `build.sh`, the release tarballs, npm and Homebrew ship `rakumux-api` and `rakumux-mcp` next to `tui` and `rakumux-ptyd`. |

## Protocol additions (session-core, additive, v1)

Capabilities advertised in `HelloOk.capabilities`: `projects`, `session.pin`, `session.git`.

- `model::GitInfo { repo_root, branch: Option, detached_at: Option, worktree: Option }`.
- `model::ProjectView { path, name, sessions, live, last_active_at: Option, added_at, git: Option<GitInfo> }`.
- `SessionView.pinned: bool`, `SessionView.git: Option<GitInfo>`.
- `Request::SessionList { scope, scopes: Vec<ScopeRoot>, include_dead }`. For user callers the result
  is the union of `scope` and `scopes`; session callers get their own project.
- `SubscribeRequest.scopes: Vec<ScopeRoot>`, with the same union.
- `Request::ProjectList` → `Response::Projects { projects }` (user callers: all projects; session
  callers: their own).
- `Request::ProjectAdd { path }` → `Response::Project { project }` (user only; must be a directory).
- `Request::ProjectRemove { path }` → `Ok` (user only; fails with `invalid_argument` while sessions
  exist).
- `Request::SessionPin { id, pinned }` → `Response::Session` (user callers, or a session pinning
  itself).
- `SessionApi`: `list_in(scopes, include_dead)`, `projects()`, `add_project(path)`,
  `remove_project(path)`, `pin(id, pinned)`.

## External API v1 (`api/openapi.yaml` is the contract)

All routes are under `/v1`. Errors are `{"error": {"code", "message"}}`, with the daemon's `ErrorCode`
mapped to an HTTP status:

| Code | Status |
|---|---|
| `not_found` | 404 |
| `invalid_argument` | 400 |
| `unauthenticated` | 401 |
| `out_of_scope`, `permission_denied` | 403 |
| `session_not_live`, `session_in_use` | 409 |
| `limit_exceeded` | 429 |
| `timeout` | 504 |
| `daemon_unavailable` | 503 |
| `unsupported` | 501 |
| anything else | 500 |

| Method and path | Purpose |
|---|---|
| `GET /v1/health` | Version and daemon reachability. This is the only route without auth. |
| `GET /v1/projects` | The projects the caller can see. |
| `GET /v1/sessions?project=&name=&q=&include_dead=` | List sessions. `project` may repeat; when omitted, user callers get every project and agents get their own. `name` is an exact match, `q` a case-insensitive substring. |
| `GET /v1/sessions/{id}` | One session by id. |
| `POST /v1/sessions` | Create: `{project, name?, cwd?, command?: [argv], startup_commands?: [..]}` → 201. |
| `GET /v1/sessions/{id}/content?source=screen\|scrollback&lines=&format=text\|ansi` | Read the terminal (`ReadResult`). |
| `PATCH /v1/sessions/{id}` | `{name}` → rename. More fields later. |

MCP tools v1:

- `list_projects`;
- `list_sessions` (project?, name?, query?, include_dead?);
- `get_session` (id **or** name; a name matching several sessions returns them all);
- `create_session`;
- `read_session` (id or name, source, lines);
- `rename_session`.

Suggested next tools, in rough order of usefulness to a coding agent:

1. `send_input` / `submit_prompt`: type into a session, or hand a prompt to an agent running in it.
2. `wait_for`: block until a session is idle, blocked, done or exited, or until its output matches a
   pattern (capped at 50 s and re-callable).
3. `report_status`: the calling agent reports its state, summary and files.
4. `kill_session` / `restart_session` / `delete_session`.
5. `pin_session` and `focus_session` (show a session in the user's TUI).
6. `poll_events` (or SSE on the API): what changed since a cursor.
7. `update_session`: description, notes, labels.
8. `spawn_agent`: create a session that runs a named agent with a first prompt and returns its id.
9. `get_git_context`: branch and worktree of a session or project.

## Work packages (parallel, coordinated as in `parallel-execution.md`)

- **K (coordinator, first):** the protocol additions above, compiling across the workspace. New ops
  answer `unsupported` in the daemon until **D** lands, and the fakes gain the new methods.
- **D (daemon + CLI):**
  - registry: `projects` table, `pinned` column, a migration that keeps existing rows;
  - the new ops and multi-project listing and subscriptions;
  - git detection and `Updated` events;
  - policy for session callers;
  - CLI: `rakumux projects [add|rm]`, `rakumux ls --project <dir>… | --all`,
    `rakumux pin|unpin <session>`;
  - tests.
- **T (TUI):**
  - the project selector and drop-down, multi-select, grouped list;
  - pinning;
  - git subtext;
  - the Project field in the new-session dialog;
  - the header, help, footer and config;
  - tests and headless screenshots.
- **A (API + MCP):**
  - `api/` crate, `api/openapi.yaml`, `mcp/` crate, a README for each with install lines for Claude
    Code, Codex and OpenCode;
  - build and release scripts;
  - tests: the API against a real daemon in a temporary `RAKUMUX_HOME`, the MCP against a stub API.

## Open questions

- Should agents be able to see other projects when the user allows it, for example through a
  per-project "share with agents" switch? Not in v1.
- Remember the selection per launch folder instead of globally? v1 remembers it globally and always
  adds the launch folder.
