//! What this machine did with the signed commands rakumux.com brought: each
//! one accepted (a paired device signed what was asked) or refused, with the
//! device, the kind and the session, never what a reply says. Kept in
//! `sensagent/signed-commands.log` (JSON lines, 0600; the newest 1 MiB, the
//! one before in `.1`), read by `rakumux devices log`.

use std::collections::VecDeque;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::Refusal;

/// The file's name, beside the paired devices'.
pub const FILE_NAME: &str = "signed-commands.log";
/// Past this size the file moves to `.1` and a new one starts.
const MAX_BYTES: u64 = 1024 * 1024;
/// How many commands' outcomes are remembered, so one checked at every read
/// until it's typed in is written once.
const REMEMBERED: usize = 512;

/// What became of a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Signed by a paired device, saying what was asked: carried out (a
    /// reply is then typed in when its session takes it).
    Accepted,
    Unsigned,
    UnknownDevice,
    BadSignature,
    Expired,
}

impl From<Refusal> for Outcome {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Unsigned => Self::Unsigned,
            Refusal::UnknownDevice => Self::UnknownDevice,
            Refusal::BadSignature => Self::BadSignature,
            Refusal::Expired => Self::Expired,
        }
    }
}

/// One command's outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogEntry {
    /// When it was checked (ms since the epoch).
    pub at: i64,
    /// The command's kind (`reply`, `new_session`, `delete_session`,
    /// `rename_session`, `update`).
    pub kind: String,
    /// rakumux.com's id for the request.
    pub id: String,
    pub outcome: Outcome,
    /// The key id of the device that signed it, when it was signed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    /// That device's name, when it's paired here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The session it was for, when it names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}

/// The log; memory only without a path.
#[derive(Default)]
pub struct SignedLog {
    path: Option<PathBuf>,
    written: VecDeque<(String, Outcome)>,
}

impl SignedLog {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            path,
            written: VecDeque::new(),
        }
    }

    /// Write `entry`, unless that command's outcome was written already.
    pub fn record(&mut self, entry: LogEntry) {
        let key = (entry.id.clone(), entry.outcome);
        if self.written.contains(&key) {
            return;
        }
        if self.written.len() >= REMEMBERED {
            self.written.pop_front();
        }
        self.written.push_back(key);
        let Some(path) = &self.path else {
            return;
        };
        if append(path, &entry).is_err() {
            eprintln!("rakumux-status: writing the signed-command log failed");
        }
    }
}

fn append(path: &Path, entry: &LogEntry) -> std::io::Result<()> {
    if std::fs::metadata(path).is_ok_and(|meta| meta.len() >= MAX_BYTES) {
        std::fs::rename(path, older(path))?;
    }
    let mut line = serde_json::to_vec(entry).map_err(std::io::Error::other)?;
    line.push(b'\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(&line)
}

fn older(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// The log kept at `path`, oldest first; lines it can't read are left out.
pub fn read(path: &Path) -> Vec<LogEntry> {
    [older(path), path.to_path_buf()]
        .iter()
        .filter_map(|path| std::fs::read_to_string(path).ok())
        .flat_map(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect::<Vec<LogEntry>>()
        })
        .collect()
}
