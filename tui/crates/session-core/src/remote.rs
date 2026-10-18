//! Remote access settings: `<home>/remote.json`, shared by `rakumux-api`
//! (which serves the remote listener) and the `rakumux remote` CLI (which
//! turns it on and off). See `api/README.md`.
//!
//! ```json
//! { "enabled": true, "port": 7337, "token": "<26 chars a-z2-7>", "listen": [] }
//! ```
//!
//! - `enabled`: the API adds a remote listener (it stays on until turned off).
//! - `port`: the port of the remote listener on each Tailscale address
//!   (`crate::tailscale::addresses`: only those on Tailscale's interface).
//! - `token`: the pairing token remote requests carry as `Authorization:
//!   Bearer <token>`: 128 random bits as 26 characters of lowercase base32.
//! - `listen`: `ADDR:PORT` entries bound instead of the Tailscale addresses
//!   (other networks, or `127.0.0.1:<port>` behind `tailscale serve`).
//!
//! The file is mode 0600 and written atomically (temp file + rename).

use crate::paths::Layout;
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::net::IpAddr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Default port of the remote listener.
pub const DEFAULT_PORT: u16 = 7337;
/// Length of a pairing token.
pub const TOKEN_LEN: usize = 26;
/// RFC 4648 base32, lowercase.
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// Contents of `remote.json`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteConfig {
    /// The API serves the remote listener.
    #[serde(default)]
    pub enabled: bool,
    /// Port of the remote listener on the Tailscale addresses.
    #[serde(default = "default_port")]
    pub port: u16,
    /// The pairing token (see [`generate_token`]).
    pub token: String,
    /// `ADDR:PORT` to bind instead of the Tailscale addresses.
    #[serde(default)]
    pub listen: Vec<String>,
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

impl std::fmt::Debug for RemoteConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemoteConfig")
            .field("enabled", &self.enabled)
            .field("port", &self.port)
            .field("token", &"…")
            .field("listen", &self.listen)
            .finish()
    }
}

impl RemoteConfig {
    /// Remote access off, the default port, a fresh token, no `listen`.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            enabled: false,
            port: DEFAULT_PORT,
            token: generate_token()?,
            listen: Vec::new(),
        })
    }

    /// Read `remote.json`; `Ok(None)` when there is none.
    pub fn load(layout: &Layout) -> io::Result<Option<Self>> {
        let raw = match std::fs::read(layout.remote_file()) {
            Ok(raw) => raw,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        serde_json::from_slice(&raw)
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Write `remote.json` atomically with mode 0600 (the home 0700 when it
    /// has to be created).
    pub fn save(&self, layout: &Layout) -> io::Result<()> {
        if !layout.home.is_dir() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&layout.home)?;
        }
        let mut bytes = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        bytes.push(b'\n');
        write_atomic(&layout.remote_file(), &bytes)
    }

    /// Replace the token with a fresh one.
    pub fn rotate_token(&mut self) -> io::Result<()> {
        self.token = generate_token()?;
        Ok(())
    }

    /// The token's first 4 characters, for status output.
    pub fn token_prefix(&self) -> &str {
        let end = self
            .token
            .char_indices()
            .nth(4)
            .map_or(self.token.len(), |(i, _)| i);
        &self.token[..end]
    }

    /// Whether the token has the expected form (see [`is_valid_token`]).
    pub fn has_valid_token(&self) -> bool {
        is_valid_token(&self.token)
    }
}

/// Whether `ip` is in Tailscale's ranges: IPv4 in `100.64.0.0/10` (the CGNAT
/// range Tailscale assigns from), IPv6 in `fd7a:115c:a1e0::/48`. The range
/// alone doesn't make an address Tailscale's (others use CGNAT addresses
/// too): [`crate::tailscale::addresses`] also checks the interface.
pub fn is_tailscale(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, ..] = ip.octets();
            a == 100 && (b & 0xc0) == 64
        }
        IpAddr::V6(ip) => {
            let [a, b, c, ..] = ip.segments();
            (a, b, c) == (0xfd7a, 0x115c, 0xa1e0)
        }
    }
}

/// A new pairing token: 16 random bytes (128 bits) as 26 characters of
/// lowercase base32 (`a-z2-7`), easy to read and paste.
pub fn generate_token() -> io::Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|e| io::Error::other(format!("no random source: {e}")))?;
    Ok(base32(&bytes))
}

/// [`TOKEN_LEN`] characters of lowercase base32.
pub fn is_valid_token(token: &str) -> bool {
    token.len() == TOKEN_LEN && token.bytes().all(|b| ALPHABET.contains(&b))
}

/// Unpadded lowercase base32 (RFC 4648 alphabet), most significant bits
/// first; the last character carries the remaining bits padded with zeros.
fn base32(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer: u32 = 0;
    let mut bits = 0u32;
    for &byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(format!(".{}.tmp", std::process::id()));
    let tmp = PathBuf::from(tmp_name);
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn tokens_are_26_lowercase_base32_characters() {
        let a = generate_token().unwrap();
        let b = generate_token().unwrap();
        assert_eq!(a.len(), TOKEN_LEN);
        assert!(is_valid_token(&a), "{a}");
        assert_ne!(a, b);
        assert!(!is_valid_token("ABCDEFGHIJKLMNOPQRSTUVWXYZ"));
        assert!(!is_valid_token("abcdefghijklmnopqrstuvwxy1"));
        assert!(!is_valid_token("abcdefghijklmnopqrstuvwxy"));
        assert!(!is_valid_token(""));
    }

    #[test]
    fn only_tailscale_ranges_are_tailscale() {
        let ip = |text: &str| text.parse::<IpAddr>().unwrap();
        for yes in [
            "100.64.0.0",
            "100.88.129.91",
            "100.127.255.255",
            "fd7a:115c:a1e0::1",
            "fd7a:115c:a1e0:ab12:4843:cd96:6258:b240",
        ] {
            assert!(is_tailscale(ip(yes)), "{yes}");
        }
        for no in [
            "100.63.255.255",
            "100.128.0.0",
            "10.0.0.1",
            "192.168.1.10",
            "127.0.0.1",
            "0.0.0.0",
            "::1",
            "fd7a:115c:a1e1::1",
            "fe80::1",
            "2001:db8::1",
        ] {
            assert!(!is_tailscale(ip(no)), "{no}");
        }
    }

    #[test]
    fn base32_matches_rfc_4648() {
        // RFC 4648 §10 test vectors, lowercase and unpadded.
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "my");
        assert_eq!(base32(b"fo"), "mzxq");
        assert_eq!(base32(b"foo"), "mzxw6");
        assert_eq!(base32(b"foob"), "mzxw6yq");
        assert_eq!(base32(b"fooba"), "mzxw6ytb");
        assert_eq!(base32(b"foobar"), "mzxw6ytboi");
        assert_eq!(base32(&[0xff; 16]), "77777777777777777777777774");
        assert_eq!(base32(&[0; 16]).len(), TOKEN_LEN);
    }

    #[test]
    fn remote_json_round_trips_privately() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().join("home"));
        assert_eq!(RemoteConfig::load(&layout).unwrap(), None);
        let mut config = RemoteConfig::new().unwrap();
        assert!(!config.enabled);
        assert_eq!(config.port, DEFAULT_PORT);
        assert!(config.has_valid_token());
        config.enabled = true;
        config.listen = vec!["127.0.0.1:7400".into()];
        config.save(&layout).unwrap();
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&layout.remote_file()), 0o600);
        assert_eq!(mode(&layout.home), 0o700);
        assert_eq!(RemoteConfig::load(&layout).unwrap(), Some(config.clone()));
        // Only the file itself is left (no temp files).
        assert_eq!(std::fs::read_dir(&layout.home).unwrap().count(), 1);

        let old = config.token.clone();
        config.rotate_token().unwrap();
        assert_ne!(config.token, old);
        assert_eq!(config.token_prefix(), &config.token[..4]);
    }

    #[test]
    fn missing_fields_take_defaults_and_bad_files_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().to_path_buf());
        std::fs::write(layout.remote_file(), r#"{"token": "x"}"#).unwrap();
        let config = RemoteConfig::load(&layout).unwrap().unwrap();
        assert!(!config.enabled);
        assert_eq!(config.port, DEFAULT_PORT);
        assert!(config.listen.is_empty());
        assert!(!config.has_valid_token());
        std::fs::write(layout.remote_file(), "{").unwrap();
        let error = RemoteConfig::load(&layout).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn debug_hides_the_token() {
        let config = RemoteConfig::new().unwrap();
        assert!(!format!("{config:?}").contains(&config.token));
    }
}
