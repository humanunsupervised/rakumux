//! Small append-only log files under `<home>/logs`, for the programs that have no logger of their
//! own (the TUI, the MCP server). Lines read as the daemon's do:
//! `2026-10-13T08:15:02.123Z ERROR [component] message`. Logging never fails its caller.

use crate::paths::Layout;
use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Rotate a file past this size, to `<name>.1`.
const MAX_BYTES: u64 = 1024 * 1024;

/// Severity of a line, as the daemon's log names them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
        }
    }
}

/// Where a program's log file is.
pub fn path(layout: &Layout, name: &str) -> PathBuf {
    layout.home.join("logs").join(name)
}

/// Append one line to `<home>/logs/<name>` (0600, its directory 0700), its newlines replaced so it
/// stays one line.
pub fn append(layout: &Layout, name: &str, level: Level, component: &str, message: &str) {
    let path = path(layout, name);
    if let Some(directory) = path.parent() {
        if !directory.exists() && std::fs::create_dir_all(directory).is_ok() {
            let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
        }
    }
    if std::fs::metadata(&path).is_ok_and(|file| file.len() > MAX_BYTES) {
        let mut rotated = path.clone().into_os_string();
        rotated.push(".1");
        let _ = std::fs::rename(&path, rotated);
    }
    let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&path)
    else {
        return;
    };
    let line = format!(
        "{} {} [{component}] {}\n",
        timestamp(SystemTime::now()),
        level.label(),
        message.trim_end().replace(['\n', '\r'], " ")
    );
    let _ = file.write_all(line.as_bytes());
}

fn timestamp(now: SystemTime) -> String {
    let elapsed = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = elapsed.as_secs() as i64;
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let of_day = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        of_day / 3_600,
        of_day % 3_600 / 60,
        of_day % 60,
        elapsed.subsec_millis()
    )
}

/// The proleptic Gregorian date of a day count from 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let of_era = shifted.rem_euclid(146_097);
    let year_of_era = (of_era - of_era / 1_460 + of_era / 36_524 - of_era / 146_096) / 365;
    let of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * of_year + 2) / 153;
    let day = (of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    } as u32;
    (year_of_era + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_one_private_line_and_rotates() {
        let home = std::env::temp_dir().join(format!(
            "logfile-{}",
            crate::ids::SessionId::generate().as_str()
        ));
        let layout = Layout::with_home(home.clone());
        append(
            &layout,
            "tui.log",
            Level::Error,
            "panic",
            "boom\nsecond line\n",
        );
        let written = std::fs::read_to_string(path(&layout, "tui.log")).unwrap();
        assert!(
            written.ends_with(" ERROR [panic] boom second line\n"),
            "{written}"
        );
        assert_eq!(written.lines().count(), 1);
        let mode = std::fs::metadata(path(&layout, "tui.log"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        std::fs::write(path(&layout, "tui.log"), vec![b'x'; MAX_BYTES as usize + 1]).unwrap();
        append(&layout, "tui.log", Level::Warn, "x", "fresh");
        assert!(std::fs::metadata(home.join("logs/tui.log.1")).is_ok());
        assert!(std::fs::read_to_string(path(&layout, "tui.log"))
            .unwrap()
            .contains(" WARN [x] fresh"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn dates_are_civil() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20_393), (2025, 11, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
    }
}
