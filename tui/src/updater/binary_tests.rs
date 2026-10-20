use super::super::releases;
use super::*;
use flate2::{write::GzEncoder, Compression};
use std::collections::HashMap;

struct Source(HashMap<String, Vec<u8>>);
impl ReleaseSource for Source {
    fn fetch(&self, url: &str, limit: usize) -> Result<Vec<u8>> {
        let bytes = self.0.get(url).context("unexpected download URL")?.clone();
        if bytes.len() > limit {
            bail!("fixture exceeds download limit");
        }
        Ok(bytes)
    }
}

fn package(version: &str) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    for name in INSTALLED {
        let (name, bytes) = if *name == "shell" {
            ("shell/gwr_load.sh", b"#!/bin/bash\n".to_vec())
        } else if BINARIES.contains(name) {
            (
                *name,
                format!(
                    "#!/bin/bash\nprintf '{name} {version}{}\\n'\n",
                    if *name == "rakumux-ptyd" {
                        " (protocol 1)"
                    } else {
                        ""
                    }
                )
                .into_bytes(),
            )
        } else {
            (*name, b"fixture".to_vec())
        };
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(
                &mut header,
                format!("rakumux-linux-x64/{name}"),
                bytes.as_slice(),
            )
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn source(bytes: Vec<u8>, checksum: Option<&str>) -> Source {
    let hash = checksum
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{:x}", Sha256::digest(&bytes)));
    Source(HashMap::from([
        (
            format!("{RELEASES}/v0.1.5/checksums.txt"),
            format!("{hash}  rakumux-linux-x64.tar.gz\n").into_bytes(),
        ),
        (format!("{RELEASES}/v0.1.5/rakumux-linux-x64.tar.gz"), bytes),
    ]))
}

fn installed(directory: &Path) -> PathBuf {
    let root = directory.join("rakumux");
    fs::create_dir(&root).unwrap();
    for name in INSTALLED {
        if *name == "shell" {
            fs::create_dir(root.join(name)).unwrap();
            fs::write(root.join("shell/gwr_load.sh"), b"old").unwrap();
        } else {
            fs::write(root.join(name), b"old").unwrap();
        }
    }
    root
}

#[test]
fn verified_bundle_replaces_every_binary_without_touching_other_directories() {
    let directory = tempfile::tempdir().unwrap();
    let root = installed(directory.path());
    let unrelated = directory.path().join("user-config");
    fs::write(&unrelated, b"keep").unwrap();
    assert_eq!(install_root(&root.join("rakumux")), Some(root.clone()));
    let launcher = install(&source(package("0.1.5"), None), &root, "0.1.5", "linux-x64").unwrap();
    assert_eq!(launcher, root.join("rakumux.sh"));
    for binary in BINARIES {
        assert!(fs::read_to_string(root.join(binary))
            .unwrap()
            .contains("0.1.5"));
    }
    assert_eq!(fs::read(unrelated).unwrap(), b"keep");
}

#[test]
fn checksum_version_and_unknown_files_fail_before_mutating_the_installation() {
    let directory = tempfile::tempdir().unwrap();
    let root = installed(directory.path());
    let zeroes = "0".repeat(64);
    for source in [
        source(package("0.1.5"), Some(&zeroes)),
        source(package("9.9.9"), None),
    ] {
        assert!(install(&source, &root, "0.1.5", "linux-x64").is_err());
        assert_eq!(fs::read(root.join("rakumux")).unwrap(), b"old");
    }
    fs::write(root.join("user-file"), b"do not discard").unwrap();
    assert!(install(&source(package("0.1.5"), None), &root, "0.1.5", "linux-x64").is_err());
    assert_eq!(fs::read(root.join("user-file")).unwrap(), b"do not discard");
    let nested_directory = tempfile::tempdir().unwrap();
    let root = installed(nested_directory.path());
    fs::write(root.join("shell/custom.sh"), b"custom work").unwrap();
    assert!(install(&source(package("0.1.5"), None), &root, "0.1.5", "linux-x64").is_err());
    assert_eq!(
        fs::read(root.join("shell/custom.sh")).unwrap(),
        b"custom work"
    );
}

#[test]
fn replacing_an_installation_does_not_end_a_running_program_from_the_old_bundle() {
    let directory = tempfile::tempdir().unwrap();
    let root = installed(directory.path());
    fs::copy("/bin/sleep", root.join("rakumux-ptyd")).unwrap();
    let mut child = Command::new(root.join("rakumux-ptyd"))
        .arg("60")
        .spawn()
        .unwrap();
    let pid = child.id();
    let result = install(&source(package("0.1.5"), None), &root, "0.1.5", "linux-x64");
    let still_running = child.try_wait().unwrap().is_none();
    let _ = child.kill();
    let _ = child.wait();
    result.unwrap();
    assert!(still_running, "updating must not end old process {pid}");
}

#[test]
fn simultaneous_updates_fail_without_replacing_any_files() {
    let directory = tempfile::tempdir().unwrap();
    let root = installed(directory.path());
    let _held = lock(directory.path()).unwrap();
    assert!(install(&source(package("0.1.5"), None), &root, "0.1.5", "linux-x64").is_err());
    assert_eq!(fs::read(root.join("rakumux")).unwrap(), b"old");
}

#[test]
fn an_unwritable_install_prefix_returns_manual_instructions_without_mutation() {
    // Root ignores Unix write bits, so this permission scenario needs a normal user.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let directory = tempfile::tempdir().unwrap();
    let root = installed(directory.path());
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o555)).unwrap();
    let result = install(&source(package("0.1.5"), None), &root, "0.1.5", "linux-x64");
    fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let error = result.unwrap_err().to_string();
    assert!(error.contains("installer outside Rakumux") && error.contains("PREFIX"));
    assert_eq!(fs::read(root.join("rakumux")).unwrap(), b"old");
}

#[test]
fn version_probes_use_an_isolated_home_and_reject_excessive_output() {
    let directory = tempfile::tempdir().unwrap();
    let binary = directory.path().join("rakumux");
    let environment = directory.path().join("environment");
    fs::write(&binary, format!("#!/bin/bash\nprintf '%s\\n%s\\n' \"$HOME\" \"$RAKUMUX_HOME\" > '{}'\nprintf 'rakumux 0.1.5\\n'\n", environment.display())).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    verify_version(&binary, "0.1.5", directory.path()).unwrap();
    assert_eq!(
        fs::read_to_string(environment).unwrap(),
        format!(
            "{}\n{}\n",
            directory.path().join("probe-home").display(),
            directory.path().join("probe-data").display()
        )
    );
    fs::write(&binary, "#!/bin/bash\nprintf '%4096s' x\n").unwrap();
    assert!(verify_version(&binary, "0.1.5", directory.path()).is_err());
}

#[test]
fn archive_links_and_unsafe_paths_are_rejected() {
    let directory = tempfile::tempdir().unwrap();
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_mode(0o755);
    header.set_entry_type(tar::EntryType::Symlink);
    header.set_link_name("/outside").unwrap();
    header.set_cksum();
    builder
        .append_data(&mut header, "rakumux-linux-x64/shell", &[][..])
        .unwrap();
    let bytes = builder.into_inner().unwrap().finish().unwrap();
    assert!(unpack(&bytes, directory.path(), "rakumux-linux-x64").is_err());
    assert!(unpack(&package("0.1.5"), directory.path(), "other-root").is_err());
}

#[test]
fn checksums_must_identify_exactly_one_asset() {
    let checksum = format!("{:x}", Sha256::digest(b"archive"));
    assert!(verify_checksum(
        format!("{checksum}  asset\n{checksum}  asset\n").as_bytes(),
        "asset",
        b"archive"
    )
    .is_err());
    assert!(verify_checksum(
        format!("{checksum}  other\n").as_bytes(),
        "asset",
        b"archive"
    )
    .is_err());
    verify_checksum(
        format!("{checksum} *asset\n").as_bytes(),
        "asset",
        b"archive",
    )
    .unwrap();
}

#[test]
fn native_platform_names_and_stable_release_metadata_are_validated() {
    assert_eq!(platform("macos", "aarch64").unwrap(), "macos-arm64");
    assert_eq!(platform("macos", "x86_64").unwrap(), "macos-intel");
    assert_eq!(platform("linux", "x86_64").unwrap(), "linux-x64");
    assert!(platform("linux", "aarch64").is_err());
    for prerelease in [false, true] {
        let source = Source(HashMap::from([(
            "https://api.github.com/repos/humanunsupervised/rakumux/releases/latest".into(),
            serde_json::to_vec(
                &serde_json::json!({"tag_name":"v0.1.5", "draft":false, "prerelease":prerelease}),
            )
            .unwrap(),
        )]));
        assert_eq!(releases::latest(&source).is_ok(), !prerelease);
    }
}
