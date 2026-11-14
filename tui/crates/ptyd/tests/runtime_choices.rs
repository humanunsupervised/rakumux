//! `session_choose` end to end: the real daemon in a temp home, a session
//! running a stand-in for Claude Code (a script installed as `claude`, so the
//! daemon takes it for Claude Code) that draws a prompt for permission
//! captured from Claude Code 2.1.325 and takes its answer as Claude Code
//! does, one key, the option's number. The daemon reads the prompt off the
//! screen into `TerminalView::choice`, refuses a pick of another prompt or
//! option without typing anything, and picks the option asked for.
#![cfg(target_os = "linux")]

mod runtime_support;

use runtime_support::{wait_until, TestHome};
use session_core::api::{CreateSession, ErrorCode};
use session_core::client::{AuthMode, Client, ClientConfig, SessionApi};
use session_core::ids::SessionId;
use session_core::model::{ChoicePrompt, ScopeRoot};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const EXE: &str = env!("CARGO_BIN_EXE_rakumux-ptyd");
const SLOW: Duration = Duration::from_secs(20);

struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn daemon(home: &TestHome) -> (Daemon, Client) {
    let daemon = Daemon(
        Command::new(EXE)
            .arg("serve")
            .env(session_core::env::HOME, &home.layout.home)
            .env("HOME", home.dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let client = loop {
        let connected = Client::connect(ClientConfig {
            layout: home.layout.clone(),
            client_name: "choices-e2e".into(),
            version: "test".into(),
            auth: AuthMode::User,
            spawn: false,
            ptyd_path: None,
        });
        if let Ok(client) = connected {
            break client;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    (daemon, client)
}

/// Install the stand-in: it shows the prompt, writes the key it gets to
/// `picked`, then shows Claude Code's prompt box. Only builtins run, so it
/// alone is the session's foreground.
fn stand_in(dir: &Path) {
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../integrations/claude-code/fixtures/choices/bash.txt");
    let script = format!(
        "#!/bin/bash\n\
         printf '\\e[2J\\e[H%s' \"$(<'{fixture}')\"\n\
         IFS= read -rsn1 key\n\
         printf '%s' \"$key\" > '{picked}'\n\
         printf '\\e[2J\\e[H done\\n────────\\n❯ \\n────────\\n'\n\
         while :; do read -rs _; done\n",
        fixture = fixture.display(),
        picked = dir.join("picked").display(),
    );
    let path = dir.join("claude");
    std::fs::write(&path, script).unwrap();
    let made = Command::new("chmod").arg("+x").arg(&path).status().unwrap();
    assert!(made.success());
}

fn choice(client: &Client, id: &SessionId) -> Option<ChoicePrompt> {
    client.get(id).ok()?.terminal?.choice
}

#[test]
fn a_prompt_for_permission_is_read_off_the_screen_and_answered_by_its_number() {
    let home = TestHome::new();
    let bin = home.dir.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stand_in(&bin);
    let (_daemon, client) = daemon(&home);
    let project = std::fs::canonicalize(&home.work).unwrap();
    let id = client
        .create(CreateSession {
            scope: Some(ScopeRoot::resolve(&project).unwrap()),
            name: Some("claude".into()),
            cwd: None,
            argv: Some(vec!["bash".into(), "--norc".into(), "--noprofile".into()]),
            startup_cmds: vec![format!("exec {}", bin.join("claude").display())],
            env: home.env(),
            cols: 80,
            rows: 24,
            machine: None,
            prefill: None,
            first_input: None,
        })
        .unwrap()
        .id;

    assert!(
        wait_until(SLOW, || choice(&client, &id).is_some()),
        "no prompt read: {:?}",
        client.get(&id).map(|view| view.terminal)
    );
    let prompt = choice(&client, &id).unwrap();
    assert_eq!(prompt.title.as_deref(), Some("Bash command"));
    assert_eq!(prompt.detail.as_deref(), Some("touch made2.txt"));
    assert_eq!(prompt.question, "Do you want to proceed?");
    let numbers: Vec<u8> = prompt.options.iter().map(|option| option.number).collect();
    assert_eq!(numbers, [1, 2, 3, 4]);
    assert_eq!(prompt.options[3].label, "No");

    // Another prompt, or an option it doesn't offer: nothing typed.
    let quiet = Duration::from_secs(1);
    let other = client
        .choose(&id, "0000000000000000", 4, quiet)
        .unwrap_err();
    assert_eq!(other.code, ErrorCode::QuestionGone);
    let none = client.choose(&id, &prompt.key, 7, quiet).unwrap_err();
    assert_eq!(none.code, ErrorCode::InvalidArgument);
    assert!(!bin.join("picked").exists());

    // No: its number, once, and the prompt is gone.
    client.choose(&id, &prompt.key, 4, quiet).unwrap();
    assert_eq!(std::fs::read_to_string(bin.join("picked")).unwrap(), "4");
    assert!(wait_until(SLOW, || choice(&client, &id).is_none()));
    let gone = client.choose(&id, &prompt.key, 4, quiet).unwrap_err();
    assert_eq!(gone.code, ErrorCode::QuestionGone);
}
