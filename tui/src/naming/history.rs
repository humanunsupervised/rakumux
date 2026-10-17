//! Reading the per-session command history written by
//! `shell/rakumux-command-hook.sh` (`session_core::paths::Layout::history_file`).

use super::prompt::HISTORY_LIMIT;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

/// Only the end of a history file is read; 64 KiB holds far more than
/// [`HISTORY_LIMIT`] commands.
const TAIL_BYTES: u64 = 64 * 1024;

/// The most recent commands from a history file (at most 50, oldest first).
///
/// Lines are `<timestamp>\t<command>`; a line without a tab is taken as a
/// bare command. Blank commands are skipped. A missing file is an empty
/// history.
pub fn load_history(path: &Path) -> io::Result<Vec<String>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };
    let start = file.metadata()?.len().saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut raw = Vec::new();
    file.read_to_end(&mut raw)?;
    Ok(parse_history(&String::from_utf8_lossy(&raw), start > 0))
}

fn parse_history(text: &str, starts_mid_line: bool) -> Vec<String> {
    let mut lines = text.lines();
    if starts_mid_line {
        // The read began somewhere inside a line; drop the fragment.
        lines.next();
    }
    let commands: Vec<String> = lines
        .map(|line| line.split_once('\t').map_or(line, |(_, command)| command))
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_string)
        .collect();
    let skip = commands.len().saturating_sub(HISTORY_LIMIT);
    commands.into_iter().skip(skip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn missing_file_is_empty_history() {
        let dir = tempfile::tempdir().unwrap();
        let history = load_history(&dir.path().join("history").join("01ABC.log")).unwrap();
        assert!(history.is_empty());
    }

    #[test]
    fn reads_hook_lines_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("01ABC.log");
        std::fs::write(
            &path,
            "2026-10-13T10:00:00+0000\tcargo build\n\
             2026-10-13T10:00:05+0000\tgit status\n\
             legacy line without timestamp\n\
             2026-10-13T10:00:09+0000\t   \n\
             2026-10-13T10:01:00+0000\techo 'a\tb'\n",
        )
        .unwrap();
        assert_eq!(
            load_history(&path).unwrap(),
            vec![
                "cargo build",
                "git status",
                "legacy line without timestamp",
                "echo 'a\tb'",
            ]
        );
    }

    #[test]
    fn keeps_the_last_fifty_commands() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("01ABC.log");
        let mut file = File::create(&path).unwrap();
        for i in 0..120 {
            writeln!(file, "2026-10-13T10:00:00+0000\tcmd{i}").unwrap();
        }
        let history = load_history(&path).unwrap();
        assert_eq!(history.len(), HISTORY_LIMIT);
        assert_eq!(history.first().map(String::as_str), Some("cmd70"));
        assert_eq!(history.last().map(String::as_str), Some("cmd119"));
    }

    #[test]
    fn large_files_are_read_from_the_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("01ABC.log");
        let mut file = File::create(&path).unwrap();
        let padding = "x".repeat(200);
        for i in 0..2_000 {
            writeln!(file, "2026-10-13T10:00:00+0000\told{i} {padding}").unwrap();
        }
        writeln!(file, "2026-10-13T10:00:00+0000\tlast command").unwrap();
        drop(file);
        assert!(std::fs::metadata(&path).unwrap().len() > TAIL_BYTES);

        let history = load_history(&path).unwrap();
        assert_eq!(history.len(), HISTORY_LIMIT);
        assert_eq!(history.last().map(String::as_str), Some("last command"));
        assert!(history
            .iter()
            .all(|c| c.starts_with("old") || c == "last command"));
    }

    #[test]
    fn mid_line_fragment_is_dropped() {
        assert_eq!(
            parse_history("tail of a cut line\n2026\tls\n", true),
            vec!["ls"]
        );
        assert_eq!(
            parse_history("2026\tpwd\n2026\tls\n", false),
            vec!["pwd", "ls"]
        );
    }
}
