//! Cursor's agent CLI: the chat a process runs, and the command that resumes
//! it with the flags that shape an interactive session.
//!
//! A chat (its id a UUID, the agent's `agentId`) is kept in
//! `<config>/chats/<md5 of the folder>/<chat>/`: `store.db`, which the
//! process opens with the chat's first message and keeps open, and
//! `meta.json` (`{createdAtMs, updatedAtMs?, hasConversation, isSubagent?}`).
//! Its transcript is `<data>/projects/<folder slug>/agent-transcripts/<chat>/<chat>.jsonl`.
//! `<config>` is `CURSOR_CONFIG_DIR`, else `$XDG_CONFIG_HOME/cursor`, else
//! `~/.cursor`; `<data>` is `CURSOR_DATA_DIR`, else `~/.cursor` (the daemon's
//! environment, as for Claude Code's `CLAUDE_CONFIG_DIR`).
use super::*;

const CURSOR_SWITCHES: &[&str] = &[
    "--plan",
    "-f",
    "--force",
    "--yolo",
    "--auto-review",
    "--approve-mcps",
    "--trust",
];
/// One value each; `--add-dir` and `--plugin-dir` repeat.
const CURSOR_OPTIONS: &[&str] = &[
    "--model",
    "--mode",
    "--sandbox",
    "--workspace",
    "-e",
    "--endpoint",
    "--add-dir",
    "--plugin-dir",
];
/// Options whose value is the next word (`--resume` and `--worktree` only
/// when one follows), for finding the first word that isn't a flag.
const CURSOR_VALUES: &[&str] = &[
    "--api-key",
    "-H",
    "--header",
    "--output-format",
    "--worktree-base",
    "--resume",
    "-w",
    "--worktree",
];
/// Commands that aren't a chat: what they ran is typed on revive, not run.
const CURSOR_COMMANDS: &[&str] = &[
    "persist",
    "install-shell-integration",
    "uninstall-shell-integration",
    "login",
    "logout",
    "mcp",
    "plugin",
    "worker",
    "status",
    "whoami",
    "models",
    "team",
    "bedrock",
    "about",
    "update",
    "create-chat",
    "generate-rule",
    "rule",
    "help",
    "acp",
    "sandbox",
];

/// Cursor's configuration directory, where chats are kept.
pub(crate) fn cursor_config_dir() -> Option<PathBuf> {
    let set = |name: &str| {
        std::env::var_os(name)
            .filter(|value| !value.to_string_lossy().trim().is_empty())
            .map(PathBuf::from)
    };
    set("CURSOR_CONFIG_DIR")
        .or_else(|| set("XDG_CONFIG_HOME").map(|dir| dir.join("cursor")))
        .or_else(|| home_dir().map(|home| home.join(".cursor")))
}

/// Cursor's data directory, where transcripts are kept (shared with the
/// Cursor editor's agents).
pub(crate) fn cursor_data_dir() -> Option<PathBuf> {
    std::env::var_os("CURSOR_DATA_DIR")
        .filter(|value| !value.to_string_lossy().trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".cursor")))
}

/// What the process `argv` asks Cursor for, after its launcher (`agent
/// --use-system-ca …/index.js`) and the `cursor agent` editor command.
fn cursor_args(cmdline: &[String]) -> Vec<String> {
    let args = match cmdline {
        [only] => only.split_whitespace().skip(1).map(String::from).collect(),
        _ => {
            let script = cmdline
                .iter()
                .position(|word| word.ends_with("/index.js") && word.contains("cursor-agent"));
            cmdline
                .iter()
                .skip(script.unwrap_or(0) + 1)
                .cloned()
                .collect::<Vec<_>>()
        }
    };
    match args.split_first() {
        Some((first, rest)) if first == "agent" => rest.to_vec(),
        _ => args,
    }
}

/// The first word of `args` that isn't a flag or a flag's value: a command
/// (`login`) or the prompt.
fn first_word(args: &[String]) -> Option<&str> {
    let mut words = args.iter().map(String::as_str);
    while let Some(word) = words.next() {
        if word == "--" {
            return words.next();
        }
        if word.starts_with('-') {
            let takes = CURSOR_OPTIONS.contains(&word) || CURSOR_VALUES.contains(&word);
            if takes {
                let mut ahead = words.clone();
                if ahead.next().is_some_and(|next| !next.starts_with('-')) {
                    words.next();
                }
            }
            continue;
        }
        return Some(word);
    }
    None
}

/// The plan for a Cursor process: a command that isn't a chat is typed back;
/// a chat is resumed by its id: `cursor-agent <flags> --resume <id>`
/// (`cursor-agent`, never `agent`, which Grok installs too).
pub(super) fn cursor_plan(process: &ProcessInfo, cwd: Option<&Path>, bound: Option<&str>) -> Plan {
    let args = cursor_args(&process.cmdline);
    if first_word(&args).is_some_and(|word| CURSOR_COMMANDS.contains(&word)) {
        let program = process
            .cmdline
            .first()
            .and_then(|argv0| argv0.split_whitespace().next())
            .map(|argv0| argv0.rsplit('/').next().unwrap_or(argv0))
            .filter(|name| matches!(*name, "agent" | "cursor-agent"))
            .unwrap_or("cursor-agent");
        let mut argv = vec![program.to_string()];
        argv.extend(args);
        return Plan::Command(argv);
    }
    let Some(id) = cursor_chat(process.pid, cwd, bound, true) else {
        return Plan::Unresolved;
    };
    let mut argv = vec!["cursor-agent".to_string()];
    argv.extend(keep_flags(&args, CURSOR_SWITCHES, CURSOR_OPTIONS, &[]));
    argv.push("--resume".into());
    argv.push(id);
    Plan::Resume(argv)
}

/// The chat process `pid` runs: one it has open (Linux), the one the
/// session named (`bound`, from its hooks) when that's among them or none
/// is, else, when `recent` allows, the folder's newest (`cwd`'s).
pub(crate) fn cursor_chat(
    pid: u32,
    cwd: Option<&Path>,
    bound: Option<&str>,
    recent: bool,
) -> Option<String> {
    cursor_chat_in(&cursor_config_dir()?, pid, cwd, bound, recent)
}

fn cursor_chat_in(
    config: &Path,
    pid: u32,
    cwd: Option<&Path>,
    bound: Option<&str>,
    recent: bool,
) -> Option<String> {
    let bound = bound.filter(|id| is_conversation_id(id));
    let open = cursor_open_chats(config, pid);
    if let Some(bound) = bound {
        if open.is_empty() || open.iter().any(|(id, _)| id == bound) {
            return Some(bound.to_string());
        }
    }
    if let Some((id, _)) = open.into_iter().max_by_key(|(_, modified)| *modified) {
        return Some(id);
    }
    if !recent {
        return None;
    }
    cursor_recent_chat(config, cwd?)
}

/// The chats `pid` has open (their `store.db`), not a subagent's, with when
/// each was last written.
fn cursor_open_chats(config: &Path, pid: u32) -> Vec<(String, std::time::SystemTime)> {
    let chats = config.join("chats");
    let mut found: Vec<(String, std::time::SystemTime)> = Vec::new();
    for path in crate::procinfo::open_files(pid) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !matches!(name, "store.db" | "store.db-wal") {
            continue;
        }
        let Some(chat) = path.parent() else {
            continue;
        };
        let in_chats = chat
            .parent()
            .and_then(Path::parent)
            .is_some_and(|dir| dir == chats);
        let Some(id) = chat.file_name().and_then(|id| id.to_str()) else {
            continue;
        };
        if !in_chats || !is_conversation_id(id) || is_subagent_chat(chat) {
            continue;
        }
        let modified = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .unwrap_or(std::time::UNIX_EPOCH);
        match found.iter_mut().find(|(known, _)| known == id) {
            Some(entry) => entry.1 = entry.1.max(modified),
            None => found.push((id.to_string(), modified)),
        }
    }
    found
}

/// `<chat>/meta.json`.
#[derive(Deserialize)]
struct ChatMeta {
    #[serde(default, rename = "hasConversation")]
    has_conversation: bool,
    #[serde(default, rename = "isSubagent")]
    is_subagent: bool,
    #[serde(default, rename = "updatedAtMs")]
    updated_at_ms: Option<i64>,
    #[serde(default, rename = "createdAtMs")]
    created_at_ms: Option<i64>,
}

fn chat_meta(chat: &Path) -> Option<ChatMeta> {
    let mut bytes = Vec::new();
    std::fs::File::open(chat.join("meta.json"))
        .ok()?
        .take(64 * 1024)
        .read_to_end(&mut bytes)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn is_subagent_chat(chat: &Path) -> bool {
    chat_meta(chat).is_some_and(|meta| meta.is_subagent)
}

/// The most chats of one folder looked at.
const MAX_CHATS: usize = 4096;

/// The newest chat of the folder `cwd` with a conversation, not a
/// subagent's: by its `updatedAtMs`, else when its store was written.
pub(crate) fn cursor_recent_chat(config: &Path, cwd: &Path) -> Option<String> {
    let folder = config
        .join("chats")
        .join(md5_hex(cwd.as_os_str().as_encoded_bytes()));
    std::fs::read_dir(folder)
        .ok()?
        .filter_map(Result::ok)
        .take(MAX_CHATS)
        .filter_map(|entry| {
            let id = entry.file_name().into_string().ok()?;
            if !is_conversation_id(&id) {
                return None;
            }
            let chat = entry.path();
            let meta = chat_meta(&chat)?;
            if !meta.has_conversation || meta.is_subagent {
                return None;
            }
            let written = || {
                let modified = std::fs::metadata(chat.join("store.db"))
                    .and_then(|meta| meta.modified())
                    .ok()?;
                let ms = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
                i64::try_from(ms.as_millis()).ok()
            };
            let at = meta
                .updated_at_ms
                .or_else(written)
                .or(meta.created_at_ms)
                .unwrap_or(0);
            Some((at, id))
        })
        .max()
        .map(|(_, id)| id)
}

/// MD5 (RFC 1321) as lowercase hex: how Cursor names a folder's chats
/// directory. Not for anything secret.
pub(crate) fn md5_hex(input: &[u8]) -> String {
    const SHIFTS: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let constants: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();
    let mut message = input.to_vec();
    let bits = (input.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bits.to_le_bytes());
    let mut state: [u32; 4] = [0x6745_2301, 0xefcd_ab89, 0x98ba_dcfe, 0x1032_5476];
    for block in message.chunks_exact(64) {
        let words: Vec<u32> = block
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes([word[0], word[1], word[2], word[3]]))
            .collect();
        let [mut a, mut b, mut c, mut d] = state;
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let rotated = a
                .wrapping_add(f)
                .wrapping_add(constants[i])
                .wrapping_add(words[g])
                .rotate_left(SHIFTS[i]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(rotated);
        }
        for (value, add) in state.iter_mut().zip([a, b, c, d]) {
            *value = value.wrapping_add(add);
        }
    }
    state
        .iter()
        .flat_map(|word| word.to_le_bytes())
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn md5_matches_the_rfc_vectors() {
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890"
            ),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
        // As Cursor names a folder's chats: Node's md5 of the resolved path.
        assert_eq!(
            md5_hex(b"/home/me/repos/x"),
            "2007e0a9ee492b7a83035364b82092af"
        );
    }

    #[test]
    fn flags_that_shape_a_chat_are_kept_and_secrets_and_prompts_dropped() {
        let launcher = [
            "/home/me/.local/bin/agent",
            "--use-system-ca",
            "/home/me/.local/share/cursor-agent/versions/2026.10.22-27dcf5f/index.js",
        ];
        let cmdline = |extra: &[&str]| {
            let mut all = args(&launcher);
            all.extend(args(extra));
            all
        };
        let kept = |extra: &[&str]| {
            let args = cursor_args(&cmdline(extra));
            keep_flags(&args, CURSOR_SWITCHES, CURSOR_OPTIONS, &[])
        };
        assert_eq!(
            kept(&[
                "--model",
                "gpt-5",
                "--yolo",
                "--api-key",
                "key_secret",
                "-H",
                "Authorization: Bearer secret",
                "--add-dir",
                "/a",
                "--add-dir",
                "/b",
                "-w",
                "fix",
                "--resume",
                "old-chat",
                "--sandbox=enabled",
                "--print",
                "--output-format",
                "json",
                "fix the bug",
            ]),
            args(&[
                "--model",
                "gpt-5",
                "--yolo",
                "--add-dir",
                "/a",
                "--add-dir",
                "/b",
                "--sandbox=enabled"
            ])
        );
        // The editor's `cursor agent` passes `agent` first.
        assert_eq!(kept(&["agent", "--plan", "-f"]), args(&["--plan", "-f"]));
        // Node's process.title rewrites argv into one string.
        assert_eq!(
            keep_flags(
                &cursor_args(&args(&["cursor-agent --trust --mode ask"])),
                CURSOR_SWITCHES,
                CURSOR_OPTIONS,
                &[]
            ),
            args(&["--trust", "--mode", "ask"])
        );
    }

    /// `<config>/chats/<md5 of cwd>/<id>/` with its `meta.json`.
    fn chat(config: &Path, cwd: &Path, id: &str, meta: serde_json::Value) -> PathBuf {
        let dir = config
            .join("chats")
            .join(md5_hex(cwd.as_os_str().as_encoded_bytes()))
            .join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
        std::fs::write(dir.join("store.db"), b"").unwrap();
        dir
    }

    #[test]
    fn the_folders_newest_chat_with_a_conversation_is_the_fallback() {
        let config = tempfile::tempdir().unwrap();
        let cwd = Path::new("/home/me/repos/x");
        let meta = |updated: i64, conversation: bool| {
            serde_json::json!({"schemaVersion": 1, "createdAtMs": 1, "updatedAtMs": updated,
                "hasConversation": conversation})
        };
        chat(config.path(), cwd, "0a-old", meta(100, true));
        chat(config.path(), cwd, "0b-new", meta(300, true));
        chat(config.path(), cwd, "0c-empty", meta(900, false));
        let mut subagent = meta(800, true);
        subagent["isSubagent"] = serde_json::json!(true);
        chat(config.path(), cwd, "0d-subagent", subagent);
        chat(
            config.path(),
            Path::new("/elsewhere"),
            "0e-other",
            meta(999, true),
        );
        assert_eq!(
            cursor_recent_chat(config.path(), cwd),
            Some("0b-new".into())
        );
        // Only where open files can't be read.
        assert_eq!(
            cursor_chat_in(config.path(), u32::MAX, Some(cwd), None, true),
            Some("0b-new".into())
        );
        assert_eq!(
            cursor_chat_in(config.path(), u32::MAX, Some(cwd), None, false),
            None
        );
        // The chat the session named wins over the folder's newest.
        assert_eq!(
            cursor_chat_in(config.path(), u32::MAX, Some(cwd), Some("0a-old"), true),
            Some("0a-old".into())
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_chat_whose_store_a_process_has_open_is_the_one_it_runs() {
        let config = tempfile::tempdir().unwrap();
        let cwd = Path::new("/home/me/repos/x");
        let meta = serde_json::json!({"createdAtMs": 1, "hasConversation": true});
        let running = chat(config.path(), cwd, "1a-running", meta.clone());
        let mut subagent = meta.clone();
        subagent["isSubagent"] = serde_json::json!(true);
        let helper = chat(config.path(), cwd, "1b-subagent", subagent);
        chat(config.path(), cwd, "1c-newer", meta);
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!(
                "exec 3<'{}' 4<'{}'; sleep 30",
                running.join("store.db").display(),
                helper.join("store.db").display()
            ))
            .spawn()
            .unwrap();
        let pid = child.id();
        let found = (0..100).find_map(|_| {
            std::thread::sleep(std::time::Duration::from_millis(20));
            cursor_chat_in(config.path(), pid, Some(cwd), None, true)
        });
        // Its own, not its subagent's, nor the folder's newest.
        assert_eq!(found, Some("1a-running".into()));
        // A chat its hooks name that it doesn't have open is stale.
        assert_eq!(
            cursor_chat_in(config.path(), pid, Some(cwd), Some("1c-newer"), true),
            Some("1a-running".into())
        );
        assert_eq!(
            cursor_chat_in(config.path(), pid, Some(cwd), Some("1a-running"), true),
            Some("1a-running".into())
        );
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn a_chat_resumes_as_cursor_agent_with_its_flags() {
        let process = ProcessInfo {
            pid: u32::MAX,
            name: "MainThread".into(),
            cmdline: args(&[
                "/home/me/.local/bin/agent",
                "--use-system-ca",
                "/home/me/.local/share/cursor-agent/versions/2026.10.22-27dcf5f/index.js",
                "--model",
                "sonnet-4-thinking",
                "--force",
                "--api-key",
                "secret",
                "fix the bug",
            ]),
            exe: Some(PathBuf::from(
                "/home/me/.local/share/cursor-agent/versions/2026.10.22-27dcf5f/node",
            )),
        };
        assert_eq!(
            plan(&Context {
                process: &process,
                cwd: None,
                bound_conversation: Some("0f8a3c1e-7b2d-4e5f-9a6b-1c2d3e4f5a6b"),
            }),
            Plan::Resume(args(&[
                "cursor-agent",
                "--model",
                "sonnet-4-thinking",
                "--force",
                "--resume",
                "0f8a3c1e-7b2d-4e5f-9a6b-1c2d3e4f5a6b"
            ]))
        );
        // Not known yet: what was recorded stays.
        assert_eq!(cursor_plan(&process, None, None), Plan::Unresolved);
        assert_eq!(
            cursor_plan(&process, None, Some("bad; id")),
            Plan::Unresolved
        );
    }

    #[test]
    fn commands_that_arent_chats_are_told_apart_from_prompts() {
        let first = |words: &[&str]| first_word(&args(words)).map(String::from);
        assert_eq!(first(&["login"]), Some("login".into()));
        assert_eq!(
            first(&["--model", "gpt-5", "mcp", "list"]),
            Some("mcp".into())
        );
        assert_eq!(first(&["--resume", "abc", "status"]), Some("status".into()));
        assert_eq!(first(&["--resume", "--yolo"]), None);
        assert_eq!(
            first(&["-w", "fix", "update the docs"]),
            Some("update the docs".into())
        );
        assert_eq!(first(&["--", "login"]), Some("login".into()));
        let process = ProcessInfo {
            pid: 1,
            name: "MainThread".into(),
            cmdline: args(&[
                "/home/me/.local/bin/agent",
                "--use-system-ca",
                "/x/cursor-agent/versions/1/index.js",
                "login",
            ]),
            exe: None,
        };
        assert_eq!(
            cursor_plan(&process, None, Some("1b2c")),
            Plan::Command(args(&["agent", "login"]))
        );
    }
}
