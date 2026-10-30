//! Where a coding agent works, from its own conversation on this machine:
//! the directories it worked in (its directory as the log records it, a
//! `cd`, a search's root, a shell call's working directory) and the files it
//! read or changed, each as a [`Touch`] of a directory (`SessionView::places`).
//! Only paths are taken: never file contents, commands' output or prompts.
//! The control layer turns touches into places: the git working trees they
//! are in (a touch outside any repository is no place), listing only those
//! where a touch changed files (`edited`: a file tool's write or edit, a
//! patch). Other shell commands' writes (`sed -i`, a redirect) aren't seen.
//!
//! A conversation is read from its start, a piece at a time ([`PlaceScan`]),
//! then on as it grows. Provider mappings live beside their hooks in
//! `integrations/<agent>`:
//! - **Claude Code**: each entry's `cwd`, and its tool calls' file paths,
//!   search roots, worktrees and Bash commands.
//! - **Codex**: `session_meta` and `turn_context`'s `cwd`, shell calls'
//!   `workdir` and commands, and `apply_patch`'s files.
//! - **OpenCode**: its tool calls' file paths, search roots and commands,
//!   from the conversation [`super::transcript`] reads.
//! - **Pi**: its conversation's folder (the header's `cwd`), its tool calls'
//!   paths and commands, and the calls codemode scripts made.
//! - **GitHub Copilot**: the session's directory as it starts, resumes and
//!   `/cd`s, its tools' paths and shell commands, and the files they changed
//!   (`fileEdits`).
//! - **Cursor**: its tool calls' file paths, search roots, and Shell calls'
//!   directories and commands, relative to the workspace.
//! - **Grok Build**: its finished edits' files, its reads and searches, its
//!   shell calls' folders and commands, and its finished subagents' own logs.
//! - **Kimi Code**: its tool calls' files, search roots and commands with
//!   their working directories, and the files it tracks before changing
//!   them (its subagents' too).
//! - **Droid**: its session's directories (the summary's `cwd` and
//!   `lastCwd`), and its tool calls' file paths, search roots, patches and
//!   `Execute` commands.
//! - **Amp**: each message's workspace, and its tool calls' file paths (the
//!   files Amp says they change, when the plugin recorded them), search
//!   roots and commands, from the copy of the thread its plugin keeps.

use super::transcript::{scan, TranscriptSource};
use super::LogAgent;
use serde_json::Value;
use session_core::api::{TranscriptCursor, TranscriptPart};
use std::path::{Component, Path, PathBuf};

#[path = "amp/places.rs"]
mod amp;
#[path = "claude-code/places.rs"]
mod claude;
#[path = "codex/places.rs"]
mod codex;
#[path = "copilot/places.rs"]
mod copilot;
#[path = "cursor-agent/places.rs"]
mod cursor;
#[path = "droid/places.rs"]
mod droid;
#[path = "grok/places.rs"]
mod grok;
#[path = "kimi/places.rs"]
mod kimi;
#[path = "open-code/places.rs"]
mod opencode;
#[path = "pi/places.rs"]
mod pi;

/// The longest path taken, in bytes.
const MAX_PATH_BYTES: usize = 4096;

/// A directory a conversation shows its agent working in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Touch {
    /// Absolute and lexically normalized, not canonical (it may not exist
    /// any more).
    pub dir: PathBuf,
    /// A file there was changed (written or edited).
    pub edited: bool,
    /// Unix milliseconds, when the conversation says.
    pub at: Option<i64>,
}

/// Reads one conversation's touches, from its start, a piece at a time.
#[derive(Debug)]
pub struct PlaceScan {
    source: TranscriptSource,
    /// How far it was read: a log's offset, or OpenCode's watermark.
    cursor: Option<TranscriptCursor>,
    /// The agent's directory as of the cursor: relative paths are its.
    cwd: Option<PathBuf>,
    home: Option<PathBuf>,
    /// The last read stopped before the end.
    behind: bool,
    /// The log file read, for one its agent rewrites (GitHub Copilot's).
    file: Option<(u64, u64)>,
}

impl PlaceScan {
    /// A scan of `source` from its start. `cwd` is the agent process's
    /// directory, for relative paths until the conversation names its own.
    pub fn new(source: TranscriptSource, cwd: Option<PathBuf>) -> Self {
        Self {
            source,
            cursor: None,
            cwd,
            home: std::env::var_os("HOME")
                .filter(|home| !home.is_empty())
                .map(PathBuf::from),
            behind: true,
            file: None,
        }
    }

    /// The conversation scanned.
    pub fn source(&self) -> &TranscriptSource {
        &self.source
    }

    /// Whether some of the conversation is still unread: a long one is read
    /// a piece at a time.
    pub fn behind(&self) -> bool {
        self.behind
    }

    /// The touches of what follows the last read, reading about `budget`
    /// bytes. Nothing when the log or database can't be read.
    pub fn read(&mut self, budget: usize) -> Vec<Touch> {
        let mut touches = Vec::new();
        match &self.source {
            TranscriptSource::Log {
                agent,
                conversation,
                path,
            } => {
                // GitHub Copilot rewrites its log when a turn is taken back:
                // another file is read from its start.
                if *agent == LogAgent::Copilot {
                    let file = std::fs::metadata(path)
                        .ok()
                        .map(|metadata| super::copilot::identity(&metadata));
                    if self.file != file {
                        self.cursor = None;
                    }
                    self.file = file;
                }
                let from = self.cursor.as_ref().map_or(0, |cursor| cursor.offset);
                let (cwd, home) = (&mut self.cwd, self.home.as_deref());
                // Cursor's lines have no times: they're as old as the file.
                let written = match agent {
                    LogAgent::Cursor => super::cursor::modified_ms(path),
                    _ => None,
                };
                let read = scan(*agent, path, from, budget, |entry, _| {
                    let at = super::entry_time(*agent, entry).or(written);
                    match agent {
                        LogAgent::Claude => claude::entry(entry, cwd, home, at, &mut touches),
                        LogAgent::Codex => codex::entry(entry, cwd, home, at, &mut touches),
                        LogAgent::Pi => pi::entry(entry, cwd, home, at, &mut touches),
                        LogAgent::Copilot => copilot::entry(entry, cwd, home, at, &mut touches),
                        LogAgent::Cursor => cursor::entry(entry, cwd, home, at, &mut touches),
                        LogAgent::Grok => grok::entry(entry, cwd, home, at, &mut touches),
                        LogAgent::Kimi => kimi::entry(entry, cwd, home, at, &mut touches),
                        LogAgent::Droid => droid::entry(entry, cwd, home, at, &mut touches),
                        LogAgent::Amp => amp::entry(entry, cwd, home, at, &mut touches),
                    }
                });
                let Some((offset, more)) = read else {
                    return Vec::new();
                };
                self.cursor = Some(TranscriptCursor {
                    conversation: conversation.clone(),
                    offset,
                    key: None,
                    subagents: Vec::new(),
                });
                self.behind = more;
            }
            TranscriptSource::OpenCode { .. } => {
                let budget = u32::try_from(budget).unwrap_or(u32::MAX);
                let Some(page) = self.source.read(self.cursor.as_ref(), Some(budget), false) else {
                    return Vec::new();
                };
                for message in &page.messages {
                    let mut collect = Collect::new(self.cwd.as_deref(), self.home.as_deref());
                    collect.at = message.at;
                    for part in &message.parts {
                        if let TranscriptPart::ToolCall { name, input, .. } = part {
                            let input = serde_json::from_str(input).unwrap_or(Value::Null);
                            opencode::call(name, &input, &mut collect);
                        }
                    }
                    touches.extend(collect.touches);
                }
                self.cursor = Some(page.cursor);
                self.behind = page.more;
            }
        }
        touches
    }
}

/// The directories of the agents' own data, never places: what they keep
/// for themselves (settings, memory, logs, scratch space), not the user's
/// work. OpenCode's worktrees under its data directory are working trees of
/// the user's repositories, which the control layer tells apart.
pub fn agent_data_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = [
        crate::resume::claude_config_dir(),
        crate::resume::codex_home(),
        crate::resume::copilot_home(),
        crate::resume::opencode_database().and_then(|db| db.parent().map(Path::to_path_buf)),
        crate::resume::pi_agent_dir(),
        crate::resume::cursor_config_dir(),
        crate::resume::cursor_data_dir(),
        // Grok's worktrees there are the user's working trees.
        crate::resume::grok_home(),
        crate::resume::kimi_home(),
        // Its worktrees (`droid -w`, `~/.factory/worktrees`) are the user's.
        crate::resume::droid_home(),
    ]
    .into_iter()
    .flatten()
    .chain(crate::resume::amp_data_dirs())
    .collect();
    // Claude Code's per-user scratch space: task output, scratchpads.
    // SAFETY: getuid has no preconditions and can't fail.
    let uid = unsafe { libc::getuid() };
    dirs.push(std::env::temp_dir().join(format!("claude-{uid}")));
    dirs.push(PathBuf::from("/tmp").join(format!("claude-{uid}")));
    dirs.sort();
    dirs.dedup();
    dirs
}

/// What one entry shows, its paths resolved against the agent's directory.
pub(super) struct Collect<'a> {
    cwd: Option<&'a Path>,
    home: Option<&'a Path>,
    pub(super) at: Option<i64>,
    pub(super) touches: Vec<Touch>,
}

impl<'a> Collect<'a> {
    pub(super) fn new(cwd: Option<&'a Path>, home: Option<&'a Path>) -> Self {
        Self {
            cwd,
            home,
            at: None,
            touches: Vec::new(),
        }
    }

    /// A directory the agent worked in (its directory, a search's root).
    pub(super) fn dir(&mut self, raw: &str) {
        if let Some(dir) = resolve(raw, self.cwd, self.home) {
            self.push(dir, false);
        }
    }

    /// A file the agent read (`edited`: changed): its directory.
    pub(super) fn file(&mut self, raw: &str, edited: bool) {
        let dir =
            resolve(raw, self.cwd, self.home).and_then(|file| file.parent().map(Path::to_path_buf));
        if let Some(dir) = dir {
            self.push(dir, edited);
        }
    }

    /// The directories a shell command works in: each `cd` or `pushd`
    /// (later relative ones from there), and each `git -C`. Words with
    /// expansions (`$HOME`, `` `pwd` ``) can't be known and are passed.
    pub(super) fn command(&mut self, command: &str) {
        self.command_in(command, self.cwd);
    }

    /// [`Collect::command`] for a command run in `dir` (a shell call's own
    /// working directory).
    pub(super) fn command_in(&mut self, command: &str, dir: Option<&Path>) {
        let mut cwd = dir.map(Path::to_path_buf);
        for words in commands(command) {
            let words: Vec<&str> = words
                .iter()
                .map(String::as_str)
                .skip_while(|word| is_assignment(word))
                .collect();
            match words.as_slice() {
                ["cd" | "pushd", dir, ..] if !dir.starts_with('-') => {
                    let Some(dir) =
                        known(dir).and_then(|dir| resolve(dir, cwd.as_deref(), self.home))
                    else {
                        // Somewhere unknown: later relative paths are too.
                        cwd = None;
                        continue;
                    };
                    self.push(dir.clone(), false);
                    cwd = Some(dir);
                }
                ["git", options @ ..] => {
                    // Git's own options, before its subcommand.
                    let mut options = options.iter();
                    while let Some(option) = options.next() {
                        match *option {
                            "-C" => {
                                let dir = options
                                    .next()
                                    .and_then(|dir| known(dir))
                                    .and_then(|dir| resolve(dir, cwd.as_deref(), self.home));
                                if let Some(dir) = dir {
                                    self.push(dir, false);
                                }
                            }
                            "-c" | "--git-dir" | "--work-tree" | "--namespace" => {
                                options.next();
                            }
                            option if option.starts_with('-') => {}
                            _ => break,
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// The files an `apply_patch` (Codex) or `patch` (OpenCode) changes,
    /// from its `*** Add File:`, `*** Update File:`, `*** Delete File:` and
    /// `*** Move to:` lines.
    pub(super) fn patch(&mut self, patch: &str) {
        self.patch_in(patch, self.cwd);
    }

    /// [`Collect::patch`] for a patch applied in `dir` (a shell call's own
    /// working directory).
    pub(super) fn patch_in(&mut self, patch: &str, dir: Option<&Path>) {
        for line in patch.lines() {
            let file = [
                "*** Add File:",
                "*** Update File:",
                "*** Delete File:",
                "*** Move to:",
            ]
            .iter()
            .find_map(|header| line.strip_prefix(header));
            let dir = file
                .and_then(|file| resolve(file, dir, self.home))
                .and_then(|file| file.parent().map(Path::to_path_buf));
            if let Some(dir) = dir {
                self.push(dir, true);
            }
        }
    }

    /// A shell call's working directory: relative paths of its command are
    /// its. Returns it.
    pub(super) fn workdir(&mut self, raw: &str) -> Option<PathBuf> {
        let dir = resolve(raw, self.cwd, self.home)?;
        self.push(dir.clone(), false);
        Some(dir)
    }

    fn push(&mut self, dir: PathBuf, edited: bool) {
        self.touches.push(Touch {
            dir,
            edited,
            at: self.at,
        });
    }
}

/// `raw` as an absolute path, lexically normalized: `~` is `home`, a
/// relative path is `cwd`'s. `None` for what isn't a path (empty, a URL,
/// control characters, too long) or a relative path without a `cwd`.
pub(super) fn resolve(raw: &str, cwd: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty()
        || raw.len() > MAX_PATH_BYTES
        || raw.contains("://")
        || raw.chars().any(char::is_control)
    {
        return None;
    }
    let path = if raw == "~" {
        home?.to_path_buf()
    } else if let Some(rest) = raw.strip_prefix("~/") {
        home?.join(rest)
    } else {
        PathBuf::from(raw)
    };
    let path = if path.is_absolute() {
        path
    } else {
        cwd?.join(path)
    };
    Some(normalize(&path))
}

/// Resolve `.` and `..` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// A word that names a known directory: not one with an expansion.
fn known(word: &str) -> Option<&str> {
    (!word.contains(['$', '`', '*', '?'])).then_some(word)
}

/// `NAME=value`: an assignment before a command.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// A shell command line as its simple commands' words, split at `;`, `&`,
/// `|`, newlines and parentheses outside quotes. Quotes are taken off and
/// backslashes escape; anything fancier stays in its word.
fn commands(line: &str) -> Vec<Vec<String>> {
    let mut commands = Vec::new();
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(open), c) if c == open => quote = None,
            (Some('"'), '\\') => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            (Some(_), c) => word.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                if let Some(next) = chars.next() {
                    if next != '\n' {
                        word.push(next);
                        in_word = true;
                    }
                }
            }
            (None, ';' | '&' | '|' | '\n' | '(' | ')') => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
                if !words.is_empty() {
                    commands.push(std::mem::take(&mut words));
                }
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            (None, c) => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    if !words.is_empty() {
        commands.push(words);
    }
    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn dirs(collect: &Collect) -> Vec<String> {
        collect
            .touches
            .iter()
            .map(|touch| {
                format!(
                    "{}{}",
                    touch.dir.display(),
                    if touch.edited { " (edited)" } else { "" }
                )
            })
            .collect()
    }

    #[test]
    fn paths_resolve_against_the_agents_directory_and_home() {
        let (cwd, home) = (Path::new("/repo/tui"), Path::new("/home/me"));
        let resolve = |raw| resolve(raw, Some(cwd), Some(home));
        assert_eq!(resolve("src/app.rs"), Some("/repo/tui/src/app.rs".into()));
        assert_eq!(resolve("../landing"), Some("/repo/landing".into()));
        assert_eq!(resolve("/etc/./hosts"), Some("/etc/hosts".into()));
        assert_eq!(resolve("~/repos/x"), Some("/home/me/repos/x".into()));
        assert_eq!(resolve("~"), Some("/home/me".into()));
        assert_eq!(resolve("https://example.com/a"), None);
        assert_eq!(resolve("  "), None);
        assert_eq!(resolve("a\u{1b}b"), None);
        assert_eq!(super::resolve("relative", None, Some(home)), None);
    }

    #[test]
    fn a_commands_directories_are_its_cds_and_git_dashes_c() {
        let mut collect = Collect::new(Some(Path::new("/repo")), Some(Path::new("/home/me")));
        collect.command(
            "cd tui && cargo test; git -C ../other status | head\n\
             FOO=1 cd \"/a b\" && (pushd sub) && git log -C x",
        );
        assert_eq!(
            dirs(&collect),
            ["/repo/tui", "/repo/other", "/a b", "/a b/sub"]
        );
        // Expansions can't be known, and leave later relative paths unknown.
        let mut collect = Collect::new(Some(Path::new("/repo")), None);
        collect.command("cd $WORK && cd sub; cd - ; cd `pwd`; git -C /abs log");
        assert_eq!(dirs(&collect), ["/abs"]);
        assert_eq!(
            commands("echo 'a;b' \"c\\\"d\" e\\ f"),
            [vec![
                "echo".to_string(),
                "a;b".into(),
                "c\"d".into(),
                "e f".into()
            ]]
        );
    }

    #[test]
    fn a_patch_changes_the_files_it_names() {
        let mut collect = Collect::new(Some(Path::new("/repo")), None);
        collect.patch(
            "*** Begin Patch\n*** Update File: src/a.rs\n@@\n-x\n+y\n\
             *** Add File: /other/new.rs\n+fn main() {}\n*** Delete File: old/b.rs\n*** End Patch",
        );
        assert_eq!(
            dirs(&collect),
            [
                "/repo/src (edited)",
                "/other (edited)",
                "/repo/old (edited)"
            ]
        );
    }

    #[test]
    fn a_long_log_is_scanned_in_pieces_and_then_on_as_it_grows() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("c.jsonl");
        let mut file = std::fs::File::create(&path).unwrap();
        let entry = |cwd: &str| {
            serde_json::json!({"type": "user", "cwd": cwd,
                "timestamp": "2026-10-23T10:00:00Z", "message": {"content": "hi"}})
        };
        for n in 0..50 {
            writeln!(file, "{}", entry(&format!("/w/{n}"))).unwrap();
        }
        let source = TranscriptSource::Log {
            agent: LogAgent::Claude,
            conversation: "c".into(),
            path,
        };
        let mut scan = PlaceScan::new(source, None);
        let mut seen = Vec::new();
        let mut reads = 0;
        while scan.behind() {
            seen.extend(scan.read(512));
            reads += 1;
            assert!(reads < 100);
        }
        assert!(reads > 1, "read in pieces");
        assert_eq!(seen.len(), 50);
        assert_eq!(seen[0].dir, Path::new("/w/0"));
        assert_eq!(seen[0].at, Some(1_792_749_600_000));
        assert!(scan.read(512).is_empty());
        writeln!(file, "{}", entry("/w/new")).unwrap();
        assert_eq!(scan.read(512)[0].dir, Path::new("/w/new"));
    }
}
