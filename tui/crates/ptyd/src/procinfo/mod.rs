//! Process inspection for terminals: the PTY's foreground process and the
//! working directory of a process. Linux uses /proc; macOS uses libproc.
//!
//! `ProcessInfo::name` is the executable name as the user would recognise it
//! (`claude`, `node`, `bash`): `comm` on Linux, `argv[0]`'s basename on
//! macOS (where the kernel's name is the resolved file, e.g. a version
//! number for Claude Code's native installs). Seeing through interpreters
//! (`node …/codex`) is left to `detect`, which gets the full `cmdline`.

use crate::host::ProcessInfo;
use std::path::PathBuf;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

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

// Parsing helpers shared by the platform modules. They are pure so both
// platforms' formats are unit-tested on any host.

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
        fn own_process_has_a_plausible_rss() {
            let rss = self_rss_bytes().unwrap();
            assert!(rss > 64 * 1024, "rss {rss}");
            assert!(rss < 64 * 1024 * 1024 * 1024, "rss {rss}");
            assert!(cwd_of(std::process::id()).is_some());
        }
    }
}
