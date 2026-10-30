//! Linux process inspection via `/proc`.

use super::{
    logical_cpus, parse_cpu_times, parse_meminfo, parse_net_dev, parse_proc_stat, parse_statm_rss,
    split_nul_args, untruncated_name, ProcStat,
};
use crate::host::ProcessInfo;
use session_core::api::MachineUsage;
use std::io;
use std::path::PathBuf;

pub(super) fn process_info(pid: u32) -> Option<ProcessInfo> {
    if pid == 0 {
        return None;
    }
    let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
    let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
        Ok(bytes) => split_nul_args(&bytes),
        // Exited between the two reads.
        Err(e) if e.kind() == io::ErrorKind::NotFound => return None,
        // Zombies and restricted processes: name only.
        Err(_) => Vec::new(),
    };
    let comm = comm.strip_suffix('\n').unwrap_or(&comm);
    Some(ProcessInfo {
        pid,
        name: untruncated_name(comm, &cmdline),
        cmdline,
        exe: executable(pid),
    })
}

/// `/proc/<pid>/exe`: unreadable for other users' processes. A file replaced
/// since it started (an agent that updated itself) is still named by its
/// path, without the kernel's ` (deleted)`.
fn executable(pid: u32) -> Option<PathBuf> {
    let path = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    match path.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
        Some(original) => Some(PathBuf::from(original)),
        None => Some(path),
    }
}

pub(super) fn cwd_of(pid: u32) -> Option<PathBuf> {
    if pid == 0 {
        return None;
    }
    let path = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
    // The kernel marks a removed directory with a suffix that isn't part of
    // any real path.
    if let Some(original) = path.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
        if !path.exists() {
            return Some(PathBuf::from(original));
        }
    }
    Some(path)
}

/// How deep [`descendants`] looks.
const DESCENDANT_DEPTH: usize = 4;

pub(super) fn descendants(pid: u32) -> Vec<u32> {
    let mut found = Vec::new();
    let mut level = vec![pid];
    for _ in 0..DESCENDANT_DEPTH {
        let next: Vec<u32> = level.iter().flat_map(|p| children(*p)).collect();
        if next.is_empty() {
            break;
        }
        found.extend(&next);
        level = next;
    }
    found
}

/// Children of every thread of `pid` (`/proc/<pid>/task/<tid>/children`).
fn children(pid: u32) -> Vec<u32> {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return Vec::new();
    };
    tasks
        .filter_map(|task| std::fs::read_to_string(task.ok()?.path().join("children")).ok())
        .flat_map(|list| {
            list.split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect::<Vec<u32>>()
        })
        .collect()
}

pub(super) fn open_files(pid: u32) -> Vec<PathBuf> {
    let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    fds.filter_map(|fd| std::fs::read_link(fd.ok()?.path()).ok())
        .filter(|path| path.is_absolute())
        .collect()
}

pub(super) fn self_rss_bytes() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    parse_statm_rss(&statm, page_size()?)
}

fn page_size() -> Option<u64> {
    // SAFETY: sysconf has no preconditions.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    u64::try_from(page_size).ok().filter(|p| *p > 0)
}

/// Clock ticks per second (`/proc`'s CPU time unit).
fn ticks_per_sec() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(ticks).ok().filter(|t| *t > 0).unwrap_or(100)
}

pub(super) fn process_table() -> Vec<ProcStat> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    let (ticks, page) = (ticks_per_sec(), page_size().unwrap_or(4096));
    entries
        .filter_map(|entry| {
            let name = entry.ok()?.file_name();
            let pid: u32 = name.to_str()?.parse().ok()?;
            // Gone between the listing and the read: left out.
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            parse_proc_stat(&stat, ticks, page)
        })
        .collect()
}

pub(super) fn machine_usage() -> MachineUsage {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    let cpu = read("/proc/stat").and_then(|stat| parse_cpu_times(&stat, ticks_per_sec()));
    let (memory_total_bytes, memory_available_bytes) = read("/proc/meminfo")
        .map(|meminfo| parse_meminfo(&meminfo))
        .unwrap_or_default();
    let net = read("/proc/net/dev").and_then(|dev| parse_net_dev(&dev));
    MachineUsage {
        cpus: logical_cpus(),
        cpu_busy_ms: cpu.map(|(busy, _)| busy),
        cpu_total_ms: cpu.map(|(_, total)| total),
        memory_total_bytes,
        memory_available_bytes,
        net_rx_bytes: net.map(|(rx, _)| rx),
        net_tx_bytes: net.map(|(_, tx)| tx),
    }
}

/// Inodes of the sockets `pid` has open (`/proc/<pid>/fd/*` → `socket:[N]`).
pub(super) fn socket_inodes(pid: u32) -> Vec<u64> {
    let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    fds.filter_map(|fd| {
        let target = std::fs::read_link(fd.ok()?.path()).ok()?;
        let target = target.to_str()?;
        target
            .strip_prefix("socket:[")?
            .strip_suffix(']')?
            .parse()
            .ok()
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn children_and_their_open_files_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("held.txt");
        std::fs::write(&file, "x").unwrap();
        // A shell that keeps the file open and waits in a child.
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("exec 3<{}; sleep 30 & wait", file.display()))
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut found = (Vec::new(), Vec::new());
        for _ in 0..100 {
            found = (descendants(pid), open_files(pid));
            if !found.0.is_empty() && found.1.contains(&file) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let _ = child.kill();
        let _ = child.wait();
        assert!(!found.0.is_empty(), "no children of {pid}");
        assert!(found.1.contains(&file), "{:?}", found.1);
    }

    #[test]
    fn deleted_cwd_loses_the_kernel_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let gone = dir.path().join("gone");
        std::fs::create_dir(&gone).unwrap();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .current_dir(&gone)
            .spawn()
            .unwrap();
        std::fs::remove_dir(&gone).unwrap();
        let cwd = cwd_of(child.id());
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(
            cwd,
            Some(std::fs::canonicalize(dir.path()).unwrap().join("gone"))
        );
    }
}
