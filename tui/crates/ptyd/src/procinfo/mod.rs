//! Process inspection for terminals: the PTY's foreground process and the
//! working directory of a process; and for `resource_usage`, the process
//! table, the machine's counters and TCP traffic. Linux uses /proc (and
//! socket diagnostics, `tcp`); macOS uses libproc and Mach.
//!
//! `ProcessInfo::name` is the executable name as the user would recognise it
//! (`claude`, `node`, `bash`): `comm` on Linux, `argv[0]`'s basename on
//! macOS (where the kernel's name is the resolved file, e.g. a version
//! number for Claude Code's native installs). Seeing through interpreters
//! (`node …/codex`) is left to `detect`, which gets the full `cmdline`, and
//! the file the process runs (`exe`), for programs that renamed themselves.

use crate::host::ProcessInfo;
use session_core::api::MachineUsage;
use std::collections::HashMap;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "linux")]
mod tcp;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;

/// Name and command line of `pid`, or None if it's gone.
pub fn process_info(pid: u32) -> Option<ProcessInfo> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        platform::process_info(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Current working directory of `pid`.
pub fn cwd_of(pid: u32) -> Option<PathBuf> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        platform::cwd_of(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Child processes of `pid`, their children and so on (a few levels).
/// Empty where the platform doesn't say.
pub fn descendants(pid: u32) -> Vec<u32> {
    #[cfg(target_os = "linux")]
    {
        linux::descendants(pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Vec::new()
    }
}

/// Files `pid` has open. Empty where the platform doesn't say.
pub fn open_files(pid: u32) -> Vec<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        linux::open_files(pid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Vec::new()
    }
}

/// Resident set size of the current process in bytes (for DaemonStatus).
pub fn self_rss_bytes() -> Option<u64> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        platform::self_rss_bytes()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// A process's line in the process table: its parent, and the CPU time and
/// resident memory it uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcStat {
    pub pid: u32,
    pub ppid: u32,
    /// User and system CPU time of all its threads so far, in ms.
    pub cpu_ms: u64,
    pub rss_bytes: u64,
}

/// Every process the platform lets this user inspect (on Linux every
/// process; on macOS this user's own). Empty where the platform doesn't say.
pub fn process_table() -> Vec<ProcStat> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        platform::process_table()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Vec::new()
    }
}

/// The machine's CPU, memory and network counters.
pub fn machine_usage() -> MachineUsage {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        platform::machine_usage()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        MachineUsage {
            cpus: logical_cpus(),
            ..MachineUsage::default()
        }
    }
}

/// Bytes received and sent over the TCP connections each of `pids` holds
/// open, to anywhere but loopback; a connection two of them hold counts for
/// the first. Every pid has an entry, `(0, 0)` without connections. None
/// where the platform doesn't say (only Linux does).
pub fn tcp_traffic(pids: &[u32]) -> Option<HashMap<u32, (u64, u64)>> {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: geteuid has no preconditions.
        let sockets = tcp::sockets(unsafe { libc::geteuid() })?;
        let mut counted = std::collections::HashSet::new();
        let mut traffic = HashMap::with_capacity(pids.len());
        for pid in pids {
            let entry: &mut (u64, u64) = traffic.entry(*pid).or_default();
            for inode in linux::socket_inodes(*pid) {
                let Some(socket) = sockets.get(&inode) else {
                    continue;
                };
                if counted.insert(inode) {
                    entry.0 = entry.0.saturating_add(socket.received);
                    entry.1 = entry.1.saturating_add(socket.sent);
                }
            }
        }
        Some(traffic)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pids;
        None
    }
}

/// Logical CPUs, at least one.
fn logical_cpus() -> u32 {
    std::thread::available_parallelism()
        .ok()
        .and_then(|n| u32::try_from(n.get()).ok())
        .unwrap_or(1)
}

// Parsing helpers shared by the platform modules. They are pure so both
// platforms' formats are unit-tested on any host.

/// Parse `/proc/<pid>/stat`: the parent is the 4th field, CPU time the
/// 14th and 15th (in clock ticks), resident pages the 24th. The name (2nd)
/// is in parentheses and may hold spaces and parentheses itself, so the
/// fields after it are counted from its last `)`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_proc_stat(stat: &str, ticks_per_sec: u64, page_size: u64) -> Option<ProcStat> {
    let (head, rest) = stat.rsplit_once(')')?;
    let pid: u32 = head.split_once('(')?.0.trim().parse().ok()?;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // `fields[0]` is the 3rd field (state).
    let field = |n: usize| -> Option<u64> { fields.get(n - 3)?.parse().ok() };
    let ppid = u32::try_from(field(4)?).ok()?;
    let ticks = field(14)?.saturating_add(field(15)?);
    Some(ProcStat {
        pid,
        ppid,
        cpu_ms: ticks.saturating_mul(1000) / ticks_per_sec.max(1),
        rss_bytes: field(24)?.saturating_mul(page_size),
    })
}

/// Busy and total CPU time in ms from `/proc/stat`'s first (`cpu`) line:
/// user, nice, system, idle, iowait, irq, softirq and steal ticks (guest
/// time is already in user and nice). Idle and iowait are not busy.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_cpu_times(stat: &str, ticks_per_sec: u64) -> Option<(u64, u64)> {
    let line = stat.lines().next()?;
    let mut words = line.split_whitespace();
    if words.next()? != "cpu" {
        return None;
    }
    let ticks: Vec<u64> = words
        .take(8)
        .map(|w| w.parse().ok())
        .collect::<Option<_>>()?;
    if ticks.len() < 4 {
        return None;
    }
    let total: u64 = ticks.iter().sum();
    let idle = ticks[3] + ticks.get(4).copied().unwrap_or(0);
    let ms = |t: u64| t.saturating_mul(1000) / ticks_per_sec.max(1);
    Some((ms(total.saturating_sub(idle)), ms(total)))
}

/// `MemTotal` and `MemAvailable` from `/proc/meminfo`, in bytes.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_meminfo(meminfo: &str) -> (Option<u64>, Option<u64>) {
    let mut total = None;
    let mut available = None;
    for line in meminfo.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let slot = match key {
            "MemTotal" => &mut total,
            "MemAvailable" => &mut available,
            _ => continue,
        };
        let mut words = value.split_whitespace();
        let amount: Option<u64> = words.next().and_then(|n| n.parse().ok());
        let unit = match words.next() {
            Some("kB") => 1024,
            _ => 1,
        };
        *slot = amount.map(|n| n.saturating_mul(unit));
    }
    (total, available)
}

/// Bytes received and sent on every interface but loopback, from
/// `/proc/net/dev` (`name: rx_bytes … (8 receive fields) tx_bytes …`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_net_dev(dev: &str) -> Option<(u64, u64)> {
    let mut found = None;
    for line in dev.lines() {
        let Some((name, counters)) = line.split_once(':') else {
            continue;
        };
        if name.trim() == "lo" {
            continue;
        }
        let fields: Vec<u64> = counters
            .split_whitespace()
            .map(|w| w.parse().ok())
            .collect::<Option<_>>()
            .unwrap_or_default();
        let (Some(rx), Some(tx)) = (fields.first(), fields.get(8)) else {
            continue;
        };
        let (all_rx, all_tx) = found.get_or_insert((0u64, 0u64));
        *all_rx = all_rx.saturating_add(*rx);
        *all_tx = all_tx.saturating_add(*tx);
    }
    found
}

/// Split a NUL-separated argument block (`/proc/<pid>/cmdline`, the argv
/// area of `KERN_PROCARGS2`). Trailing empty strings are padding left when
/// a program shortens its argv (e.g. Node's `process.title`), so they are
/// dropped; empty arguments in the middle are kept.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn split_nul_args(bytes: &[u8]) -> Vec<String> {
    let mut args: Vec<String> = bytes
        .split(|b| *b == 0)
        .map(|arg| String::from_utf8_lossy(arg).into_owned())
        .collect();
    while args.last().is_some_and(|arg| arg.is_empty()) {
        args.pop();
    }
    args
}

/// Last path component of `arg`, without the leading `-` login shells put
/// in `argv[0]`.
fn arg_basename(arg: &str) -> &str {
    let base = arg.rsplit('/').next().unwrap_or(arg);
    base.strip_prefix('-').unwrap_or(base)
}

/// Linux truncates `comm` to 15 bytes (`cursor-agent-wr…`). When it looks
/// truncated and `argv[0]`'s basename extends it, use the full basename.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn untruncated_name(comm: &str, cmdline: &[String]) -> String {
    const TASK_COMM_LEN: usize = 15;
    if comm.len() == TASK_COMM_LEN {
        if let Some(base) = cmdline.first().map(|arg0| arg_basename(arg0)) {
            if base.len() > comm.len() && base.starts_with(comm) {
                return base.to_string();
            }
        }
    }
    comm.to_string()
}

/// Executable name from `argv[0]`: basename without a login-shell `-`.
/// None when `argv[0]` is missing or empty.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn name_from_argv0(cmdline: &[String]) -> Option<String> {
    let first = cmdline.first()?;
    // Paths may contain spaces ("/Applications/My Tools/x"); a bare word
    // with spaces is a program that rewrote its argv as one string
    // ("claude --resume"), which still names itself first.
    let token = if first.contains('/') {
        first.as_str()
    } else {
        first.split_whitespace().next().unwrap_or("")
    };
    let base = arg_basename(token);
    (!base.is_empty()).then(|| base.to_string())
}

/// Parse a `KERN_PROCARGS2` buffer: native-endian `int argc`, the exec path
/// (NUL-terminated, then NUL padding), then `argc` NUL-terminated arguments
/// followed by the environment. Returns the arguments, or None if the
/// buffer is malformed.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn parse_procargs2(buf: &[u8]) -> Option<Vec<String>> {
    let argc_bytes: [u8; 4] = buf.get(..4)?.try_into().ok()?;
    let argc = usize::try_from(i32::from_ne_bytes(argc_bytes)).ok()?;
    let rest = &buf[4..];
    let exec_path_end = rest.iter().position(|b| *b == 0)?;
    let mut pos = exec_path_end;
    while rest.get(pos) == Some(&0) {
        pos += 1;
    }
    let mut args = Vec::with_capacity(argc.min(256));
    let mut remaining = &rest[pos..];
    while args.len() < argc && !remaining.is_empty() {
        let end = remaining
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(remaining.len());
        args.push(String::from_utf8_lossy(&remaining[..end]).into_owned());
        remaining = remaining.get(end + 1..).unwrap_or(&[]);
    }
    while args.last().is_some_and(|arg| arg.is_empty()) {
        args.pop();
    }
    Some(args)
}

/// Resident pages (second field of `/proc/<pid>/statm`) times the page size.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn parse_statm_rss(statm: &str, page_size: u64) -> Option<u64> {
    let resident: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    resident.checked_mul(page_size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cmdline_splits_on_nul_and_drops_padding() {
        assert_eq!(
            split_nul_args(b"node\0/usr/lib/node_modules/@openai/codex/bin/codex.js\0"),
            strings(&["node", "/usr/lib/node_modules/@openai/codex/bin/codex.js"])
        );
        // Node's process.title zero-fills the rest of the argv area.
        assert_eq!(split_nul_args(b"claude\0\0\0\0\0"), strings(&["claude"]));
        assert_eq!(
            split_nul_args(b"printf\0\0x\0"),
            strings(&["printf", "", "x"])
        );
        assert!(split_nul_args(b"").is_empty());
        assert_eq!(
            split_nul_args(b"no-terminator"),
            strings(&["no-terminator"])
        );
    }

    #[test]
    fn truncated_comm_is_completed_from_argv0() {
        let argv = strings(&["/opt/bin/cursor-agent-wrapper", "--flag"]);
        assert_eq!(
            untruncated_name("cursor-agent-wr", &argv),
            "cursor-agent-wrapper"
        );
        // Not truncated: keep comm even if argv[0] differs (node sets it).
        assert_eq!(untruncated_name("node", &strings(&["claude"])), "node");
        // 15 bytes but argv[0] doesn't extend it.
        assert_eq!(
            untruncated_name("abcdefghijklmno", &strings(&["other"])),
            "abcdefghijklmno"
        );
        assert_eq!(untruncated_name("bash", &[]), "bash");
    }

    #[test]
    fn argv0_names_strip_paths_and_login_dashes() {
        assert_eq!(name_from_argv0(&strings(&["-zsh"])).as_deref(), Some("zsh"));
        assert_eq!(
            name_from_argv0(&strings(&["/Users/me/.local/bin/claude", "--resume"])).as_deref(),
            Some("claude")
        );
        assert_eq!(
            name_from_argv0(&strings(&["claude --resume abc"])).as_deref(),
            Some("claude")
        );
        assert_eq!(
            name_from_argv0(&strings(&["/Applications/My Tools/claude"])).as_deref(),
            Some("claude")
        );
        assert_eq!(name_from_argv0(&strings(&[""])), None);
        assert_eq!(name_from_argv0(&[]), None);
    }

    fn procargs2(argc: i32, exec_path: &str, padding: usize, rest: &[&str]) -> Vec<u8> {
        let mut buf = argc.to_ne_bytes().to_vec();
        buf.extend_from_slice(exec_path.as_bytes());
        buf.push(0);
        buf.resize(buf.len() + padding, 0);
        for item in rest {
            buf.extend_from_slice(item.as_bytes());
            buf.push(0);
        }
        buf
    }

    #[test]
    fn procargs2_yields_argv_without_exec_path_or_env() {
        let buf = procargs2(
            2,
            "/usr/local/bin/node",
            5,
            &[
                "node",
                "/usr/local/lib/node_modules/@openai/codex/bin/codex.js",
                "HOME=/Users/me",
            ],
        );
        assert_eq!(
            parse_procargs2(&buf).unwrap(),
            strings(&[
                "node",
                "/usr/local/lib/node_modules/@openai/codex/bin/codex.js"
            ])
        );
        let buf = procargs2(1, "/bin/zsh", 0, &["-zsh", "PATH=/bin"]);
        assert_eq!(parse_procargs2(&buf).unwrap(), strings(&["-zsh"]));
    }

    #[test]
    fn procargs2_tolerates_short_and_rejects_malformed_buffers() {
        // Fewer strings than argc (buffer cut short).
        let buf = procargs2(3, "/bin/sleep", 2, &["sleep"]);
        assert_eq!(parse_procargs2(&buf).unwrap(), strings(&["sleep"]));
        assert_eq!(parse_procargs2(&[1, 0]), None);
        assert_eq!(parse_procargs2(&procargs2(-1, "/x", 0, &["x"])), None);
        // Exec path without a terminator.
        let mut buf = 1i32.to_ne_bytes().to_vec();
        buf.extend_from_slice(b"/bin/x");
        assert_eq!(parse_procargs2(&buf), None);
    }

    #[test]
    fn proc_stat_fields_are_counted_after_the_name() {
        // A name with spaces and a parenthesis, as `comm` allows.
        let stat = "4242 (node (x) y) S 4200 4242 4200 34816 4242 4194304 1000 0 0 0 \
                    250 150 0 0 20 0 11 0 12345 1000000000 3000 18446744073709551615";
        assert_eq!(
            parse_proc_stat(stat, 100, 4096),
            Some(ProcStat {
                pid: 4242,
                ppid: 4200,
                cpu_ms: 4000,
                rss_bytes: 3000 * 4096,
            })
        );
        assert_eq!(parse_proc_stat("4242 (sh) S 1", 100, 4096), None);
        assert_eq!(parse_proc_stat("garbage", 100, 4096), None);
    }

    #[test]
    fn cpu_times_leave_idle_and_iowait_out_of_busy() {
        let stat = "cpu  100 20 30 800 50 0 0 0 7 0\ncpu0 1 2 3 4 5 6 7 8 9 10\n";
        // Total 1000 ticks, idle 850: busy 150 ticks.
        assert_eq!(parse_cpu_times(stat, 100), Some((1500, 10000)));
        assert_eq!(parse_cpu_times("cpu0 1 2 3 4\n", 100), None);
        assert_eq!(parse_cpu_times("", 100), None);
    }

    #[test]
    fn meminfo_reads_total_and_available() {
        let meminfo =
            "MemTotal:       32000000 kB\nMemFree:  100 kB\nMemAvailable:   16000000 kB\n";
        assert_eq!(
            parse_meminfo(meminfo),
            (Some(32_000_000 * 1024), Some(16_000_000 * 1024))
        );
        assert_eq!(parse_meminfo("MemTotal: 5 kB\n"), (Some(5 * 1024), None));
    }

    #[test]
    fn net_dev_sums_every_interface_but_loopback() {
        let dev = "Inter-|   Receive                                                |  Transmit\n \
                   face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets\n    \
                   lo: 9999 10 0 0 0 0 0 0 9999 10 0 0 0 0 0 0\n  \
                   eth0: 1000 10 0 0 0 0 0 0 200 5 0 0 0 0 0 0\n\
                   wlan0: 30 1 0 0 0 0 0 0 4 1 0 0 0 0 0 0\n";
        assert_eq!(parse_net_dev(dev), Some((1030, 204)));
        assert_eq!(
            parse_net_dev("    lo: 1 0 0 0 0 0 0 0 1 0 0 0 0 0 0 0\n"),
            None
        );
    }

    #[test]
    fn statm_resident_pages_scale_by_page_size() {
        assert_eq!(
            parse_statm_rss("12345 678 90 1 0 300 0\n", 4096),
            Some(678 * 4096)
        );
        assert_eq!(parse_statm_rss("12345", 4096), None);
        assert_eq!(parse_statm_rss("a b c", 4096), None);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mod live {
        use super::super::*;
        use std::process::{Command, Stdio};

        struct Child(std::process::Child);

        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }

        /// `spawn` can return before the child's exec is visible in /proc:
        /// for a moment the command line reads empty, or still shows the
        /// forking test binary. Wait until the expected program shows up.
        fn settled_info(pid: u32, name: &str) -> ProcessInfo {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let info = process_info(pid).unwrap();
                let settled = info.name == name && !info.cmdline.is_empty();
                if settled || std::time::Instant::now() > deadline {
                    return info;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }

        fn spawn_sleep(dir: &std::path::Path) -> Child {
            Child(
                Command::new("sleep")
                    .arg("30")
                    .current_dir(dir)
                    .stdin(Stdio::null())
                    .spawn()
                    .unwrap(),
            )
        }

        #[test]
        fn inspects_a_running_child() {
            let dir = tempfile::tempdir().unwrap();
            let child = spawn_sleep(dir.path());
            let pid = child.0.id();
            let info = settled_info(pid, "sleep");
            assert_eq!(info.pid, pid);
            assert_eq!(info.name, "sleep");
            assert_eq!(info.cmdline.last().map(String::as_str), Some("30"));
            assert_eq!(
                cwd_of(pid).unwrap(),
                std::fs::canonicalize(dir.path()).unwrap()
            );
        }

        #[test]
        fn interpreter_children_report_the_runtime_and_script() {
            let dir = tempfile::tempdir().unwrap();
            let script = dir.path().join("fake-agent.sh");
            // `exit` after the command so no shell execs `sleep` in place.
            std::fs::write(&script, "sleep 30\nexit 0\n").unwrap();
            let child = Child(
                Command::new("sh")
                    .arg(&script)
                    .stdin(Stdio::null())
                    .spawn()
                    .unwrap(),
            );
            let info = settled_info(child.0.id(), "sh");
            assert_eq!(info.name, "sh");
            assert_eq!(
                info.cmdline,
                vec!["sh".to_string(), script.display().to_string()]
            );
        }

        #[test]
        fn gone_processes_yield_none() {
            let mut child = Command::new("true").spawn().unwrap();
            let pid = child.id();
            child.wait().unwrap();
            assert_eq!(process_info(pid), None);
            assert_eq!(cwd_of(pid), None);
            assert_eq!(process_info(0), None);
            assert_eq!(process_info(u32::MAX), None);
        }

        #[test]
        fn the_file_a_process_runs_is_read() {
            let own = process_info(std::process::id()).unwrap();
            let exe = std::env::current_exe().unwrap().canonicalize().unwrap();
            assert_eq!(own.exe.map(|path| path.canonicalize().unwrap()), Some(exe));
        }

        #[test]
        fn own_process_has_a_plausible_rss() {
            let rss = self_rss_bytes().unwrap();
            assert!(rss > 64 * 1024, "rss {rss}");
            assert!(rss < 64 * 1024 * 1024 * 1024, "rss {rss}");
            assert!(cwd_of(std::process::id()).is_some());
        }
    }
}
