//! Remote access (`plans/mobile-client.md`): the listener that serves the
//! user's phone over the network, normally a tailnet.
//!
//! When `remote.json` is enabled the API keeps its 127.0.0.1 listener and
//! adds a remote one on every local Tailscale address (IPv4 in
//! `100.64.0.0/10`, IPv6 in `fd7a:115c:a1e0::/48`, and on Tailscale's own
//! interface: `session_core::tailscale::addresses`) at `remote.json`'s port.
//! `--remote-listen ADDR:PORT` (repeatable), or `remote.json` `listen`,
//! binds other addresses instead. Requests on the remote listener need the
//! pairing token and always act as the user (see `guard`).

use crate::log;
use session_core::ids::Token;
use session_core::paths::Layout;
use session_core::remote::RemoteConfig;
use session_core::tailscale;
use std::net::SocketAddr;

/// What the remote listener binds, and the token it admits.
pub struct RemotePlan {
    /// The pairing token from `remote.json`.
    pub token: Token,
    /// Addresses to bind; empty when nothing was found (loopback only).
    pub addrs: Vec<SocketAddr>,
    /// The addresses came from `--remote-listen`: a failure to bind them is
    /// an error, not a warning.
    pub explicit: bool,
}

impl std::fmt::Debug for RemotePlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemotePlan")
            .field("addrs", &self.addrs)
            .field("explicit", &self.explicit)
            .finish()
    }
}

/// The remote listener to serve, from `--remote-listen` (`flags`) and
/// `remote.json`. `Ok(None)`: remote access is off. Problems with
/// `remote.json` are warnings (the loopback API keeps working for local
/// clients) unless `flags` asked for a remote listener explicitly.
pub fn plan(layout: &Layout, flags: &[SocketAddr]) -> Result<Option<RemotePlan>, String> {
    let path = layout.remote_file();
    let explicit = !flags.is_empty();
    let refuse = |message: String| -> Result<Option<RemotePlan>, String> {
        if explicit {
            Err(message)
        } else {
            log::warn(format_args!("{message}; serving 127.0.0.1 only"));
            Ok(None)
        }
    };
    let config = match RemoteConfig::load(layout) {
        Ok(Some(config)) if config.enabled => config,
        Ok(_) if explicit => {
            return Err(
                "--remote-listen needs remote access to be on: run `orchestra remote on`".into(),
            )
        }
        Ok(_) => return Ok(None),
        Err(error) => return refuse(format!("cannot read {}: {error}", path.display())),
    };
    if !config.has_valid_token() {
        return refuse(format!(
            "{} has no valid token; run `orchestra remote token --rotate`",
            path.display()
        ));
    }
    let addrs = if explicit {
        flags.to_vec()
    } else if !config.listen.is_empty() {
        let mut addrs = Vec::new();
        for entry in &config.listen {
            match entry.parse::<SocketAddr>() {
                Ok(addr) if !addrs.contains(&addr) => addrs.push(addr),
                Ok(_) => {}
                Err(_) => log::warn(format_args!(
                    "{}: `listen` entry {entry:?} is not ADDR:PORT; skipped",
                    path.display()
                )),
            }
        }
        addrs
    } else {
        match tailscale::addresses() {
            Ok(ips) => ips
                .into_iter()
                .map(|ip| SocketAddr::new(ip, config.port))
                .collect(),
            Err(error) => {
                log::warn(format_args!(
                    "cannot list this machine's addresses: {error}"
                ));
                Vec::new()
            }
        }
    };
    Ok(Some(RemotePlan {
        token: Token::from_string(config.token),
        addrs,
        explicit,
    }))
}

/// `http://<addr>` for a bound address (`[..]` around IPv6).
pub fn url(addr: SocketAddr) -> String {
    format!("http://{addr}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_bracket_ipv6() {
        assert_eq!(
            url("100.64.0.1:7337".parse().unwrap()),
            "http://100.64.0.1:7337"
        );
        assert_eq!(
            url("[fd7a:115c:a1e0::1]:7337".parse().unwrap()),
            "http://[fd7a:115c:a1e0::1]:7337"
        );
    }

    fn home() -> (tempfile::TempDir, Layout) {
        let dir = tempfile::tempdir().unwrap();
        let layout = Layout::with_home(dir.path().to_path_buf());
        (dir, layout)
    }

    fn enabled(listen: &[&str]) -> RemoteConfig {
        RemoteConfig {
            enabled: true,
            listen: listen.iter().map(|s| s.to_string()).collect(),
            ..RemoteConfig::new().unwrap()
        }
    }

    #[test]
    fn remote_access_is_off_without_an_enabled_remote_json() {
        let (_dir, layout) = home();
        assert!(plan(&layout, &[]).unwrap().is_none());
        let flag: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let error = plan(&layout, &[flag]).unwrap_err();
        assert!(error.contains("orchestra remote on"), "{error}");
        let mut config = enabled(&[]);
        config.enabled = false;
        config.save(&layout).unwrap();
        assert!(plan(&layout, &[]).unwrap().is_none());
        assert!(plan(&layout, &[flag]).is_err());
    }

    #[test]
    fn flags_win_over_listen_and_listen_over_tailscale() {
        let (_dir, layout) = home();
        let config = enabled(&["127.0.0.1:7400", "nonsense", "127.0.0.1:7400", "[::1]:7401"]);
        config.save(&layout).unwrap();
        let from_file = plan(&layout, &[]).unwrap().unwrap();
        assert!(!from_file.explicit);
        assert!(from_file.token.matches(&config.token));
        assert_eq!(
            from_file.addrs,
            vec![
                "127.0.0.1:7400".parse::<SocketAddr>().unwrap(),
                "[::1]:7401".parse().unwrap()
            ]
        );
        let flag: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let from_flags = plan(&layout, &[flag]).unwrap().unwrap();
        assert!(from_flags.explicit);
        assert_eq!(from_flags.addrs, vec![flag]);
    }

    #[test]
    fn without_listen_the_tailscale_addresses_are_used() {
        let (_dir, layout) = home();
        let mut config = enabled(&[]);
        config.port = 7999;
        config.save(&layout).unwrap();
        let planned = plan(&layout, &[]).unwrap().unwrap();
        let expected: Vec<SocketAddr> = tailscale::addresses()
            .unwrap()
            .into_iter()
            .map(|ip| SocketAddr::new(ip, 7999))
            .collect();
        assert_eq!(planned.addrs, expected);
    }

    #[test]
    fn a_broken_remote_json_is_a_warning_unless_flags_ask() {
        let (_dir, layout) = home();
        std::fs::write(layout.remote_file(), "{").unwrap();
        assert!(plan(&layout, &[]).unwrap().is_none());
        let flag: SocketAddr = "127.0.0.1:0".parse().unwrap();
        assert!(plan(&layout, &[flag]).is_err());
        let mut config = enabled(&[]);
        config.token = "short".into();
        config.save(&layout).unwrap();
        assert!(plan(&layout, &[]).unwrap().is_none());
        let error = plan(&layout, &[flag]).unwrap_err();
        assert!(error.contains("no valid token"), "{error}");
    }
}
