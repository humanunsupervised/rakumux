//! Workspaces end to end: the real daemon binary in a temp home, its
//! registry on disk, and clients over its socket. A workspace is made, a
//! session starts in it and another in none, sessions move between them,
//! a session's own sessions start in its workspace while it can't make one,
//! everything survives the daemon starting again, and forgetting a workspace
//! leaves its sessions in none.
#![cfg(target_os = "linux")]

mod runtime_support;

use runtime_support::{wait_until, TestHome};
use session_core::api::{CreateSession, ErrorCode};
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::ids::SessionId;
use session_core::model::ScopeRoot;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const EXE: &str = env!("CARGO_BIN_EXE_rakumux-ptyd");
const SLOW: Duration = Duration::from_secs(30);

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(home: &TestHome) -> Daemon {
    let mut command = Command::new(EXE);
    command
        .arg("serve")
        .env(session_core::env::HOME, &home.layout.home)
        .env("HOME", home.dir.path())
        .env("SHELL", "/bin/bash")
        .env_remove(session_core::env::SESSION_ID)
        .env_remove(session_core::env::SESSION_TOKEN)
        .env_remove(session_core::env::SOCKET)
        .env_remove(session_core::env::BIN)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Daemon(command.spawn().unwrap())
}

fn connect(home: &TestHome, auth: AuthMode) -> Client {
    let mut tries = 0;
    loop {
        let connected = Client::connect(ClientConfig {
            layout: home.layout.clone(),
            client_name: "workspaces-e2e".into(),
            version: "test".into(),
            auth: auth.clone(),
            spawn: false,
            ptyd_path: None,
        });
        match connected {
            Ok(client) => return client,
            Err(error) if tries > 200 => panic!("no daemon: {error:?}"),
            Err(_) => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

fn create(
    scope: &ScopeRoot,
    name: &str,
    workspace: Option<&session_core::ids::WorkspaceId>,
    startup: &[String],
) -> CreateSession {
    CreateSession {
        scope: Some(scope.clone()),
        name: Some(name.into()),
        cwd: None,
        argv: None,
        startup_cmds: startup.to_vec(),
        env: vec![],
        cols: 80,
        rows: 24,
        machine: None,
        prefill: None,
        first_input: None,
        workspace: workspace.cloned(),
    }
}

fn workspace_of(client: &Client, id: &SessionId) -> Option<String> {
    let view = client.get(id).unwrap();
    let workspace = view.workspace?;
    client
        .workspaces()
        .unwrap()
        .into_iter()
        .find(|known| known.id == workspace)
        .map(|known| known.name)
}

#[test]
fn sessions_live_in_workspaces_move_between_them_and_keep_them_across_a_restart() {
    let home = TestHome::new();
    let work = std::fs::canonicalize(&home.work).unwrap();
    let scope = ScopeRoot::from_canonical(work);
    let daemon = start(&home);
    let client = connect(&home, AuthMode::User);

    // Made, and its name is its own whatever the case.
    let launch = client.workspace_create("  Launch ").unwrap();
    assert_eq!(launch.name, "Launch");
    let taken = client.workspace_create("launch").unwrap_err();
    assert_eq!(taken.code, ErrorCode::InvalidArgument, "{taken:?}");
    assert_eq!(
        client.workspace_create("two\nlines").unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    let research = client.workspace_create("Research").unwrap();
    let names: Vec<String> = client
        .workspaces()
        .unwrap()
        .into_iter()
        .map(|w| w.name)
        .collect();
    assert_eq!(names, ["Launch", "Research"]);

    // A session starts in a workspace, another in none; an unknown one is
    // refused.
    let token_file = home.dir.path().join("token");
    let write_token = format!(
        "printf %s \"$RAKUMUX_SESSION_TOKEN\" > {}",
        token_file.display()
    );
    let lead = client
        .create(create(&scope, "lead", Some(&launch.id), &[write_token]))
        .unwrap();
    assert_eq!(lead.workspace.as_ref(), Some(&launch.id));
    let loose = client.create(create(&scope, "loose", None, &[])).unwrap();
    assert_eq!(loose.workspace, None);
    let gone = session_core::ids::WorkspaceId::generate();
    assert_eq!(
        client
            .create(create(&scope, "nowhere", Some(&gone), &[]))
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let listed = client.list_all(false).unwrap();
    let in_launch: Vec<&str> = listed
        .iter()
        .filter(|view| view.workspace.as_ref() == Some(&launch.id))
        .map(|view| view.name.as_str())
        .collect();
    assert_eq!(in_launch, ["lead"]);

    // Moved into one, then another, then out, and into one again.
    client.set_workspace(&loose.id, Some(&research.id)).unwrap();
    assert_eq!(
        workspace_of(&client, &loose.id).as_deref(),
        Some("Research")
    );
    client.set_workspace(&loose.id, None).unwrap();
    assert_eq!(workspace_of(&client, &loose.id), None);
    let moved = client.set_workspace(&loose.id, Some(&research.id)).unwrap();
    assert_eq!(moved.workspace.as_ref(), Some(&research.id));
    assert_eq!(
        client
            .set_workspace(&loose.id, Some(&gone))
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );

    // Inside `lead`: it lists workspaces, its own sessions start in its
    // workspace, it moves itself but nobody else, and makes none.
    assert!(wait_until(SLOW, || std::fs::read_to_string(&token_file)
        .is_ok_and(|token| !token.is_empty())));
    let token = std::fs::read_to_string(&token_file).unwrap();
    let inside = connect(
        &home,
        AuthMode::Session {
            id: lead.id.clone(),
            token,
        },
    );
    assert_eq!(inside.workspaces().unwrap().len(), 2);
    let child = inside
        .create(create(&scope, "helper", Some(&research.id), &[]))
        .unwrap();
    assert_eq!(child.workspace.as_ref(), Some(&launch.id));
    assert_eq!(
        inside.workspace_create("Mine").unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        inside
            .workspace_rename(&launch.id, "Mine")
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        inside.set_workspace(&loose.id, None).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    inside.set_workspace(&lead.id, Some(&research.id)).unwrap();
    assert_eq!(workspace_of(&client, &lead.id).as_deref(), Some("Research"));
    drop(inside);

    // The daemon starts again: workspaces and sessions' are where they were.
    drop(client);
    drop(daemon);
    let _daemon = start(&home);
    let client = connect(&home, AuthMode::User);
    assert_eq!(client.workspaces().unwrap().len(), 2);
    assert_eq!(workspace_of(&client, &lead.id).as_deref(), Some("Research"));
    assert_eq!(
        workspace_of(&client, &loose.id).as_deref(),
        Some("Research")
    );
    assert_eq!(workspace_of(&client, &child.id).as_deref(), Some("Launch"));

    // Renamed: its sessions stay in it; a name another has is refused.
    let renamed = client
        .workspace_rename(&research.id, " Deep research ")
        .unwrap();
    assert_eq!(renamed.name, "Deep research");
    assert_eq!(
        workspace_of(&client, &loose.id).as_deref(),
        Some("Deep research")
    );
    assert_eq!(
        client
            .workspace_rename(&research.id, "launch")
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        client.workspace_rename(&gone, "Other").unwrap_err().code,
        ErrorCode::NotFound
    );

    // Forgotten: its sessions are in none, and the others stay.
    client.workspace_delete(&research.id).unwrap();
    assert_eq!(workspace_of(&client, &lead.id), None);
    assert_eq!(workspace_of(&client, &loose.id), None);
    assert_eq!(workspace_of(&client, &child.id).as_deref(), Some("Launch"));
    assert_eq!(
        client.workspace_delete(&research.id).unwrap_err().code,
        ErrorCode::NotFound
    );

    // A session deleted into the history comes back in its workspace.
    client.delete(&child.id).unwrap();
    let restored = client.restore(&child.id).unwrap();
    assert_eq!(restored.workspace.as_ref(), Some(&launch.id));
}
