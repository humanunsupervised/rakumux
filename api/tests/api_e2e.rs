//! The real `rakumux-api` binary against a real `rakumux-ptyd`, both in
//! a temporary `RAKUMUX_HOME`. The API spawns the daemon on the first user
//! request (as the CLI does); every test stops both.

mod support;

use rakumux_api::discovery::{self, ApiInfo};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::process::Stdio;
use support::*;

/// Create a bash session in `folder` and return its JSON.
fn create(api: &Api, folder: &std::path::Path, name: &str, startup: &[&str]) -> Value {
    let reply = api.call(
        "POST",
        "/v1/sessions",
        Some(json!({
            "folder": folder,
            "name": name,
            "command": bash(),
            "startup_commands": startup,
        })),
    );
    assert_eq!(reply.status, 201, "create failed: {:?}", reply.body);
    reply.body["session"].clone()
}

/// Poll the session's screen until it contains `needle`.
fn wait_for_text(api: &Api, id: &str, needle: &str) -> String {
    let mut text = String::new();
    let found = wait_until(TIMEOUT, || {
        let reply = api.call("GET", &format!("/v1/sessions/{id}/content"), None);
        text = reply.body["text"].as_str().unwrap_or("").to_owned();
        text.contains(needle)
    });
    assert!(found, "never saw {needle:?}; screen:\n{text}");
    text
}

#[test]
fn requests_are_checked_before_they_reach_the_daemon() {
    let api = Api::start();

    // Health needs no token and never starts the daemon.
    let health = api.raw("GET", "/v1/health", &[], None);
    assert_eq!(health.status, 200);
    assert_eq!(health.body["status"], "ok");
    assert_eq!(health.body["pid"], api.info.pid);
    assert_eq!(health.body["daemon"]["reachable"], false);

    // No token, a wrong token, a wrong scheme.
    let reply = api.raw("GET", "/v1/sessions", &[], None);
    assert_eq!(reply.status, 401);
    assert_eq!(reply.code(), "unauthenticated");
    assert_eq!(reply.header("www-authenticate"), Some("Bearer"));
    let reply = api.raw(
        "GET",
        "/v1/sessions",
        &[("Authorization", "Bearer nope")],
        None,
    );
    assert_eq!(reply.status, 401);
    let basic = format!("Basic {}", api.info.token);
    let reply = api.raw("GET", "/v1/folders", &[("Authorization", &basic)], None);
    assert_eq!(reply.status, 401);
    let reply = api.raw("GET", "/v1/projects", &[], None);
    assert_eq!(reply.status, 401);
    // Unknown routes are not revealed before authentication.
    let reply = api.raw("GET", "/v1/secret", &[], None);
    assert_eq!(reply.status, 401);

    // A non-loopback Host (DNS rebinding) or a browser Origin is refused,
    // even with the right token and on the health route.
    let bearer = format!("Bearer {}", api.info.token);
    let reply = api.raw(
        "GET",
        "/v1/sessions",
        &[("Authorization", &bearer), ("Host", "evil.example:80")],
        None,
    );
    assert_eq!(reply.status, 403);
    assert_eq!(reply.code(), "permission_denied");
    let reply = api.raw("GET", "/v1/health", &[("Host", "attacker.test")], None);
    assert_eq!(reply.status, 403);
    let reply = api.raw(
        "GET",
        "/v1/sessions",
        &[
            ("Authorization", &bearer),
            ("Origin", "http://localhost:3000"),
        ],
        None,
    );
    assert_eq!(reply.status, 403);
    assert!(reply.header("access-control-allow-origin").is_none());
    let reply = api.raw("GET", "/v1/health", &[("Origin", "null")], None);
    assert_eq!(reply.status, 403);
    let reply = api.raw(
        "GET",
        "/v1/health",
        &[(
            "Host",
            &format!("localhost:{}", api.addr().rsplit(':').next().unwrap()),
        )],
        None,
    );
    assert_eq!(reply.status, 200);

    // Authenticated: unknown route, wrong method, half an identity.
    let reply = api.call("GET", "/v1/secret", None);
    assert_eq!(reply.status, 404);
    assert_eq!(reply.code(), "not_found");
    let reply = api.call("DELETE", "/v1/sessions", None);
    assert_eq!(reply.status, 405);
    assert_eq!(reply.header("allow"), Some("GET, POST"));
    let reply = api.raw(
        "GET",
        "/v1/sessions",
        &[("Authorization", &bearer), ("X-Rakumux-Session-Id", "01JX")],
        None,
    );
    assert_eq!(reply.status, 400);
    assert_eq!(reply.code(), "invalid_argument");
    let reply = api.call("GET", "/v1/sessions?nme=x", None);
    assert_eq!(reply.status, 400);

    // A user request starts the daemon; health then sees it.
    let folder = format!(
        "/v1/sessions?folder={}",
        enc(&api.env.project.display().to_string())
    );
    let reply = api.call("GET", &folder, None);
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    assert_eq!(reply.body, json!({"sessions": []}));
    let health = api.raw("GET", "/v1/health", &[], None);
    assert_eq!(health.body["daemon"]["reachable"], true);

    // A session identity the daemon doesn't know.
    let reply = api.call_as(
        "01JUNKNOWN0000000000000000",
        "bad",
        "GET",
        "/v1/sessions",
        None,
    );
    assert_eq!(reply.status, 401);
    assert_eq!(reply.code(), "unauthenticated");
}

#[test]
fn sessions_can_be_created_found_read_and_renamed() {
    let api = Api::start();
    let project = api.env.project.clone();

    let session = create(&api, &project, "api-e2e", &["echo API_E2E_$((40+2))"]);
    let id = session["id"].as_str().unwrap().to_owned();
    assert_eq!(session["name"], "api-e2e");
    assert_eq!(session["folder"], json!(project));
    assert_eq!(session["project"], json!(project), "the deprecated alias");
    assert_eq!(session["status"], "live");
    assert_eq!(session["command"], bash());
    assert_eq!(session["created_by"], json!({"kind": "user"}));
    assert!(session["created_at"].as_str().unwrap().ends_with('Z'));

    // Location header, and a second session in a subdirectory, created
    // with the deprecated `project` field.
    let reply = api.call(
        "POST",
        "/v1/sessions",
        Some(json!({"project": project, "name": "Second Shell", "cwd": "sub", "command": bash()})),
    );
    assert_eq!(reply.status, 201);
    let second = reply.body["session"].clone();
    assert_eq!(
        reply.header("location"),
        Some(format!("/v1/sessions/{}", second["id"].as_str().unwrap()).as_str())
    );
    assert_eq!(second["cwd"], json!(project.join("sub")));

    // Create errors map onto statuses.
    let reply = api.call(
        "POST",
        "/v1/sessions",
        Some(json!({"name": "x", "command": bash()})),
    );
    assert_eq!(reply.status, 400);
    assert!(
        reply.message().contains("folder is required"),
        "{}",
        reply.message()
    );
    assert_eq!(reply.body["error"]["details"]["missing"], "folder");
    let missing = project.join("missing");
    let reply = api.call("POST", "/v1/sessions", Some(json!({"folder": missing})));
    assert_eq!(reply.status, 404);
    let reply = api.call(
        "POST",
        "/v1/sessions",
        Some(json!({"folder": project, "name": "api-e2e", "command": bash()})),
    );
    assert_eq!(reply.status, 400, "duplicate names are refused");
    let reply = api.call(
        "POST",
        "/v1/sessions",
        Some(json!({"folder": project, "cwd": "/", "command": bash()})),
    );
    assert_eq!(reply.status, 403, "cwd outside the folder");
    assert_eq!(reply.code(), "out_of_scope");
    let reply = api.call(
        "POST",
        "/v1/sessions",
        Some(json!({"folder": project, "argv": ["x"]})),
    );
    assert_eq!(reply.status, 400, "unknown body fields are refused");
    let bearer = format!("Bearer {}", api.info.token);
    let reply = api.raw(
        "POST",
        "/v1/sessions",
        &[("Authorization", &bearer), ("Content-Type", "text/plain")],
        Some("{}"),
    );
    assert_eq!(reply.status, 415);

    // List, filter by exact name and by case-insensitive substring.
    let base = format!(
        "/v1/sessions?folder={}",
        enc(&project.display().to_string())
    );
    let all = api.call("GET", &base, None);
    assert_eq!(all.status, 200);
    assert_eq!(all.body["sessions"].as_array().unwrap().len(), 2);
    let by_name = api.call("GET", &format!("{base}&name=api-e2e"), None);
    let names: Vec<&str> = by_name.body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["api-e2e"]);
    let by_q = api.call("GET", &format!("{base}&q=SECOND"), None);
    assert_eq!(by_q.body["sessions"][0]["name"], "Second Shell");
    assert_eq!(by_q.body["sessions"].as_array().unwrap().len(), 1);
    let none = api.call("GET", &format!("{base}&name=API-E2E"), None);
    assert_eq!(none.body["sessions"], json!([]), "name is an exact match");

    // Get by id; unknown ids are 404.
    let reply = api.call("GET", &format!("/v1/sessions/{id}"), None);
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body["session"]["id"], id.as_str());
    let reply = api.call("GET", "/v1/sessions/01JNOSUCHSESSION0000000000", None);
    assert_eq!(reply.status, 404);
    assert_eq!(reply.code(), "not_found");
    let reply = api.call("GET", "/v1/sessions/..%2F..%2Fetc", None);
    assert_eq!(reply.status, 404);

    // Read the screen and the scrollback.
    let text = wait_for_text(&api, &id, "API_E2E_42");
    assert!(!text.contains('\u{1b}'), "text format has no escapes");
    let reply = api.call(
        "GET",
        &format!("/v1/sessions/{id}/content?source=scrollback&lines=3"),
        None,
    );
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body["source"], "scrollback");
    assert_eq!(reply.body["untrusted"], true);
    assert!(reply.body["text"].as_str().unwrap().lines().count() <= 3);
    assert!(reply.body["rows"].as_u64().unwrap() > 0);
    let reply = api.call(
        "GET",
        &format!("/v1/sessions/{id}/content?format=ansi"),
        None,
    );
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body["format"], "ansi");
    let reply = api.call(
        "GET",
        &format!("/v1/sessions/{id}/content?source=all"),
        None,
    );
    assert_eq!(reply.status, 400);
    let reply = api.call(
        "GET",
        "/v1/sessions/01JNOSUCHSESSION0000000000/content",
        None,
    );
    assert_eq!(reply.status, 404);

    // Rename.
    let reply = api.call(
        "PATCH",
        &format!("/v1/sessions/{id}"),
        Some(json!({"name": "renamed"})),
    );
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    assert_eq!(reply.body["session"]["name"], "renamed");
    let reply = api.call(
        "PATCH",
        &format!("/v1/sessions/{id}"),
        Some(json!({"name": "Second Shell"})),
    );
    assert_eq!(reply.status, 400, "names stay unique in a folder");
    let reply = api.call("PATCH", &format!("/v1/sessions/{id}"), Some(json!({})));
    assert_eq!(reply.status, 400);
    let reply = api.call("PATCH", &format!("/v1/sessions/{id}"), None);
    assert_eq!(reply.status, 415);
    let reply = api.call("GET", &format!("{base}&name=renamed"), None);
    assert_eq!(reply.body["sessions"][0]["id"], id.as_str());
}

#[test]
fn folders_are_listed_and_lists_span_folders() {
    let api = Api::start();
    let first = create(&api, &api.env.project, "one", &[]);
    let second = create(&api, &api.env.other_project, "two", &[]);

    let folders = api.call("GET", "/v1/folders", None);
    let all = api.call("GET", "/v1/sessions", None);
    let health = api.raw("GET", "/v1/health", &[], None);
    let capabilities = health.body["daemon"]["capabilities"].as_array().unwrap();
    if !capabilities.iter().any(|c| c == "projects") {
        // An older daemon: /v1/folders is 501 and a user list without a
        // folder asks for one.
        assert_eq!(folders.status, 501);
        assert_eq!(folders.code(), "unsupported");
        assert_eq!(api.call("GET", "/v1/projects", None).status, 501);
        assert_eq!(all.status, 400);
        assert!(
            all.message().contains("folder is required"),
            "{}",
            all.message()
        );
        assert_eq!(all.body["error"]["details"]["missing"], "folder");
        let reply = api.call("GET", "/v1/sessions?folder=project", None);
        assert_eq!(reply.status, 400, "folder names need the registry");
        return;
    }

    // Every folder with sessions, with counts.
    assert_eq!(folders.status, 200, "{:?}", folders.body);
    let listed = folders.body["folders"].as_array().unwrap();
    let find = |path: &std::path::Path| {
        listed
            .iter()
            .find(|f| f["path"] == json!(path))
            .unwrap_or_else(|| panic!("{} not listed: {listed:?}", path.display()))
    };
    let folder = find(&api.env.project);
    assert_eq!(folder["name"], "project");
    assert_eq!(folder["sessions"], 1);
    assert_eq!(folder["live"], 1);
    assert!(folder["added_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(find(&api.env.other_project)["name"], "other");
    // `projects`: the same list under its deprecated name.
    assert_eq!(folders.body["projects"], folders.body["folders"]);
    // `/v1/projects` is an alias of `/v1/folders`.
    let alias = api.call("GET", "/v1/projects", None);
    assert_eq!(alias.status, 200, "{:?}", alias.body);
    assert_eq!(alias.body, folders.body);
    let reply = api.call("DELETE", "/v1/folders", None);
    assert_eq!(reply.status, 405);

    // Without a folder, a user lists every folder.
    assert_eq!(all.status, 200, "{:?}", all.body);
    let names: Vec<&str> = all.body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"one") && names.contains(&"two"),
        "{names:?}"
    );
    let reply = api.call("GET", "/v1/sessions?name=two", None);
    assert_eq!(reply.body["sessions"][0]["id"], second["id"]);

    // A folder can be named instead of spelled out, with `folder` or its
    // deprecated alias `project`.
    for key in ["folder", "project"] {
        let reply = api.call("GET", &format!("/v1/sessions?{key}=other"), None);
        assert_eq!(reply.status, 200, "{key}: {:?}", reply.body);
        assert_eq!(reply.body["sessions"].as_array().unwrap().len(), 1);
        assert_eq!(reply.body["sessions"][0]["id"], second["id"]);
    }
    let reply = api.call("GET", "/v1/sessions?folder=no-such-folder", None);
    assert_eq!(reply.status, 404);
    assert!(
        reply.message().contains("no folder named"),
        "{}",
        reply.message()
    );
    // Both: the same folder is fine, different folders are a 400.
    let reply = api.call("GET", "/v1/sessions?folder=other&project=other", None);
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    assert_eq!(reply.body["sessions"].as_array().unwrap().len(), 1);
    let reply = api.call("GET", "/v1/sessions?folder=other&project=project", None);
    assert_eq!(reply.status, 400, "{:?}", reply.body);
    assert_eq!(reply.code(), "invalid_argument");
    let reply = api.call(
        "POST",
        "/v1/sessions",
        Some(json!({"folder": "other", "project": "project", "command": bash()})),
    );
    assert_eq!(reply.status, 400, "{:?}", reply.body);
    assert!(
        reply.message().contains("`folder` and `project`"),
        "{}",
        reply.message()
    );
    let reply = api.call(
        "POST",
        "/v1/sessions",
        Some(json!({"folder": "other", "name": "three", "command": bash()})),
    );
    assert_eq!(reply.status, 201, "{:?}", reply.body);
    assert_eq!(
        reply.body["session"]["folder"],
        json!(api.env.other_project)
    );
    let third = reply.body["session"].clone();
    let reply = api.call("GET", "/v1/folders?x=1", None);
    assert_eq!(reply.status, 400);

    // Several folders: the union, newest activity first.
    let path = format!(
        "/v1/sessions?folder={}&folder={}",
        enc(&api.env.project.display().to_string()),
        enc(&api.env.other_project.display().to_string())
    );
    let reply = api.call("GET", &path, None);
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    let mut ids: Vec<&str> = reply.body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    let times: Vec<&str> = reply.body["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["last_active_at"].as_str().unwrap())
        .collect();
    assert!(times.windows(2).all(|w| w[0] >= w[1]), "{times:?}");
    ids.sort();
    let mut expected = vec![
        first["id"].as_str().unwrap(),
        second["id"].as_str().unwrap(),
        third["id"].as_str().unwrap(),
    ];
    expected.sort();
    assert_eq!(ids, expected);
    let reply = api.call("GET", "/v1/sessions?folder=relative/dir", None);
    assert_eq!(reply.status, 400);
}

#[test]
fn session_identity_applies_the_daemon_policy() {
    let api = Api::start();
    let agent = create(
        &api,
        &api.env.project,
        "agent",
        &["echo TOKEN=$RAKUMUX_SESSION_TOKEN"],
    );
    let other = create(&api, &api.env.project, "other", &[]);
    let elsewhere = create(&api, &api.env.other_project, "elsewhere", &[]);
    let id = agent["id"].as_str().unwrap().to_owned();
    let screen = wait_for_text(&api, &id, "TOKEN=");
    let token = screen
        .lines()
        .filter_map(|line| line.strip_prefix("TOKEN="))
        .find(|token| token.len() == 64)
        .expect("session token on screen")
        .to_owned();

    // Without a folder, a session sees its own folder only.
    let reply = api.call_as(&id, &token, "GET", "/v1/sessions", None);
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    let sessions = reply.body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 2);
    let me = sessions.iter().find(|s| s["id"] == id.as_str()).unwrap();
    assert_eq!(me["is_self"], true);

    // A session sees only its own folder.
    let reply = api.call_as(&id, &token, "GET", "/v1/folders", None);
    if reply.status != 501 {
        assert_eq!(reply.status, 200, "{:?}", reply.body);
        let folders = reply.body["folders"].as_array().unwrap();
        assert_eq!(folders.len(), 1, "{folders:?}");
        assert_eq!(folders[0]["path"], json!(api.env.project));
    }

    // Other folders are out of scope.
    let path = format!(
        "/v1/sessions?folder={}",
        enc(&api.env.other_project.display().to_string())
    );
    let reply = api.call_as(&id, &token, "GET", &path, None);
    assert_eq!(reply.status, 403);
    assert_eq!(reply.code(), "out_of_scope");
    let elsewhere_path = format!("/v1/sessions/{}", elsewhere["id"].as_str().unwrap());
    let reply = api.call_as(&id, &token, "GET", &elsewhere_path, None);
    assert_eq!(reply.status, 403);
    let reply = api.call_as(
        &id,
        &token,
        "POST",
        "/v1/sessions",
        Some(json!({"folder": api.env.other_project, "command": bash()})),
    );
    assert_eq!(reply.status, 403);
    assert_eq!(reply.code(), "out_of_scope");

    // A session may rename itself, not others.
    let reply = api.call_as(
        &id,
        &token,
        "PATCH",
        &format!("/v1/sessions/{id}"),
        Some(json!({"name": "agent-renamed"})),
    );
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    assert_eq!(reply.body["session"]["name"], "agent-renamed");
    let reply = api.call_as(
        &id,
        &token,
        "PATCH",
        &format!("/v1/sessions/{}", other["id"].as_str().unwrap()),
        Some(json!({"name": "hijacked"})),
    );
    assert_eq!(reply.status, 403, "{:?}", reply.body);
    assert_eq!(reply.code(), "permission_denied");

    // A session creates sessions in its own folder, not in another.
    let reply = api.call_as(
        &id,
        &token,
        "POST",
        "/v1/sessions",
        Some(json!({"name": "child", "command": bash()})),
    );
    assert_eq!(reply.status, 201, "{:?}", reply.body);
    assert_eq!(reply.body["session"]["folder"], json!(api.env.project));
    assert_eq!(reply.body["session"]["parent_id"], id.as_str());
    let reply = api.call_as(
        &id,
        &token,
        "POST",
        "/v1/sessions",
        Some(json!({"folder": api.env.project, "name": "sibling", "command": bash()})),
    );
    assert_eq!(reply.status, 201, "{:?}", reply.body);
    let reply = api.call_as(
        &id,
        &token,
        "POST",
        "/v1/sessions",
        Some(json!({"folder": api.env.other_project, "name": "away", "command": bash()})),
    );
    assert_eq!(reply.status, 403, "{:?}", reply.body);
    assert_eq!(reply.code(), "out_of_scope");

    // Reading content as the session works within its folder.
    let reply = api.call_as(
        &id,
        &token,
        "GET",
        &format!("/v1/sessions/{id}/content"),
        None,
    );
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body["untrusted"], true);
}

#[test]
fn agent_conversations_act_as_the_session_that_bound_them() {
    let api = Api::start();
    let agent = create(
        &api,
        &api.env.project,
        "opencode-tui",
        &["echo TOKEN=$RAKUMUX_SESSION_TOKEN"],
    );
    let id = agent["id"].as_str().unwrap().to_owned();
    let screen = wait_for_text(&api, &id, "TOKEN=");
    let token = screen
        .lines()
        .filter_map(|line| line.strip_prefix("TOKEN="))
        .find(|token| token.len() == 64)
        .expect("session token on screen")
        .to_owned();
    let auth = format!("Bearer {}", api.info.token);
    let as_conversation = |conversation: &str, method: &str, path: &str, body: Option<Value>| {
        let body = body.map(|b| b.to_string());
        let mut headers = vec![
            ("Authorization", auth.as_str()),
            ("X-Rakumux-Agent", "opencode"),
            ("X-Rakumux-Agent-Conversation", conversation),
        ];
        if body.is_some() {
            headers.push(("Content-Type", "application/json"));
        }
        api.raw(method, path, &headers, body.as_deref())
    };

    // Nobody bound it yet: 401, marked so the MCP server can fall back.
    let reply = as_conversation("ses_e2e", "GET", "/v1/sessions", None);
    assert_eq!(reply.status, 401, "{:?}", reply.body);
    assert_eq!(
        reply.body["error"]["details"]["agent_conversation"],
        "unbound"
    );

    // The session binds it (as the OpenCode plugin does from its TUI)…
    let client = session_core::client::Client::connect(session_core::client::ClientConfig {
        layout: api.env.layout.clone(),
        client_name: "test".into(),
        version: "test".into(),
        auth: session_core::client::AuthMode::Session {
            id: session_core::ids::SessionId::from_string(id.clone()),
            token: token.clone(),
        },
        spawn: false,
        ptyd_path: None,
    })
    .unwrap();
    use session_core::client::SessionApi;
    client.bind("opencode", &["ses_e2e".to_owned()]).unwrap();

    // …and the conversation's calls act as that session.
    let reply = as_conversation("ses_e2e", "GET", "/v1/sessions", None);
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    let sessions = reply.body["sessions"].as_array().unwrap();
    let me = sessions.iter().find(|s| s["id"] == id.as_str()).unwrap();
    assert_eq!(me["is_self"], true);
    let reply = as_conversation(
        "ses_e2e",
        "POST",
        "/v1/sessions",
        Some(json!({"name": "helper", "command": bash()})),
    );
    assert_eq!(reply.status, 201, "{:?}", reply.body);
    assert_eq!(reply.body["session"]["parent_id"], id.as_str());
    assert_eq!(reply.body["session"]["folder"], json!(api.env.project));
    let reply = as_conversation(
        "ses_e2e",
        "POST",
        "/v1/sessions",
        Some(json!({"folder": api.env.other_project, "name": "away", "command": bash()})),
    );
    assert_eq!(reply.status, 403, "{:?}", reply.body);
}

#[test]
fn api_json_tracks_the_running_api() {
    let env = TestEnv::new();
    // A leftover file from an API that died is replaced.
    let stale = ApiInfo {
        url: "http://127.0.0.1:9".into(),
        token: "stale".into(),
        pid: 999_999,
        version: "0.0.0".into(),
        remote_urls: Vec::new(),
    };
    discovery::write(&env.layout, &stale).unwrap();
    let mut api = Api::start_in(env);
    let path = discovery::info_path(&api.env.layout);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_ne!(api.info.token, "stale");
    assert_eq!(api.info.token.len(), 64);
    assert!(api.info.url.starts_with("http://127.0.0.1:"));
    assert!(api.info.remote_urls.is_empty(), "remote access is off");
    assert_eq!(api.info.version, rakumux_api::VERSION);

    // A second API for the same home refuses to start…
    let output = api
        .env
        .command(std::path::Path::new(API_EXE))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("already running")
            || String::from_utf8_lossy(&output.stderr).contains("another rakumux-api"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // …and --detach returns the running one.
    let output = api
        .env
        .command(std::path::Path::new(API_EXE))
        .arg("--detach")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(&api.info.url));
    assert_eq!(ApiInfo::load(&api.env.layout).unwrap(), api.info);

    // A deleted api.json is published again.
    std::fs::remove_file(&path).unwrap();
    assert!(
        wait_until(TIMEOUT, || path.exists()),
        "api.json not restored"
    );

    // SIGTERM: clean exit, file removed.
    assert_eq!(api.terminate(), Some(0));
    assert!(!path.exists());
}

#[test]
fn detach_starts_a_background_api() {
    let env = TestEnv::new();
    let output = env
        .command(std::path::Path::new(API_EXE))
        .arg("--detach")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let info = ApiInfo::load(&env.layout).expect("api.json after --detach");
    assert!(info.process_alive());
    let reply = http(
        info.url.trim_start_matches("http://"),
        "GET",
        "/v1/health",
        &[],
        None,
    );
    assert_eq!(reply.status, 200);
    assert_eq!(reply.body["pid"], info.pid);
    assert!(discovery::log_path(&env.layout).exists());

    // Stop it (it is not our child).
    // SAFETY: plain kill(2).
    unsafe { libc::kill(info.pid as i32, libc::SIGTERM) };
    assert!(wait_until(TIMEOUT, || !discovery::info_path(&env.layout).exists()));
    env.stop_daemon();
}

// ---------------------------------------------------------------- remote access

/// Poll a session until its status is `status`.
fn wait_for_status(api: &Api, id: &str, status: &str) {
    let mut last = Value::Null;
    let reached = wait_until(TIMEOUT, || {
        let reply = api.call("GET", &format!("/v1/sessions/{id}"), None);
        last = reply.body["session"]["status"].clone();
        last == status
    });
    assert!(reached, "{id} never became {status} (last: {last})");
}

#[test]
fn the_remote_listener_admits_the_remote_token_only() {
    let (api, token) = Api::start_remote();
    let remote = api.remote_addr();
    assert!(remote.starts_with("127.0.0.1:"), "{remote}");
    assert_ne!(remote, api.addr());
    assert_eq!(
        ApiInfo::load(&api.env.layout).unwrap().remote_urls,
        api.info.remote_urls,
        "api.json names the remote listener"
    );

    // Health needs no token, and the remote listener checks no Host.
    let health = http(
        &remote,
        "GET",
        "/v1/health",
        &[("Host", "machine.tailnet.ts.net:7337")],
        None,
    );
    assert_eq!(health.status, 200);
    assert_eq!(health.body["pid"], api.info.pid);
    // Why the daemon is down (local paths) is only told with the token.
    assert_eq!(health.body["daemon"]["reachable"], false);
    assert_eq!(
        health.body["daemon"]["error"], "the daemon is not reachable",
        "{:?}",
        health.body
    );
    let detailed = api.call_remote(&token, "GET", "/v1/health", None);
    assert!(
        detailed.body["daemon"]["error"]
            .as_str()
            .is_some_and(|e| e.contains("not running")),
        "{:?}",
        detailed.body
    );

    // No token, the loopback API token, a wrong one: 401, routes hidden.
    let reply = http(&remote, "GET", "/v1/sessions?all=true", &[], None);
    assert_eq!(reply.status, 401);
    assert_eq!(reply.header("www-authenticate"), Some("Bearer"));
    let reply = api.call_remote(&api.info.token, "GET", "/v1/sessions?all=true", None);
    assert_eq!(reply.status, 401);
    assert!(
        reply.message().contains("remote token"),
        "{}",
        reply.message()
    );
    let reply = api.call_remote("abcdefghijklmnopqrstuvwxyz", "GET", "/v1/folders", None);
    assert_eq!(reply.status, 401);
    let reply = http(&remote, "GET", "/v1/secret", &[], None);
    assert_eq!(reply.status, 401, "unknown routes stay hidden");

    // The remote token, from any Host and with an Origin: the user.
    let bearer = format!("Bearer {token}");
    let reply = http(
        &remote,
        "GET",
        "/v1/sessions?all=true",
        &[
            ("Authorization", &bearer),
            ("Host", "machine.tailnet.ts.net:7337"),
            ("Origin", "http://localhost:8081"),
        ],
        None,
    );
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    assert_eq!(reply.body, json!({"sessions": []}));
    assert!(reply.header("access-control-allow-origin").is_none());
    let reply = api.call_remote(
        &token,
        "POST",
        "/v1/sessions",
        Some(json!({"folder": api.env.project, "name": "from-phone", "command": bash()})),
    );
    assert_eq!(reply.status, 201, "{:?}", reply.body);
    assert_eq!(reply.body["session"]["created_by"], json!({"kind": "user"}));
    let reply = api.call_remote(&token, "GET", "/v1/secret", None);
    assert_eq!(reply.status, 404);

    // Identity headers are refused: remote callers are the user.
    for (name, value) in [
        ("X-Rakumux-Session-Id", "01JX"),
        ("X-Rakumux-Session-Token", "t"),
        ("X-Rakumux-Agent", "opencode"),
        ("X-Rakumux-Agent-Conversation", "ses_1"),
    ] {
        let reply = http(
            &remote,
            "GET",
            "/v1/sessions?all=true",
            &[("Authorization", &bearer), (name, value)],
            None,
        );
        assert_eq!(reply.status, 403, "{name}: {:?}", reply.body);
        assert_eq!(reply.code(), "permission_denied");
    }

    // The loopback listener keeps its rules: its own token, no browsers,
    // a loopback Host.
    let reply = api.raw("GET", "/v1/folders", &[("Authorization", &bearer)], None);
    assert_eq!(reply.status, 401);
    let loopback = format!("Bearer {}", api.info.token);
    let reply = api.raw(
        "GET",
        "/v1/folders",
        &[
            ("Authorization", &loopback),
            ("Origin", "http://localhost:8081"),
        ],
        None,
    );
    assert_eq!(reply.status, 403);
    let reply = api.raw(
        "GET",
        "/v1/folders",
        &[
            ("Authorization", &loopback),
            ("Host", "machine.tailnet.ts.net"),
        ],
        None,
    );
    assert_eq!(reply.status, 403);
}

#[test]
fn remote_listen_needs_remote_access_on() {
    let env = TestEnv::new();
    let output = env
        .command(std::path::Path::new(API_EXE))
        .args(["--remote-listen", "127.0.0.1:0"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("rakumux remote on"), "{stderr}");
    assert!(!discovery::info_path(&env.layout).exists());

    // remote.json's `listen` binds without the flag.
    let token = env.enable_remote(&["127.0.0.1:0"]);
    let mut api = Api::start_in(env);
    assert_eq!(api.info.remote_urls.len(), 1, "{:?}", api.info);
    let reply = api.call_remote(&token, "GET", "/v1/folders", None);
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    assert_eq!(api.terminate(), Some(0));
}

#[test]
fn sessions_can_be_killed_revived_and_deleted() {
    let (api, token) = Api::start_remote();
    let first = create(&api, &api.env.project, "one", &[]);
    let second = create(&api, &api.env.other_project, "two", &[]);
    let id = first["id"].as_str().unwrap().to_owned();

    // all=true: every folder's sessions, on both listeners.
    for reply in [
        api.call_remote(&token, "GET", "/v1/sessions?all=true", None),
        api.call("GET", "/v1/sessions?all=true&include_dead=true", None),
    ] {
        assert_eq!(reply.status, 200, "{:?}", reply.body);
        let ids: Vec<&str> = reply.body["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["id"].as_str().unwrap())
            .collect();
        assert!(ids.contains(&id.as_str()), "{ids:?}");
        assert!(ids.contains(&second["id"].as_str().unwrap()), "{ids:?}");
    }
    let reply = api.call("GET", "/v1/sessions?all=true&folder=other", None);
    assert_eq!(reply.status, 400);

    // Kill: 204, then the session is exited; killing it again is a 409.
    let kill = format!("/v1/sessions/{id}/kill");
    let reply = api.call_remote(&token, "POST", &kill, None);
    assert_eq!(reply.status, 204, "{:?}", reply.body);
    assert_eq!(reply.body, Value::Null);
    wait_for_status(&api, &id, "exited");
    let reply = api.call_remote(&token, "POST", &kill, None);
    assert_eq!(reply.status, 409, "{:?}", reply.body);
    assert_eq!(reply.code(), "session_not_live");
    assert_eq!(api.call_remote(&token, "GET", &kill, None).status, 405);

    // Revive: the same session, live again. A body may set the size.
    let revive = format!("/v1/sessions/{id}/revive");
    let reply = api.call_remote(&token, "POST", &revive, Some(json!({"resume": false})));
    assert_eq!(reply.status, 400, "unknown body fields are refused");
    let reply = api.call_remote(&token, "POST", &revive, None);
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    assert_eq!(reply.body["session"]["id"], id.as_str());
    assert_eq!(reply.body["session"]["status"], "live");
    let reply = api.call_remote(&token, "POST", &revive, None);
    assert_eq!(reply.status, 400, "already running: {:?}", reply.body);
    assert_eq!(api.call("POST", &kill, None).status, 204);
    wait_for_status(&api, &id, "exited");
    let reply = api.call("POST", &revive, Some(json!({"cols": 100, "rows": 30})));
    assert_eq!(reply.status, 200, "{:?}", reply.body);
    assert_eq!(reply.body["session"]["terminal"]["cols"], 100);
    assert_eq!(reply.body["session"]["terminal"]["rows"], 30);

    // Delete: 204, then gone.
    let path = format!("/v1/sessions/{id}");
    let reply = api.call_remote(&token, "DELETE", &path, None);
    assert_eq!(reply.status, 204, "{:?}", reply.body);
    assert_eq!(api.call("GET", &path, None).status, 404);
    assert_eq!(api.call_remote(&token, "DELETE", &path, None).status, 404);
    assert_eq!(api.call("POST", &revive, None).status, 404);
    assert_eq!(api.call("POST", &kill, None).status, 404);
    let other = format!("/v1/sessions/{}", second["id"].as_str().unwrap());
    assert_eq!(api.call("DELETE", &other, None).status, 204);
}

#[test]
fn a_terminal_streams_over_websocket() {
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let (api, token) = Api::start_remote();
    let session = create(&api, &api.env.project, "ws", &[]);
    let id = session["id"].as_str().unwrap().to_owned();
    let remote = api.remote_addr();
    let bearer = format!("Bearer {token}");
    let loopback = format!("Bearer {}", api.info.token);
    let attach = |query: &str| format!("/v1/sessions/{id}/attach?{query}");
    let rt = runtime();

    rt.block_on(async {
        // Errors come before the upgrade, as JSON.
        let (status, body) = ws_connect(
            &remote,
            "/v1/sessions/01JNOSUCHSESSION0000000000/attach",
            &[("Authorization", &bearer)],
        )
        .await
        .err()
        .unwrap();
        assert_eq!(status, 404);
        assert_eq!(body["error"]["code"], "not_found");
        let (status, _) = ws_connect(&remote, &attach("role=observer"), &[])
            .await
            .err()
            .unwrap();
        assert_eq!(status, 401);
        let (status, body) = ws_connect(
            &remote,
            &attach("role=controller"),
            &[("Authorization", &bearer)],
        )
        .await
        .err()
        .unwrap();
        assert_eq!(status, 400, "{body}");

        // A controller at 80×24 (React Native sends an Origin).
        let mut controller = ws_connect(
            &remote,
            &attach("role=controller&cols=80&rows=24"),
            &[
                ("Authorization", &bearer),
                ("Origin", "http://localhost:8081"),
            ],
        )
        .await
        .unwrap();
        let attached = next_json(&mut controller).await.unwrap();
        assert_eq!(attached["type"], "attached");
        assert_eq!(attached["role"], "controller");
        assert_eq!(attached["session"]["id"], id.as_str());
        assert_eq!(attached["session"]["folder"], json!(api.env.project));
        assert_eq!(
            (&attached["cols"], &attached["rows"]),
            (&json!(80), &json!(24))
        );
        use base64::Engine as _;
        let checkpoint = base64::engine::general_purpose::STANDARD
            .decode(attached["checkpoint"].as_str().unwrap())
            .unwrap();
        assert!(!checkpoint.is_empty());
        assert!(attached["offset"].is_u64());

        // An observer, on the loopback listener: the same terminal; its
        // input is ignored.
        let mut observer = ws_connect(
            &api.addr(),
            &attach("role=observer"),
            &[("Authorization", &loopback)],
        )
        .await
        .unwrap();
        let first = next_json(&mut observer).await.unwrap();
        assert_eq!(first["type"], "attached");
        assert_eq!(first["role"], "observer");
        assert_eq!((&first["cols"], &first["rows"]), (&json!(80), &json!(24)));
        observer
            .send(Message::Binary(b"echo OBSERVER_$((1+1))\r".to_vec()))
            .await
            .unwrap();

        // The controller's input runs; output comes back as binary frames.
        controller
            .send(Message::Binary(b"echo WS_$((6*7))\r".to_vec()))
            .await
            .unwrap();
        let mut output = Vec::new();
        read_until(&mut controller, &mut output, |_, out| {
            String::from_utf8_lossy(out).contains("WS_42")
        })
        .await;
        let mut seen = Vec::new();
        read_until(&mut observer, &mut seen, |_, out| {
            String::from_utf8_lossy(out).contains("WS_42")
        })
        .await;

        // Resize: both get `resized` with a checkpoint.
        controller
            .send(Message::Text(
                json!({"type": "resize", "cols": 100, "rows": 30}).to_string(),
            ))
            .await
            .unwrap();
        for socket in [&mut controller, &mut observer] {
            let resized = read_until(socket, &mut Vec::new(), |message, _| {
                message.is_some_and(|m| m["type"] == "resized")
            })
            .await
            .unwrap();
            assert_eq!(
                (&resized["cols"], &resized["rows"]),
                (&json!(100), &json!(30))
            );
            assert!(resized["checkpoint"]
                .as_str()
                .is_some_and(|c| !c.is_empty()));
            assert!(resized["offset"].is_u64());
        }

        // The observer takes control; both hear about it.
        observer
            .send(Message::Text(
                json!({"type": "promote", "cols": 90, "rows": 28}).to_string(),
            ))
            .await
            .unwrap();
        let promoted = read_until(&mut observer, &mut Vec::new(), |message, _| {
            message.is_some_and(|m| m["type"] == "role_changed")
        })
        .await
        .unwrap();
        assert_eq!(promoted["role"], "controller");
        let demoted = read_until(&mut controller, &mut Vec::new(), |message, _| {
            message.is_some_and(|m| m["type"] == "role_changed")
        })
        .await
        .unwrap();
        assert_eq!(demoted["role"], "observer");

        // Closing the sockets detaches; the session keeps running.
        controller.close(None).await.unwrap();
        observer.close(None).await.unwrap();
    });
    let text = wait_for_text(&api, &id, "WS_42");
    assert!(
        !text.contains("OBSERVER_2"),
        "an observer's input ran:\n{text}"
    );
    wait_for_status(&api, &id, "live");

    // When the program exits the server says so and closes the socket.
    rt.block_on(async {
        let mut socket = ws_connect(
            &remote,
            &attach("role=controller&cols=80&rows=24"),
            &[("Authorization", &bearer)],
        )
        .await
        .unwrap();
        assert_eq!(next_json(&mut socket).await.unwrap()["type"], "attached");
        socket
            .send(Message::Binary(b"exit 3\r".to_vec()))
            .await
            .unwrap();
        let exited = read_until(&mut socket, &mut Vec::new(), |message, _| {
            message.is_some_and(|m| m["type"] == "exited")
        })
        .await
        .unwrap();
        assert_eq!(exited["code"], 3);
        assert!(next_json(&mut socket).await.is_none(), "the socket closes");
    });
    wait_for_status(&api, &id, "exited");

    // Attaching to a session that isn't live: 409 before the upgrade. A
    // plain GET is told to upgrade.
    rt.block_on(async {
        let (status, body) = ws_connect(
            &remote,
            &attach("role=observer"),
            &[("Authorization", &bearer)],
        )
        .await
        .err()
        .unwrap();
        assert_eq!(status, 409, "{body}");
        assert_eq!(body["error"]["code"], "session_not_live");
    });
    let reply = api.call_remote(&token, "GET", &attach("role=observer"), None);
    assert_eq!(reply.status, 426);
    assert_eq!(reply.code(), "upgrade_required");
    assert_eq!(reply.header("sec-websocket-version"), Some("13"));
}

#[test]
fn session_events_stream_over_websocket() {
    let (api, token) = Api::start_remote();
    let remote = api.remote_addr();
    let bearer = format!("Bearer {token}");
    runtime().block_on(async {
        let (status, _) = ws_connect(&remote, "/v1/events", &[]).await.err().unwrap();
        assert_eq!(status, 401);
        let mut events = ws_connect(
            &remote,
            "/v1/events",
            &[
                ("Authorization", &bearer),
                ("Origin", "http://localhost:8081"),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            next_json(&mut events).await.unwrap(),
            json!({"type": "subscribed"})
        );

        // A session in a folder that had none when the stream opened.
        let session = create(&api, &api.env.other_project, "evented", &[]);
        let id = session["id"].as_str().unwrap().to_owned();
        let of = |kind: &'static str| {
            let id = id.clone();
            move |message: Option<&Value>, _: &[u8]| {
                message.is_some_and(|m| {
                    m["type"] == "event"
                        && m["session_id"] == id.as_str()
                        && m["event"]["kind"] == kind
                })
            }
        };
        let created = read_until(&mut events, &mut Vec::new(), of("created"))
            .await
            .unwrap();
        assert_eq!(created["folder"], json!(api.env.other_project));
        assert_eq!(created["event"]["session"]["name"], "evented");
        assert_eq!(
            created["event"]["session"]["folder"],
            json!(api.env.other_project)
        );
        assert!(created["at"].as_i64().unwrap() > 0);

        let reply = api.call("POST", &format!("/v1/sessions/{id}/kill"), None);
        assert_eq!(reply.status, 204);
        let exited = read_until(&mut events, &mut Vec::new(), of("exited"))
            .await
            .unwrap();
        assert!(exited["event"].get("code").is_some(), "{exited}");

        let reply = api.call("DELETE", &format!("/v1/sessions/{id}"), None);
        assert_eq!(reply.status, 204);
        let removed = read_until(&mut events, &mut Vec::new(), of("removed"))
            .await
            .unwrap();
        assert_eq!(removed["event"], json!({"kind": "removed"}));
    });

    // Loopback works the same with the API token; a plain GET is a 426.
    runtime().block_on(async {
        let loopback = format!("Bearer {}", api.info.token);
        let mut events = ws_connect(&api.addr(), "/v1/events", &[("Authorization", &loopback)])
            .await
            .unwrap();
        assert_eq!(next_json(&mut events).await.unwrap()["type"], "subscribed");
    });
    assert_eq!(api.call("GET", "/v1/events", None).status, 426);
    assert_eq!(api.call("GET", "/v1/events?x=1", None).status, 400);
}
