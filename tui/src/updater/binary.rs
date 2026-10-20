//! Verify and atomically replace a complete binary installation; never touch sessions.
use super::releases::{stable_version, ReleaseSource, RELEASES};
use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const MAX_ARCHIVE: usize = 128 * 1024 * 1024;
const MAX_UNPACKED: u64 = 512 * 1024 * 1024;
const BINARIES: &[&str] = &["rakumux", "rakumux-ptyd", "rakumux-api", "rakumux-mcp"];
const INSTALLED: &[&str] = &[
    "rakumux",
    "rakumux-ptyd",
    "rakumux-api",
    "rakumux-mcp",
    "rakumux.sh",
    "rakumux-local.sh",
    "shell",
    "LICENSE",
    "THIRD_PARTY_NOTICES",
];

pub(super) fn platform(os: &str, architecture: &str) -> Result<&'static str> {
    match (os, architecture) {
        ("linux", "x86_64") => Ok("linux-x64"),
        ("macos", "aarch64") => Ok("macos-arm64"),
        ("macos", "x86_64") => Ok("macos-intel"),
        _ => bail!("no binary release is available for {os}/{architecture}"),
    }
}

pub(super) fn install_root(executable: &Path) -> Option<PathBuf> {
    let root = executable.parent()?;
    (executable.file_name()? == "rakumux"
        && root.join("rakumux.sh").is_file()
        && root.join("shell/gwr_load.sh").is_file()
        && BINARIES.iter().all(|name| root.join(name).is_file()))
    .then(|| root.to_path_buf())
}

pub(super) fn install(
    source: &dyn ReleaseSource,
    root: &Path,
    version: &str,
    platform: &str,
) -> Result<PathBuf> {
    let version = stable_version(version)?;
    let root = root
        .canonicalize()
        .context("cannot locate the installed app")?;
    let parent = root.parent().context("install directory has no parent")?;
    let _lock = lock(parent)?;
    let staging = tempfile::Builder::new().prefix(".rakumux-update-").tempdir_in(parent)
        .with_context(|| format!("Cannot write {}. Update with the installer using your original PREFIX outside Rakumux; administrator permission may be required.", parent.display()))?;
    // Do not discard files somebody added to the installation directory.
    for entry in fs::read_dir(&root)? {
        let entry = entry?;
        if !INSTALLED.iter().any(|name| entry.file_name() == *name) {
            bail!(
                "unrecognized file in installation: {}; use the installer manually",
                entry.path().display()
            );
        }
    }
    let asset = format!("rakumux-{platform}.tar.gz");
    let base = format!("{RELEASES}/v{version}");
    let checksums = source.fetch(&format!("{base}/checksums.txt"), 64 * 1024)?;
    let archive = source.fetch(&format!("{base}/{asset}"), MAX_ARCHIVE)?;
    verify_checksum(&checksums, &asset, &archive)?;
    let package = unpack(&archive, staging.path(), &format!("rakumux-{platform}"))?;
    // Only the installed payload is promoted, not the tarball's installer or README.
    let candidate = staging.path().join("installation");
    fs::create_dir(&candidate)?;
    for name in INSTALLED {
        let path = package.join(name);
        if !path.exists() {
            bail!("release package is missing {name}");
        }
        fs::rename(path, candidate.join(name))?;
    }
    validate_existing_tree(&root, &candidate)?;
    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o755))?;
    for binary in BINARIES {
        fs::set_permissions(candidate.join(binary), fs::Permissions::from_mode(0o755))?;
        verify_version(
            &candidate.join(binary),
            &version.to_string(),
            staging.path(),
        )?;
    }
    for script in ["rakumux.sh", "rakumux-local.sh", "shell/gwr_load.sh"] {
        if !candidate.join(script).is_file() {
            bail!("release package is missing {script}");
        }
    }
    sync_tree(&candidate)?;
    // One kernel operation: every client sees either the old bundle or the new one.
    // Unsupported filesystems fail without renaming or deleting the current install.
    exchange(&candidate, &root)
        .context("cannot atomically replace the installation; current app was not changed")?;
    // Staging now owns the old bundle. Running Unix programs keep their mapped images.
    Ok(root.join("rakumux.sh"))
}

fn lock(parent: &Path) -> Result<File> {
    let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).mode(0o600).custom_flags(libc::O_NOFOLLOW)
        .open(parent.join(".rakumux-update.lock"))
        .context("installation is not writable; run the installer outside Rakumux with your original PREFIX")?;
    // SAFETY: `file` owns this valid descriptor and is kept alive throughout installation.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(io::Error::last_os_error()).context("another update may already be running");
    }
    Ok(file)
}

fn verify_checksum(checksums: &[u8], asset: &str, bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(checksums).context("invalid release checksums")?;
    let matches: Vec<_> = text
        .lines()
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            let checksum = words.next()?;
            let name = words.next()?.trim_start_matches('*');
            (name == asset && words.next().is_none()).then_some(checksum)
        })
        .collect();
    if matches.len() != 1
        || matches[0].len() != 64
        || !matches[0].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        bail!("release is missing a unique SHA-256 checksum for {asset}");
    }
    let actual = format!("{:x}", Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(matches[0]) {
        bail!("SHA-256 checksum mismatch; update was not installed");
    }
    Ok(())
}

fn validate_existing_tree(current: &Path, candidate: &Path) -> Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let expected = candidate.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() && expected.is_dir() {
            validate_existing_tree(&entry.path(), &expected)?;
        } else if !kind.is_file() || !expected.is_file() {
            bail!(
                "unrecognized file in installation: {}; update manually to preserve custom files",
                entry.path().display()
            );
        }
    }
    Ok(())
}

fn unpack(bytes: &[u8], directory: &Path, package: &str) -> Result<PathBuf> {
    let mut archive = tar::Archive::new(GzDecoder::new(bytes).take(MAX_UNPACKED + 1));
    let mut total = 0u64;
    for (index, entry) in archive.entries()?.enumerate() {
        if index >= 512 {
            bail!("release package contains too many entries");
        }
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
            || path.components().next() != Some(Component::Normal(package.as_ref()))
        {
            bail!("unsafe path in release package");
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            bail!("links and special files are not allowed in release packages");
        }
        total = total
            .checked_add(entry.size())
            .context("release package is too large")?;
        if total > MAX_UNPACKED {
            bail!("release package is too large");
        }
        let target = directory.join(&path);
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o755))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&target)?;
            io::copy(&mut entry, &mut file)?;
            file.flush()?;
            let mode = if target
                .extension()
                .is_some_and(|extension| extension == "sh")
            {
                0o755
            } else {
                0o644
            };
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
    }
    Ok(directory.join(package))
}

fn verify_version(binary: &Path, version: &str, staging: &Path) -> Result<()> {
    let output = staging.join("version-output");
    let file = File::create(&output)?;
    let probe_home = staging.join("probe-home");
    fs::create_dir_all(&probe_home)?;
    let mut child = VersionProbe(
        Command::new(binary)
            .arg("--version")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &probe_home)
            .env("RAKUMUX_HOME", staging.join("probe-data"))
            .stdin(Stdio::null())
            .stdout(file.try_clone()?)
            .stderr(file)
            .spawn()
            .context("release binary cannot run on this machine")?,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if fs::metadata(&output)?.len() > 2048 {
            bail!("release version check produced too much output");
        }
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            bail!("release binary version check timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if fs::metadata(&output)?.len() > 2048 {
        bail!("release version check produced too much output");
    }
    let mut text = String::new();
    File::open(&output)?.take(2048).read_to_string(&mut text)?;
    let mut words = text.split_whitespace();
    let program = binary
        .file_name()
        .and_then(|name| name.to_str())
        .context("invalid release binary name")?;
    // The daemon also prints `(protocol N)` after its version.
    if !status.success() || words.next() != Some(program) || words.next() != Some(version) {
        bail!("release binary {program} did not report requested version {version}");
    }
    Ok(())
}

/// Reap only the version-probe child, including on timeout or I/O failure.
struct VersionProbe(Child);

impl Drop for VersionProbe {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn sync_tree(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_tree(&entry.path())?;
        } else {
            File::open(entry.path())?.sync_all()?;
        }
    }
    File::open(path)?.sync_all()?;
    Ok(())
}

fn exchange(left: &Path, right: &Path) -> io::Result<()> {
    let left = CString::new(left.as_os_str().as_bytes())?;
    let right = CString::new(right.as_os_str().as_bytes())?;
    // SAFETY: both paths are valid NUL-terminated strings owned throughout this call.
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            left.as_ptr(),
            libc::AT_FDCWD,
            right.as_ptr(),
            libc::RENAME_EXCHANGE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe { libc::renamex_np(left.as_ptr(), right.as_ptr(), libc::RENAME_SWAP) };
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    return Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic updates are unsupported on this platform",
    ));
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
#[path = "binary_tests.rs"]
mod tests;
