//! Identifiers and secrets.

use serde::{Deserialize, Serialize};
use std::fmt;

const CROCKFORD: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    getrandom::getrandom(&mut buf).expect("OS random source unavailable");
    buf
}

/// Stable session identifier: a ULID (48-bit millisecond timestamp + 80
/// random bits, Crockford base32), so ids sort by creation time.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    pub fn generate() -> Self {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u128)
            .unwrap_or(0)
            & ((1u128 << 48) - 1);
        let random = u128::from_be_bytes({
            let mut b = [0u8; 16];
            b[6..].copy_from_slice(&random_bytes::<10>());
            b
        });
        let mut value = (millis << 80) | random;
        let mut out = [0u8; 26];
        for slot in out.iter_mut().rev() {
            *slot = CROCKFORD[(value & 31) as usize];
            value >>= 5;
        }
        Self(String::from_utf8(out.to_vec()).expect("ascii"))
    }

    /// Wrap an id received from the wire or the registry. Ids are opaque;
    /// callers must not derive them.
    pub fn from_string(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Opaque identity of a temporary terminal pane, never a registry session.
/// The client keeps it only for its window's lifetime and reconnects.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(String);

impl PaneId {
    /// Generate a fresh identity that cannot alias a pane after a daemon crash.
    pub fn generate() -> Self {
        Self(SessionId::generate().0)
    }

    /// Wrap an opaque identity received from the wire.
    pub fn from_string(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

impl fmt::Display for PaneId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Daemon-assigned terminal (PTY) id. Not stable across daemon restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TerminalId(pub u64);

impl fmt::Display for TerminalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// 256-bit secret, hex encoded. Used for the daemon's user token and for
/// per-session tokens.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Token(String);

impl Token {
    pub fn generate() -> Self {
        let bytes = random_bytes::<32>();
        Self(bytes.iter().map(|b| format!("{b:02x}")).collect())
    }

    pub fn from_string(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Constant-time comparison.
    pub fn matches(&self, other: &str) -> bool {
        let a = self.0.as_bytes();
        let b = other.as_bytes();
        if a.len() != b.len() {
            return false;
        }
        a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(…)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_are_unique_sortable_and_26_chars() {
        let a = SessionId::generate();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = SessionId::generate();
        assert_eq!(a.as_str().len(), 26);
        assert_ne!(a, b);
        assert!(a < b);
    }

    #[test]
    fn tokens_compare_exactly() {
        let t = Token::generate();
        assert_eq!(t.as_str().len(), 64);
        assert!(t.matches(t.as_str()));
        assert!(!t.matches("nope"));
        assert_eq!(format!("{t:?}"), "Token(…)");
    }
}
