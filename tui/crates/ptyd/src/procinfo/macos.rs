//! macOS process inspection via libproc (`proc_name`, `proc_pidpath`,
//! `proc_pidinfo`) and `sysctl(KERN_PROCARGS2)`.

use super::{arg_basename, name_from_argv0, parse_procargs2};
use crate::host::ProcessInfo;
use std::ffi::OsStr;
use std::mem::{size_of, MaybeUninit};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;

fn to_pid(pid: u32) -> Option<libc::pid_t> {
    libc::pid_t::try_from(pid).ok().filter(|p| *p > 0)
}

pub(super) fn process_info(pid: u32) -> Option<ProcessInfo> {
    let raw = to_pid(pid)?;
    // Fails once the process is gone; also the fallback name.
    let kernel_name = kernel_name(raw)?;
    let cmdline = procargs(raw).unwrap_or_default();
    // The kernel's name is the resolved executable (Claude Code's native
    // install is a file named after its version); argv[0] is what the user
    // ran, like Linux's comm.
    let name = name_from_argv0(&cmdline).unwrap_or(kernel_name);
    Some(ProcessInfo { pid, name, cmdline })
}

fn kernel_name(pid: libc::pid_t) -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: the buffer is valid for `buf.len()` bytes.
    let len = unsafe { libc::proc_name(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if let Some(len) = usize::try_from(len).ok().filter(|l| *l > 0) {
        return Some(String::from_utf8_lossy(&buf[..len.min(buf.len())]).into_owned());
    }
    let path = executable_path(pid)?;
    let path = path.to_string_lossy();
    Some(arg_basename(&path).to_string())
}

fn executable_path(pid: libc::pid_t) -> Option<PathBuf> {
    let mut buf = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is valid for `buf.len()` bytes.
    let len = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    let len = usize::try_from(len).ok().filter(|l| *l > 0)?;
    Some(PathBuf::from(OsStr::from_bytes(&buf[..len.min(buf.len())])))
}

/// `argv` from `KERN_PROCARGS2` (readable for the caller's own processes).
fn procargs(pid: libc::pid_t) -> Option<Vec<String>> {
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let mut argmax: libc::c_int = 0;
    let mut size = size_of::<libc::c_int>();
    // SAFETY: `argmax` is valid for `size` bytes; no new value is set.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            ptr::addr_of_mut!(argmax).cast(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let mut buf = vec![0u8; usize::try_from(argmax).ok().filter(|n| *n > 0)?];
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size = buf.len();
    // SAFETY: `buf` is valid for `size` bytes; no new value is set.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr().cast(),
            &mut size,
            ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(size);
    parse_procargs2(&buf)
}

pub(super) fn cwd_of(pid: u32) -> Option<PathBuf> {
    let raw = to_pid(pid)?;
    let mut info = MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let size = size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: `info` is valid for `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            raw,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // `vip_path` is `char[MAXPATHLEN]` in C; read it as bytes through a raw
    // pointer so this doesn't depend on how libc shapes the Rust array.
    let path_ptr = unsafe { ptr::addr_of!((*info.as_ptr()).pvi_cdir.vip_path) };
    let path_len = size_of_pointee(path_ptr);
    // SAFETY: proc_pidinfo filled the whole struct (checked above), and the
    // field spans `path_len` bytes of it.
    let bytes = unsafe { std::slice::from_raw_parts(path_ptr.cast::<u8>(), path_len) };
    let end = bytes.iter().position(|b| *b == 0)?;
    (end > 0).then(|| PathBuf::from(OsStr::from_bytes(&bytes[..end])))
}

fn size_of_pointee<T>(_: *const T) -> usize {
    size_of::<T>()
}

pub(super) fn self_rss_bytes() -> Option<u64> {
    let pid = to_pid(std::process::id())?;
    let mut info = MaybeUninit::<libc::proc_taskinfo>::zeroed();
    let size = size_of::<libc::proc_taskinfo>() as libc::c_int;
    // SAFETY: `info` is valid for `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: fully written by proc_pidinfo; all fields are plain integers.
    Some(unsafe { info.assume_init() }.pti_resident_size)
}
