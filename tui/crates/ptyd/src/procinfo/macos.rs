//! macOS process inspection via libproc (`proc_name`, `proc_pidpath`,
//! `proc_pidinfo`, `proc_listallpids`) and `sysctl(KERN_PROCARGS2)`; the
//! machine's counters from Mach host statistics, `sysctl(HW_MEMSIZE)` and
//! `getifaddrs`.

use super::{arg_basename, logical_cpus, name_from_argv0, parse_procargs2, ProcStat};
use crate::host::ProcessInfo;
use session_core::api::MachineUsage;
use std::ffi::OsStr;
use std::mem::{size_of, MaybeUninit};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::ptr;
use std::sync::OnceLock;

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
    Some(ProcessInfo {
        pid,
        name,
        cmdline,
        exe: executable_path(raw),
    })
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

pub(super) fn process_table() -> Vec<ProcStat> {
    let timebase = timebase();
    all_pids()
        .into_iter()
        .filter_map(|pid| task_stat(pid, timebase))
        .collect()
}

/// Every pid, with room for processes started while it's read.
fn all_pids() -> Vec<libc::pid_t> {
    // SAFETY: a null buffer asks how many there are.
    let count = unsafe { libc::proc_listallpids(ptr::null_mut(), 0) };
    let Some(count) = usize::try_from(count).ok().filter(|n| *n > 0) else {
        return Vec::new();
    };
    let mut pids: Vec<libc::pid_t> = vec![0; count + 64];
    let bytes = libc::c_int::try_from(pids.len() * size_of::<libc::pid_t>()).unwrap_or(0);
    // SAFETY: `pids` is valid for `bytes` bytes.
    let found = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    pids.truncate(usize::try_from(found).unwrap_or(0).min(pids.len()));
    pids.retain(|pid| *pid > 0);
    pids
}

/// Parent, CPU time and resident memory of `pid`; None for another user's
/// process, a zombie, or one that's gone.
fn task_stat(pid: libc::pid_t, timebase: (u64, u64)) -> Option<ProcStat> {
    let mut info = MaybeUninit::<libc::proc_taskallinfo>::zeroed();
    let size = size_of::<libc::proc_taskallinfo>() as libc::c_int;
    // SAFETY: `info` is valid for `size` bytes.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKALLINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: fully written by proc_pidinfo; all fields are plain data.
    let info = unsafe { info.assume_init() };
    // In Mach absolute time units, which are nanoseconds only on Intel.
    let ticks = info
        .ptinfo
        .pti_total_user
        .saturating_add(info.ptinfo.pti_total_system);
    let nanos = u128::from(ticks) * u128::from(timebase.0) / u128::from(timebase.1);
    Some(ProcStat {
        pid: info.pbsd.pbi_pid,
        ppid: info.pbsd.pbi_ppid,
        cpu_ms: u64::try_from(nanos / 1_000_000).unwrap_or(u64::MAX),
        rss_bytes: info.ptinfo.pti_resident_size,
    })
}

/// Mach absolute time's numerator and denominator to nanoseconds.
// The `mach2` crate replaces libc's Mach bindings; these two calls don't
// warrant a dependency.
#[allow(deprecated)]
fn timebase() -> (u64, u64) {
    let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
    // SAFETY: `info` is valid for writing.
    let rc = unsafe { libc::mach_timebase_info(&mut info) };
    if rc != 0 || info.numer == 0 || info.denom == 0 {
        return (1, 1);
    }
    (u64::from(info.numer), u64::from(info.denom))
}

/// The host port, asked for once (each ask adds a reference to it).
#[allow(deprecated)]
fn host() -> libc::mach_port_t {
    static HOST: OnceLock<libc::mach_port_t> = OnceLock::new();
    // SAFETY: mach_host_self has no preconditions.
    *HOST.get_or_init(|| unsafe { libc::mach_host_self() })
}

pub(super) fn machine_usage() -> MachineUsage {
    let cpu = cpu_times();
    let net = interface_bytes();
    MachineUsage {
        cpus: logical_cpus(),
        cpu_busy_ms: cpu.map(|(busy, _)| busy),
        cpu_total_ms: cpu.map(|(_, total)| total),
        memory_total_bytes: memory_size(),
        memory_available_bytes: memory_available(),
        net_rx_bytes: net.map(|(rx, _)| rx),
        net_tx_bytes: net.map(|(_, tx)| tx),
    }
}

/// Busy and total CPU time of every CPU, in ms (the host's load ticks).
fn cpu_times() -> Option<(u64, u64)> {
    let mut info = MaybeUninit::<libc::host_cpu_load_info>::zeroed();
    let mut count = libc::HOST_CPU_LOAD_INFO_COUNT;
    // SAFETY: `info` holds `count` integers.
    let rc = unsafe {
        libc::host_statistics(
            host(),
            libc::HOST_CPU_LOAD_INFO,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: written by host_statistics; plain integers.
    let ticks = unsafe { info.assume_init() }.cpu_ticks;
    let total: u64 = ticks.iter().map(|t| u64::from(*t)).sum();
    let idle = u64::from(ticks[libc::CPU_STATE_IDLE as usize]);
    let ms = |t: u64| t.saturating_mul(1000) / clock_ticks();
    Some((ms(total.saturating_sub(idle)), ms(total)))
}

fn clock_ticks() -> u64 {
    // SAFETY: sysconf has no preconditions.
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    u64::try_from(ticks).ok().filter(|t| *t > 0).unwrap_or(100)
}

fn memory_size() -> Option<u64> {
    let mut mib = [libc::CTL_HW, libc::HW_MEMSIZE];
    let mut size: u64 = 0;
    let mut len = size_of::<u64>();
    // SAFETY: `size` is valid for `len` bytes; no new value is set.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            ptr::addr_of_mut!(size).cast(),
            &mut len,
            ptr::null_mut(),
            0,
        )
    };
    (rc == 0 && size > 0).then_some(size)
}

/// Free (less speculative) and inactive pages, as `psutil` counts
/// available memory on macOS.
fn memory_available() -> Option<u64> {
    let mut info = MaybeUninit::<libc::vm_statistics64>::zeroed();
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `info` holds `count` integers.
    let rc = unsafe {
        libc::host_statistics64(
            host(),
            libc::HOST_VM_INFO64,
            info.as_mut_ptr().cast(),
            &mut count,
        )
    };
    if rc != 0 {
        return None;
    }
    // SAFETY: written by host_statistics64; plain integers.
    let info = unsafe { info.assume_init() };
    // SAFETY: sysconf has no preconditions.
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    let pages = u64::from(info.free_count.saturating_sub(info.speculative_count))
        + u64::from(info.inactive_count);
    Some(pages.saturating_mul(page))
}

/// Bytes received and sent on every interface but loopback. The kernel
/// keeps them in 32 bits here, so they wrap every 4 GiB.
fn interface_bytes() -> Option<(u64, u64)> {
    let mut list: *mut libc::ifaddrs = ptr::null_mut();
    // SAFETY: `list` receives a list freed below.
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return None;
    }
    let mut totals = (0u64, 0u64);
    let mut entry = list;
    while !entry.is_null() {
        // SAFETY: getifaddrs returned a valid list; `entry` is in it.
        let ifa = unsafe { &*entry };
        entry = ifa.ifa_next;
        let loopback = ifa.ifa_flags & (libc::IFF_LOOPBACK as libc::c_uint) != 0;
        // SAFETY: a non-null ifa_addr points to a sockaddr.
        let link = !ifa.ifa_addr.is_null()
            && i32::from(unsafe { (*ifa.ifa_addr).sa_family }) == libc::AF_LINK;
        if loopback || !link || ifa.ifa_data.is_null() {
            continue;
        }
        // SAFETY: an AF_LINK entry's ifa_data is its `if_data`.
        let data = unsafe { &*(ifa.ifa_data as *const libc::if_data) };
        totals.0 = totals.0.saturating_add(u64::from(data.ifi_ibytes));
        totals.1 = totals.1.saturating_add(u64::from(data.ifi_obytes));
    }
    // SAFETY: `list` came from getifaddrs and is freed once.
    unsafe { libc::freeifaddrs(list) };
    Some(totals)
}
