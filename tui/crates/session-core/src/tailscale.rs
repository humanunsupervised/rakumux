//! This machine on Tailscale: the addresses Tailscale gave it, and its
//! MagicDNS name. Used by `rakumux-api` (where the remote listener binds)
//! and by the TUI and CLI (whether to offer phone access, the pairing URL).
//!
//! An address counts only when it is in Tailscale's ranges
//! ([`is_tailscale`]) **and** belongs to Tailscale: it is on Tailscale's own
//! interface (`tailscale0` on Linux, or any `tailscale*` name), or the
//! `tailscale` CLI lists it as this machine's (macOS, where Tailscale's
//! interface is an anonymous `utun`, or a renamed Linux interface). The
//! ranges alone aren't enough: `100.64.0.0/10` is the carrier-grade NAT
//! range, which some ISPs and hotspots hand out directly, and other VPNs
//! (Cloudflare WARP) use it too.
//!
//! The `tailscale` CLI is optional: when it is missing or doesn't answer
//! within [`CLI_WAIT`], only addresses on a `tailscale*` interface count.

use crate::remote::is_tailscale;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long `tailscale status --json` may take.
pub const CLI_WAIT: Duration = Duration::from_secs(3);

/// An address of a local network interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterfaceAddress {
    /// The interface's name, e.g. `tailscale0`, `en0`, `utun4`.
    pub interface: String,
    pub ip: IpAddr,
}

/// What `tailscale status --json` says about this machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// `BackendState` is `Running` (logged in and connected).
    pub running: bool,
    /// `Self.TailscaleIPs`.
    pub ips: Vec<IpAddr>,
    /// `Self.DNSName` without its trailing dot, e.g.
    /// `box.tail1234.ts.net`.
    pub dns_name: Option<String>,
}

/// This machine's Tailscale addresses (see the module docs), IPv4 first,
/// without duplicates. Runs the `tailscale` CLI only when an address in
/// Tailscale's ranges isn't on a `tailscale*` interface.
pub fn addresses() -> io::Result<Vec<IpAddr>> {
    Ok(select(&interface_addresses()?, status))
}

/// The addresses of `local` that belong to Tailscale: in its ranges and on
/// a `tailscale*` interface, else listed by `status` (asked only when some
/// address in the ranges is on another interface).
pub fn select(local: &[InterfaceAddress], status: impl FnOnce() -> Option<Status>) -> Vec<IpAddr> {
    let in_range: Vec<&InterfaceAddress> =
        local.iter().filter(|addr| is_tailscale(addr.ip)).collect();
    let mut ips: Vec<IpAddr> = in_range
        .iter()
        .filter(|addr| is_tailscale_interface(&addr.interface))
        .map(|addr| addr.ip)
        .collect();
    let others: Vec<IpAddr> = in_range
        .iter()
        .filter(|addr| !is_tailscale_interface(&addr.interface))
        .map(|addr| addr.ip)
        .collect();
    if !others.is_empty() {
        if let Some(status) = status().filter(|status| status.running) {
            ips.extend(others.into_iter().filter(|ip| status.ips.contains(ip)));
        }
    }
    ips.sort_by_key(|ip| (ip.is_ipv6(), *ip));
    ips.dedup();
    ips
}

/// Whether `name` is Tailscale's interface on Linux (`tailscale0` by
/// default; `tailscaled --tun` can choose another `tailscale*` name).
pub fn is_tailscale_interface(name: &str) -> bool {
    name.starts_with("tailscale")
}

/// Every IPv4 and IPv6 address of the local interfaces (`getifaddrs`),
/// with the interface's name.
pub fn interface_addresses() -> io::Result<Vec<InterfaceAddress>> {
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs allocates a linked list into `list`; it is freed
    // with freeifaddrs below and not used afterwards.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut out = Vec::new();
    let mut cursor = list;
    while !cursor.is_null() {
        // SAFETY: `cursor` is a node of the list getifaddrs returned.
        let entry = unsafe { &*cursor };
        if !entry.ifa_addr.is_null() && !entry.ifa_name.is_null() {
            // SAFETY: `ifa_addr` points at a sockaddr whose family says
            // which concrete type it is; read unaligned to be safe.
            let family = i32::from(unsafe { (*entry.ifa_addr).sa_family });
            let ip = if family == libc::AF_INET {
                let addr =
                    unsafe { std::ptr::read_unaligned(entry.ifa_addr as *const libc::sockaddr_in) };
                Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(
                    addr.sin_addr.s_addr,
                ))))
            } else if family == libc::AF_INET6 {
                let addr = unsafe {
                    std::ptr::read_unaligned(entry.ifa_addr as *const libc::sockaddr_in6)
                };
                Some(IpAddr::V6(Ipv6Addr::from(addr.sin6_addr.s6_addr)))
            } else {
                None
            };
            if let Some(ip) = ip {
                // SAFETY: `ifa_name` is a NUL-terminated string owned by
                // the list, valid until freeifaddrs.
                let name = unsafe { std::ffi::CStr::from_ptr(entry.ifa_name) };
                out.push(InterfaceAddress {
                    interface: name.to_string_lossy().into_owned(),
                    ip,
                });
            }
        }
        cursor = entry.ifa_next;
    }
    // SAFETY: `list` came from getifaddrs and is freed exactly once.
    unsafe { libc::freeifaddrs(list) };
    Ok(out)
}

/// `tailscale status --json`, when the CLI is there and answers within
/// [`CLI_WAIT`]. Optional: callers work without it.
pub fn status() -> Option<Status> {
    cli_candidates()
        .into_iter()
        .find_map(|program| run_with_timeout(&program, &["status", "--json"], CLI_WAIT))
        .and_then(|output| parse_status(&output))
}

/// The `tailscale` CLI: on `PATH`, and on macOS inside the app as well.
fn cli_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("tailscale")];
    if cfg!(target_os = "macos") {
        candidates.push("/Applications/Tailscale.app/Contents/MacOS/Tailscale".into());
    }
    candidates
}

/// Read `tailscale status --json`: `BackendState`, `Self.TailscaleIPs` and
/// `Self.DNSName` (kept only when it is a plain host name).
pub fn parse_status(json: &[u8]) -> Option<Status> {
    let value: serde_json::Value = serde_json::from_slice(json).ok()?;
    let me = value.get("Self");
    let ips = me
        .and_then(|me| me.get("TailscaleIPs"))
        .and_then(|ips| ips.as_array())
        .map(|ips| {
            ips.iter()
                .filter_map(|ip| ip.as_str()?.parse::<IpAddr>().ok())
                .collect()
        })
        .unwrap_or_default();
    let dns_name = me
        .and_then(|me| me.get("DNSName"))
        .and_then(|name| name.as_str())
        .map(|name| name.trim_end_matches('.'))
        .filter(|name| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        })
        .map(str::to_owned);
    Some(Status {
        running: value.get("BackendState").and_then(|s| s.as_str()) == Some("Running"),
        ips,
        dns_name,
    })
}

/// Run `program args` and return its stdout when it succeeds within
/// `wait`; the process is killed after that.
fn run_with_timeout(program: &Path, args: &[&str], wait: Duration) -> Option<Vec<u8>> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        io::Read::read_to_end(&mut stdout, &mut bytes)
            .ok()
            .map(|_| bytes)
    });
    let deadline = Instant::now() + wait;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let bytes = reader.join().ok().flatten()?;
    status.filter(|status| status.success()).map(|_| bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(interface: &str, ip: &str) -> InterfaceAddress {
        InterfaceAddress {
            interface: interface.into(),
            ip: ip.parse().unwrap(),
        }
    }

    fn ips(list: &[&str]) -> Vec<IpAddr> {
        list.iter().map(|ip| ip.parse().unwrap()).collect()
    }

    #[test]
    fn local_addresses_are_listed_with_their_interface() {
        let local = interface_addresses().unwrap();
        assert!(!local.is_empty());
        assert!(local.iter().all(|a| !a.interface.is_empty()), "{local:?}");
        assert!(local.iter().all(|a| !a.ip.is_unspecified()), "{local:?}");
    }

    #[test]
    fn the_tailscale_interface_counts_without_asking_the_cli() {
        let local = [
            addr("lo", "127.0.0.1"),
            addr("wlp7s0", "192.168.68.58"),
            addr("tailscale0", "fd7a:115c:a1e0::9633:815b"),
            addr("tailscale0", "100.88.129.91"),
            addr("tailscale0", "fe80::1"),
        ];
        assert_eq!(
            select(&local, || panic!("the CLI was asked")),
            ips(&["100.88.129.91", "fd7a:115c:a1e0::9633:815b"])
        );
    }

    #[test]
    fn a_carrier_grade_nat_address_elsewhere_does_not_count() {
        // An ISP or hotspot handing out 100.64.0.0/10 on Wi-Fi, WARP on its
        // own interface: not Tailscale's, whatever the range says.
        let local = [
            addr("eth0", "100.72.10.20"),
            addr("CloudflareWARP", "100.96.0.5"),
            addr("tailscale0", "100.88.129.91"),
        ];
        assert_eq!(select(&local, || None), ips(&["100.88.129.91"]));
        let not_running = || {
            Some(Status {
                running: false,
                ips: ips(&["100.72.10.20"]),
                dns_name: None,
            })
        };
        assert_eq!(select(&local, not_running), ips(&["100.88.129.91"]));
        assert!(select(&[addr("eth0", "100.72.10.20")], || None).is_empty());
    }

    #[test]
    fn on_macos_the_cli_says_which_utun_is_tailscale() {
        let local = [
            addr("en0", "100.72.10.20"),
            addr("utun3", "100.96.0.5"),
            addr("utun4", "100.101.102.103"),
            addr("utun4", "fd7a:115c:a1e0::1"),
        ];
        let status = || {
            Some(Status {
                running: true,
                ips: ips(&["100.101.102.103", "fd7a:115c:a1e0::1"]),
                dns_name: Some("mac.tail1234.ts.net".into()),
            })
        };
        assert_eq!(
            select(&local, status),
            ips(&["100.101.102.103", "fd7a:115c:a1e0::1"])
        );
        // Without the CLI nothing on a utun counts.
        assert!(select(&local, || None).is_empty());
    }

    #[test]
    fn without_addresses_in_range_the_cli_is_not_asked() {
        let local = [addr("en0", "192.168.1.10"), addr("lo0", "::1")];
        assert!(select(&local, || panic!("the CLI was asked")).is_empty());
    }

    #[test]
    fn status_json_is_read() {
        let json = br#"{"BackendState": "Running",
            "Self": {"DNSName": "box.tail1234.ts.net.", "HostName": "box",
                     "TailscaleIPs": ["100.88.129.91", "fd7a:115c:a1e0::9633:815b", "nonsense"]}}"#;
        assert_eq!(
            parse_status(json),
            Some(Status {
                running: true,
                ips: ips(&["100.88.129.91", "fd7a:115c:a1e0::9633:815b"]),
                dns_name: Some("box.tail1234.ts.net".into()),
            })
        );
        let stopped = br#"{"BackendState": "Stopped", "Self": {"DNSName": "a b."}}"#;
        assert_eq!(
            parse_status(stopped),
            Some(Status {
                running: false,
                ips: vec![],
                dns_name: None,
            })
        );
        for bad in [&br#"{"Self": {"DNSName": ""}}"#[..], br#"{}"#] {
            assert_eq!(parse_status(bad).and_then(|s| s.dns_name), None);
        }
        assert_eq!(parse_status(b"not json"), None);
    }

    #[test]
    fn a_missing_cli_is_no_answer() {
        assert_eq!(
            run_with_timeout(
                Path::new("/nonexistent/tailscale"),
                &["status"],
                Duration::from_millis(100)
            ),
            None
        );
        // A program that outlives the wait is killed: no answer.
        assert_eq!(
            run_with_timeout(Path::new("sleep"), &["5"], Duration::from_millis(100)),
            None
        );
        assert_eq!(
            run_with_timeout(Path::new("echo"), &["hi"], Duration::from_secs(2)),
            Some(b"hi\n".to_vec())
        );
    }
}
