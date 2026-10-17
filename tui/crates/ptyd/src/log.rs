//! Minimal file logger: `~/.orchestra/logs/ptyd.log`, size-rotated, level
//! from `ORCHESTRA_PTYD_LOG` (error|warn|info|debug; default info).
//!
//! Lines look like `2026-10-13T08:15:02.123Z INFO  [thread] message`. When
//! the file would grow past 1 MiB it is renamed to `ptyd.log.1` (replacing
//! the previous one) and a fresh file is started. Logging never panics and
//! silently drops lines it cannot write.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Environment variable selecting the log level.
pub const LEVEL_ENV: &str = "ORCHESTRA_PTYD_LOG";

/// Rotate when the file would grow past this size.
pub const MAX_LOG_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

impl Level {
    /// Parse a level name (case-insensitive). `None` for unknown names.
    pub fn parse(value: &str) -> Option<Level> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Level::Error),
            "warn" | "warning" => Some(Level::Warn),
            "info" => Some(Level::Info),
            "debug" => Some(Level::Debug),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Level::Error => "ERROR",
            Level::Warn => "WARN",
            Level::Info => "INFO",
            Level::Debug => "DEBUG",
        }
    }
}

/// A size-rotated log file.
struct FileLog {
    level: Level,
    path: PathBuf,
    max_bytes: u64,
    /// The open file and its current size; `None` after a failed (re)open,
    /// retried on the next line.
    file: Mutex<Option<(File, u64)>>,
}

impl FileLog {
    fn new(path: PathBuf, level: Level, max_bytes: u64) -> Self {
        let file = open_log(&path).ok();
        Self {
            level,
            path,
            max_bytes,
            file: Mutex::new(file),
        }
    }

    fn enabled(&self, level: Level) -> bool {
        level <= self.level
    }

    fn write(&self, level: Level, message: &str) {
        if !self.enabled(level) {
            return;
        }
        let thread = std::thread::current();
        let line = format!(
            "{} {:<5} [{}] {}\n",
            utc_timestamp(SystemTime::now()),
            level.label(),
            thread.name().unwrap_or("-"),
            message.trim_end()
        );
        let mut guard = self.file.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = open_log(&self.path).ok();
        }
        let needs_rotation = guard
            .as_ref()
            .is_some_and(|(_, size)| *size > 0 && size + line.len() as u64 > self.max_bytes);
        if needs_rotation {
            *guard = None;
            let _ = std::fs::rename(&self.path, rotated_path(&self.path));
            *guard = open_log(&self.path).ok();
        }
        if let Some((file, size)) = guard.as_mut() {
            if file.write_all(line.as_bytes()).is_ok() {
                *size += line.len() as u64;
            }
        }
    }
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

/// Open for appending (0600), creating the directory (0700) if needed.
fn open_log(path: &Path) -> io::Result<(File, u64)> {
    if let Some(dir) = path.parent() {
        if !dir.exists() {
            std::fs::create_dir_all(dir)?;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    let size = file.metadata()?.len();
    Ok((file, size))
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` in UTC.
fn utc_timestamp(at: SystemTime) -> String {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = since.as_secs();
    let (days, rem) = ((secs / 86_400) as i64, secs % 86_400);
    // Civil date from days since 1970-01-01 (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
        since.subsec_millis()
    )
}

static LOGGER: OnceLock<FileLog> = OnceLock::new();

/// Initialise the global logger. Safe to call once; later calls are ignored.
pub fn init(layout: &session_core::paths::Layout) {
    let level = std::env::var(LEVEL_ENV)
        .ok()
        .and_then(|v| Level::parse(&v))
        .unwrap_or(Level::Info);
    let _ = LOGGER.set(FileLog::new(layout.log_file(), level, MAX_LOG_BYTES));
}

/// Whether a line at `level` would be written.
pub fn enabled(level: Level) -> bool {
    LOGGER.get().is_some_and(|log| log.enabled(level))
}

/// Write one line if `level` is enabled. Never panics, never blocks long.
pub fn write(level: Level, message: &str) {
    if let Some(log) = LOGGER.get() {
        log.write(level, message);
    }
}

#[macro_export]
macro_rules! log_info { ($($t:tt)*) => { $crate::log::write($crate::log::Level::Info, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_warn { ($($t:tt)*) => { $crate::log::write($crate::log::Level::Warn, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_error { ($($t:tt)*) => { $crate::log::write($crate::log::Level::Error, &format!($($t)*)) } }
#[macro_export]
macro_rules! log_debug { ($($t:tt)*) => { if $crate::log::enabled($crate::log::Level::Debug) { $crate::log::write($crate::log::Level::Debug, &format!($($t)*)) } } }

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn levels_parse_and_filter() {
        assert_eq!(Level::parse("DEBUG"), Some(Level::Debug));
        assert_eq!(Level::parse(" warn "), Some(Level::Warn));
        assert_eq!(Level::parse("loud"), None);
        let dir = tempfile::tempdir().unwrap();
        let log = FileLog::new(dir.path().join("logs/ptyd.log"), Level::Warn, 1024);
        log.write(Level::Info, "hidden");
        log.write(Level::Error, "shown");
        let text = std::fs::read_to_string(dir.path().join("logs/ptyd.log")).unwrap();
        assert!(!text.contains("hidden"));
        assert!(text.contains("ERROR"));
        assert!(text.trim_end().ends_with("shown"));
    }

    #[test]
    fn files_are_private_and_rotate_keeping_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs/ptyd.log");
        let log = FileLog::new(path.clone(), Level::Debug, 200);
        for i in 0..10 {
            log.write(Level::Info, &format!("line {i} {}", "x".repeat(40)));
        }
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        let current = std::fs::read_to_string(&path).unwrap();
        let previous = std::fs::read_to_string(rotated_path(&path)).unwrap();
        assert!(current.len() <= 200 && previous.len() <= 200);
        assert!(current.contains("line 9"));
        assert!(!current.contains("line 0") && !previous.contains("line 0"));
        assert!(!dir.path().join("logs/ptyd.log.2").exists());
    }

    #[test]
    fn timestamps_are_utc_iso8601() {
        let at = UNIX_EPOCH + Duration::from_millis(1_791_879_302_123);
        assert_eq!(utc_timestamp(at), "2026-10-13T08:15:02.123Z");
        assert_eq!(utc_timestamp(UNIX_EPOCH), "1970-01-01T00:00:00.000Z");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400); // 2000-02-29
        assert_eq!(utc_timestamp(leap), "2000-02-29T00:00:00.000Z");
    }
}
