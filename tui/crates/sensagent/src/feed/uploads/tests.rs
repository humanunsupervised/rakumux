use std::os::unix::fs::symlink;

use super::*;

const REPLY: &str = "01K8A1B2C3D4E5F6G7H8J9K0MN";
const OTHER: &str = "01K8A1B2C3D4E5F6G7H8J9K0MP";

fn temp_home() -> (tempfile::TempDir, Uploads) {
    let home = tempfile::tempdir().unwrap();
    let uploads = Uploads::new(home.path().join("uploads"));
    (home, uploads)
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().mode() & 0o777
}

fn name(path: &Path) -> &str {
    path.file_name().unwrap().to_str().unwrap()
}

#[test]
fn a_file_is_saved_privately_under_its_replys_directory() {
    let (home, uploads) = temp_home();
    let path = uploads
        .save(REPLY, "shot.png", &[], b"\x89PNG", true)
        .unwrap();
    assert_eq!(
        path,
        home.path().join("uploads").join(REPLY).join("shot.png")
    );
    assert_eq!(fs::read(&path).unwrap(), b"\x89PNG");
    assert_eq!(mode(&home.path().join("uploads")), 0o700);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    assert_eq!(mode(&path), 0o600);
}

#[test]
fn a_replys_first_file_starts_its_directory_afresh() {
    let (_home, uploads) = temp_home();
    let left = uploads.save(REPLY, "old.txt", &[], b"x", true).unwrap();
    let path = uploads.save(REPLY, "new.txt", &[], b"y", true).unwrap();
    assert!(!left.exists(), "an earlier attempt's files go");
    assert!(path.exists());
    // Later files join it.
    let next = uploads
        .save(REPLY, "next.txt", &["new.txt".into()], b"z", false)
        .unwrap();
    assert_eq!(next.parent(), path.parent());
    // A later file needs the directory its first made.
    assert_eq!(
        uploads.save(OTHER, "a.txt", &[], b"z", false),
        Err(SaveError::Io)
    );
}

#[test]
fn files_of_one_name_get_their_own_ignoring_case() {
    let (_home, uploads) = temp_home();
    let first = uploads.save(REPLY, "shot.png", &[], b"1", true).unwrap();
    let taken = vec![name(&first).to_owned()];
    let second = uploads
        .save(REPLY, "shot.png", &taken, b"2", false)
        .unwrap();
    assert_eq!(name(&second), "shot (2).png");
    let taken = vec!["shot.png".into(), "shot (2).png".into()];
    let third = uploads
        .save(REPLY, "SHOT.PNG", &taken, b"3", false)
        .unwrap();
    assert_eq!(
        name(&third),
        "SHOT (3).PNG",
        "names equal ignoring case differ"
    );
    assert_eq!(fs::read(&first).unwrap(), b"1");
    assert_eq!(fs::read(&second).unwrap(), b"2");

    // A file already there by another spelling (macOS's normalisations) is
    // never written over: the next name is.
    fs::write(first.with_file_name("notes.txt"), b"theirs").unwrap();
    let path = uploads
        .save(REPLY, "notes.txt", &[], b"ours", false)
        .unwrap();
    assert_eq!(name(&path), "notes (2).txt");
    assert_eq!(
        fs::read(first.with_file_name("notes.txt")).unwrap(),
        b"theirs"
    );
}

#[test]
fn numbered_names_keep_their_extension_and_fit_255_bytes() {
    assert_eq!(numbered("shot.png", 2), "shot (2).png");
    assert_eq!(numbered(".env", 2), ".env (2)");
    assert_eq!(numbered("archive.tar.gz", 3), "archive.tar (3).gz");
    assert_eq!(numbered("README", 2), "README (2)");
    let long = format!("{}.txt", "é".repeat(125));
    let cut = numbered(&long, 12);
    assert!(cut.len() <= 255, "{}", cut.len());
    assert!(cut.ends_with("é (12).txt"));
    // A dot far from the end isn't an extension's.
    let dotted = format!("v1.{}", "x".repeat(60));
    assert_eq!(numbered(&dotted, 2), format!("{dotted} (2)"));
}

#[test]
fn symlinks_and_other_things_in_the_way_are_refused() {
    // `uploads` a symlink: refused, nothing written where it points.
    let (home, uploads) = temp_home();
    let elsewhere = home.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    symlink(&elsewhere, home.path().join("uploads")).unwrap();
    assert_eq!(
        uploads.save(REPLY, "a.txt", &[], b"x", true),
        Err(SaveError::Refused)
    );
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);

    // `uploads` a file.
    let (home, uploads) = temp_home();
    fs::write(home.path().join("uploads"), b"").unwrap();
    assert_eq!(
        uploads.save(REPLY, "a.txt", &[], b"x", true),
        Err(SaveError::Refused)
    );

    // The reply's directory a symlink, for its first file or a later one.
    let (home, uploads) = temp_home();
    let elsewhere = home.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::create_dir(home.path().join("uploads")).unwrap();
    symlink(&elsewhere, home.path().join("uploads").join(REPLY)).unwrap();
    for first in [true, false] {
        assert_eq!(
            uploads.save(REPLY, "a.txt", &[], b"x", first),
            Err(SaveError::Refused)
        );
    }
    assert_eq!(fs::read_dir(&elsewhere).unwrap().count(), 0);
    assert!(home.path().join("uploads").join(REPLY).is_symlink());

    // A symlink where the file goes is never followed.
    let (home, uploads) = temp_home();
    let target = home.path().join("target");
    fs::write(&target, b"keep").unwrap();
    let dir = uploads
        .save(REPLY, "first.txt", &[], b"x", true)
        .unwrap()
        .with_file_name("a.txt");
    symlink(&target, &dir).unwrap();
    let path = uploads
        .save(REPLY, "a.txt", &["first.txt".into()], b"x", false)
        .unwrap();
    assert_eq!(name(&path), "a (2).txt");
    assert_eq!(fs::read(&target).unwrap(), b"keep");
}

#[test]
fn only_a_replys_id_and_a_files_name_make_its_path() {
    let (home, uploads) = temp_home();
    for bad in [
        "../escape",
        "a/b",
        "a\\b",
        ".",
        "..",
        "",
        "bell\u{7}",
        "\u{202e}gpj.exe",
    ] {
        assert_eq!(
            uploads.save(REPLY, bad, &[], b"x", true),
            Err(SaveError::Refused),
            "{bad:?}"
        );
    }
    assert_eq!(
        uploads.save("../01K8A1B2C3D4E5F6G7H8J9K0", "a.txt", &[], b"x", true),
        Err(SaveError::Refused)
    );
    assert!(!home.path().join("escape").exists());
    // A name with spaces and other scripts is a name.
    let path = uploads
        .save(
            REPLY,
            "Screen Shot 2026-10-26 at 10.00.png",
            &[],
            b"x",
            true,
        )
        .unwrap();
    assert!(path.exists());
}

#[test]
fn a_discarded_replys_files_go_and_nothing_through_a_symlink() {
    let (home, uploads) = temp_home();
    let path = uploads.save(REPLY, "a.txt", &[], b"x", true).unwrap();
    uploads.discard(REPLY);
    assert!(!path.exists() && !path.parent().unwrap().exists());
    uploads.discard(REPLY);

    let elsewhere = home.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    fs::write(elsewhere.join("keep"), b"").unwrap();
    symlink(&elsewhere, home.path().join("uploads").join(OTHER)).unwrap();
    uploads.discard(OTHER);
    assert!(elsewhere.join("keep").exists());
}

/// Set `path`'s modification time `age` ago.
fn aged(path: &Path, age: Duration) {
    File::open(path)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

#[test]
fn clean_up_removes_replies_directories_after_seven_days_only() {
    let (home, uploads) = temp_home();
    assert_eq!(uploads.clean(SystemTime::now()), 0, "no uploads yet");
    let old = uploads.save(REPLY, "a.txt", &[], b"x", true).unwrap();
    let new = uploads.save(OTHER, "b.txt", &[], b"x", true).unwrap();
    let root = home.path().join("uploads");
    // Not made here: left alone, however old.
    let mine = root.join("notes");
    fs::create_dir(&mine).unwrap();
    let elsewhere = home.path().join("elsewhere");
    fs::create_dir(&elsewhere).unwrap();
    let linked = root.join("01K8A1B2C3D4E5F6G7H8J9K0MQ");
    symlink(&elsewhere, &linked).unwrap();
    let eight_days = Duration::from_secs(8 * 24 * 3600);
    for dir in [old.parent().unwrap(), &mine, &elsewhere] {
        aged(dir, eight_days);
    }
    aged(new.parent().unwrap(), KEEP - Duration::from_secs(3600));

    assert_eq!(uploads.clean(SystemTime::now()), 1);
    assert!(!old.parent().unwrap().exists());
    assert!(new.exists());
    assert!(mine.exists());
    assert!(linked.is_symlink() && elsewhere.exists());

    // `uploads` a symlink: nothing behind it is touched.
    let (home, uploads) = temp_home();
    let behind = home.path().join("behind");
    fs::create_dir_all(behind.join(REPLY)).unwrap();
    aged(&behind.join(REPLY), eight_days);
    symlink(&behind, home.path().join("uploads")).unwrap();
    assert_eq!(uploads.clean(SystemTime::now()), 0);
    assert!(behind.join(REPLY).exists());
}
