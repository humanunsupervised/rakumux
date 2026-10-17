//! Linux process inspection via `/proc`.

use super::{parse_statm_rss, split_nul_args, untruncated_name};
use crate::host::ProcessInfo;
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
    })
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
    // SAFETY: sysconf has no preconditions.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    parse_statm_rss(&statm, u64::try_from(page_size).ok().filter(|p| *p > 0)?)
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
