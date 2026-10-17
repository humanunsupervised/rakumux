//! Minimal logging to stderr (a detached API's stderr is `logs/api.log`).
//! `ORCHESTRA_API_LOG=debug` adds one line per request. Tokens are never
//! logged.

use crate::time::rfc3339;
use std::fmt::Display;
use std::sync::atomic::{AtomicBool, Ordering};

static DEBUG: AtomicBool = AtomicBool::new(false);

/// Read the log level from the environment.
pub fn init() {
    let debug = std::env::var(crate::env::API_LOG).is_ok_and(|level| level == "debug");
    DEBUG.store(debug, Ordering::Relaxed);
}

pub fn debug_enabled() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

pub fn info(message: impl Display) {
    write("INFO", message);
}

pub fn warn(message: impl Display) {
    write("WARN", message);
}

pub fn debug(message: impl Display) {
    if debug_enabled() {
        write("DEBUG", message);
    }
}

fn write(level: &str, message: impl Display) {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    eprintln!("{} {level} {message}", rfc3339(now));
}
