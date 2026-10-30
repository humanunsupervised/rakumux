//! Read-only git context of a directory, derived from `.git` files alone:
//! the daemon never runs `git`. Feeds `SessionView.git` and
//! `ProjectView.git`.
//!
//! The nearest enclosing `.git` decides:
//! - a directory: the repository's main worktree;
//! - a file `gitdir: <path>`: a linked worktree when `<path>` has a
//!   `commondir` file or is `…/worktrees/<name>` (`worktree` = `<name>`);
//!   otherwise a submodule (`…/modules/<name>`) or a separate git dir,
//!   which are reported like a main worktree.
//!
//! `HEAD` in the (per-worktree) git dir gives the branch
//! (`ref: refs/heads/<branch>`) or, when detached, the commit id.
//!
//! The repository's name ([`GitInfo::repo`]) is its `origin` remote's,
//! from `config` in the common git dir (a linked worktree's `commondir`):
//! the URL's last path component without `.git`. Without one, it's the main
//! working tree's folder name: the working tree itself unless it's a linked
//! worktree, else the common dir's parent (`<main>/.git`, `<main>/.bare`) or
//! a bare repository's own name (`project.git`). A submodule is a
//! repository of its own (its `origin`, else its folder).

use session_core::model::GitInfo;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Largest `.git` file or `HEAD` read; git's own are a line long.
const MAX_FILE_BYTES: u64 = 4096;
/// Largest part of a `config` read; a repository's own is a few hundred
/// bytes.
const MAX_CONFIG_BYTES: u64 = 64 * 1024;
/// Longest `GitInfo::repo`, in characters.
const MAX_NAME_CHARS: usize = 128;
/// Length of `GitInfo::detached_at`.
const SHORT_ID: usize = 7;
/// Branch name git writes into `HEAD` of reftable repositories, whose
/// real HEAD lives in the reftable.
const REFTABLE_PLACEHOLDER: &str = ".invalid";

/// Git context of `dir` (absolute). `None` outside a repository, or when
/// the nearest `.git` or its `HEAD` can't be read or parsed. A branch or
/// worktree name that isn't [`displayable`] is left out.
pub fn detect(dir: &Path) -> Option<GitInfo> {
    let root = dir
        .ancestors()
        .find(|ancestor| std::fs::metadata(ancestor.join(".git")).is_ok())?;
    let dot_git = root.join(".git");
    let (git_dir, worktree) = if std::fs::metadata(&dot_git).ok()?.is_dir() {
        (dot_git, None)
    } else {
        let target = parse_gitdir(&read_small(&dot_git)?)?;
        let git_dir = normalize(&root.join(target));
        let worktree = linked_worktree(&git_dir);
        (git_dir, worktree)
    };
    let head = parse_head(&read_small(&git_dir.join("HEAD"))?)?;
    let (branch, detached_at) = match head {
        Head::Branch(branch) => (Some(branch), None),
        Head::Detached(id) => (None, Some(id)),
        Head::Unknown => (None, None),
    };
    let repo = repo_name(root, &git_dir, worktree.is_some());
    let origin = read_config(&common_dir(&git_dir).join("config"))
        .and_then(|config| origin_url(&config))
        .and_then(|url| without_credentials(&url));
    Some(GitInfo {
        repo_root: root.to_path_buf(),
        branch: branch.filter(|name| displayable(name)),
        detached_at,
        worktree: worktree.filter(|name| displayable(name)),
        repo,
        origin,
    })
}

/// `url` without a user and password (`https://user:token@host/…`); `None`
/// when it isn't one line of printable text. scp-like `git@host:path` URLs
/// keep their user, which names no secret.
fn without_credentials(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() || url.chars().any(char::is_control) {
        return None;
    }
    match url.split_once("://") {
        Some((scheme, rest)) => {
            let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
            let host = authority
                .rsplit_once('@')
                .map_or(authority, |(_, host)| host);
            Some(format!("{scheme}://{host}{path}"))
        }
        None => Some(url.to_string()),
    }
}

/// The name of the repository whose working tree is `root` (its git dir
/// `git_dir`, a `linked` worktree's admin dir or not): its `origin`
/// remote's, else its main working tree's folder name. `None` when the
/// common git dir isn't there, or its `config` can't be read or isn't text.
fn repo_name(root: &Path, git_dir: &Path, linked: bool) -> Option<String> {
    let common = common_dir(git_dir);
    if !common.is_dir() {
        return None;
    }
    let from_origin = origin_url(&read_config(&common.join("config"))?)
        .as_deref()
        .and_then(name_of_url)
        .filter(|name| displayable(name));
    from_origin
        .or_else(|| {
            let main = if linked {
                main_folder_of(&common)?
            } else {
                root.file_name()?.to_str()?
            };
            Some(main.to_string())
        })
        .filter(|name| displayable(name))
}

/// The git dir shared by every worktree of `git_dir`'s repository: its
/// `commondir` (a linked worktree's, relative to it or absolute), else for
/// an admin dir without one its `<common>/worktrees/<name>` parent's parent,
/// else `git_dir` itself.
fn common_dir(git_dir: &Path) -> PathBuf {
    let listed = read_small(&git_dir.join("commondir"));
    if let Some(common) = listed
        .as_deref()
        .and_then(|text| text.lines().next())
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        return normalize(&git_dir.join(common));
    }
    match git_dir.parent() {
        Some(parent) if parent.file_name().is_some_and(|name| name == "worktrees") => {
            parent.parent().unwrap_or(parent).to_path_buf()
        }
        _ => git_dir.to_path_buf(),
    }
}

/// The main working tree's folder name from the common git dir: its parent
/// for a dot-named one (`<main>/.git`, `<main>/.bare`), else a bare
/// repository's own name without `.git` (`project.git` → `project`).
fn main_folder_of(common: &Path) -> Option<&str> {
    let name = common.file_name()?.to_str()?;
    let bare = name.strip_suffix(".git").unwrap_or(name);
    if bare.is_empty() || bare.starts_with('.') {
        common.parent()?.file_name()?.to_str()
    } else {
        Some(bare)
    }
}

/// A `config`'s text: empty when there's none (git reads none as empty),
/// `None` when it can't be read or isn't text (a NUL byte). Only the first
/// [`MAX_CONFIG_BYTES`] are read, without a line they'd cut.
fn read_config(path: &Path) -> Option<String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(String::new()),
        Err(_) => return None,
    };
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES).read_to_end(&mut bytes).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    if bytes.len() as u64 == MAX_CONFIG_BYTES {
        let end = bytes
            .iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |at| at + 1);
        bytes.truncate(end);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// The first `url` of `[remote "origin"]` (or the old `[remote.origin]`)
/// in a `config`'s text.
fn origin_url(config: &str) -> Option<String> {
    let mut in_origin = false;
    for line in config.lines() {
        let mut rest = line.trim_start();
        if let Some(header) = rest.strip_prefix('[') {
            let Some((section, after)) = header.split_once(']') else {
                in_origin = false;
                continue;
            };
            in_origin = is_origin(section);
            rest = after;
        }
        if !in_origin {
            continue;
        }
        let Some((key, value)) = rest.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case("url") {
            return config_value(value);
        }
    }
    None
}

/// Whether a section header names the `origin` remote.
fn is_origin(section: &str) -> bool {
    let section = section.trim();
    match section.split_once(char::is_whitespace) {
        Some((name, sub)) => name.eq_ignore_ascii_case("remote") && sub.trim() == "\"origin\"",
        None => section.eq_ignore_ascii_case("remote.origin"),
    }
}

/// A `config` value as git reads it: quotes removed, escapes resolved, up
/// to a comment (`;` or `#` outside quotes), trimmed. `None` when empty or
/// continued on the next line.
fn config_value(raw: &str) -> Option<String> {
    let mut value = String::new();
    let mut quoted = false;
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' => value.push(match chars.next()? {
                'n' => '\n',
                't' => '\t',
                'b' => '\u{8}',
                other => other,
            }),
            ';' | '#' if !quoted => break,
            c => value.push(c),
        }
    }
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// The repository's name in a remote's URL: the last component of its
/// path without `.git` (`git@github.com:org/rakumux.git`,
/// `https://github.com/org/focal-md/`, `/srv/git/focal-md.git`). `None`
/// for a URL without a path (`https://github.com`).
fn name_of_url(url: &str) -> Option<String> {
    let path = match url.split_once("://") {
        // `scheme://host[:port]/path`: the path.
        Some((_, rest)) => rest.split_once('/').map_or("", |(_, path)| path),
        // `[user@]host:path`, or a path on this machine.
        None => url,
    };
    path.rsplit(['/', ':'])
        .map(|part| part.strip_suffix(".git").unwrap_or(part))
        .find(|part| !matches!(*part, "" | "." | ".."))
        .map(str::to_string)
}

/// A name worth showing: something to read, nothing that moves the cursor.
fn displayable(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && !name
            .chars()
            .any(|c| c.is_control() || c == char::REPLACEMENT_CHARACTER)
}

/// What `HEAD` points at.
#[derive(Debug, PartialEq, Eq)]
enum Head {
    Branch(String),
    /// Short commit id.
    Detached(String),
    /// A repository whose HEAD isn't kept in the `HEAD` file (reftable).
    Unknown,
}

fn parse_head(text: &str) -> Option<Head> {
    let line = text.lines().next()?.trim();
    if let Some(target) = line.strip_prefix("ref:") {
        let target = target.trim();
        let branch = target.strip_prefix("refs/heads/").unwrap_or(target);
        return match branch {
            "" => None,
            REFTABLE_PLACEHOLDER => Some(Head::Unknown),
            branch => Some(Head::Branch(branch.to_string())),
        };
    }
    // SHA-1 or SHA-256 object id.
    let is_id = matches!(line.len(), 40 | 64) && line.bytes().all(|b| b.is_ascii_hexdigit());
    is_id.then(|| Head::Detached(line[..SHORT_ID].to_ascii_lowercase()))
}

/// The path in a `.git` file: `gitdir: <path>`.
fn parse_gitdir(text: &str) -> Option<PathBuf> {
    let path = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    (!path.is_empty()).then(|| PathBuf::from(path))
}

/// The linked worktree's name when `git_dir` is a worktree's admin dir
/// (`<common>/worktrees/<name>`, with a `commondir` file).
fn linked_worktree(git_dir: &Path) -> Option<String> {
    let name = git_dir.file_name()?.to_str()?;
    let under_worktrees = git_dir
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|parent| parent == "worktrees");
    (under_worktrees || git_dir.join("commondir").is_file()).then(|| name.to_string())
}

fn read_small(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut text = String::new();
    file.take(MAX_FILE_BYTES).read_to_string(&mut text).ok()?;
    Some(text)
}

/// Resolve `.` and `..` without touching the filesystem (`gitdir:` paths of
/// submodules are relative, e.g. `../.git/modules/lib`).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_urls_lose_their_credentials() {
        assert_eq!(
            without_credentials("https://x-access-token:ghp_123@github.com/o/r.git").as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert_eq!(
            without_credentials("git@github.com:o/r.git").as_deref(),
            Some("git@github.com:o/r.git")
        );
        assert_eq!(
            without_credentials("ssh://git@host:22/o/r").as_deref(),
            Some("ssh://host:22/o/r")
        );
        assert_eq!(without_credentials("bad\nurl"), None);
    }
    use std::fs;

    const SHA: &str = "1f2e3d4c5b6a79881f2e3d4c5b6a79881f2e3d4c";

    fn tempdir() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        (dir, root)
    }

    /// A main worktree at `root` with HEAD `head`.
    fn repo(root: &Path, head: &str) {
        fs::create_dir_all(root.join(".git/refs/heads")).unwrap();
        fs::write(root.join(".git/HEAD"), head).unwrap();
    }

    #[test]
    fn main_worktree_branch_from_any_subdirectory() {
        let (_dir, root) = tempdir();
        let repo_root = root.join("repo");
        repo(&repo_root, "ref: refs/heads/feature/login\n");
        fs::create_dir_all(repo_root.join("src/deep")).unwrap();
        let expected = GitInfo {
            repo_root: repo_root.clone(),
            branch: Some("feature/login".into()),
            detached_at: None,
            worktree: None,
            // No remote: the folder's name.
            repo: Some("repo".into()),
            origin: None,
        };
        assert_eq!(detect(&repo_root), Some(expected.clone()));
        assert_eq!(detect(&repo_root.join("src/deep")), Some(expected));
    }

    #[test]
    fn detached_head_gives_a_short_id() {
        let (_dir, root) = tempdir();
        repo(&root, &format!("{SHA}\n"));
        let info = detect(&root).unwrap();
        assert_eq!(info.branch, None);
        assert_eq!(info.detached_at.as_deref(), Some("1f2e3d4"));
    }

    #[test]
    fn linked_worktree_with_commondir() {
        let (_dir, root) = tempdir();
        let main = root.join("main");
        repo(&main, "ref: refs/heads/main\n");
        let admin = main.join(".git/worktrees/fix-login");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("HEAD"), "ref: refs/heads/fix-login\n").unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        let checkout = root.join("checkouts/fix");
        fs::create_dir_all(checkout.join("src")).unwrap();
        fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        assert_eq!(
            detect(&checkout.join("src")),
            Some(GitInfo {
                repo_root: checkout.clone(),
                branch: Some("fix-login".into()),
                detached_at: None,
                worktree: Some("fix-login".into()),
                // The main working tree's name, not the checkout's.
                repo: Some("main".into()),
                origin: None,
            })
        );
        // The main worktree is unaffected.
        assert_eq!(detect(&main).unwrap().worktree, None);
    }

    #[test]
    fn linked_worktree_with_a_relative_gitdir_and_detached_head() {
        let (_dir, root) = tempdir();
        repo(&root.join("main"), "ref: refs/heads/main\n");
        let admin = root.join("main/.git/worktrees/review");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("HEAD"), SHA).unwrap();
        fs::create_dir_all(root.join("review")).unwrap();
        fs::write(
            root.join("review/.git"),
            "gitdir: ../main/.git/worktrees/review",
        )
        .unwrap();
        let info = detect(&root.join("review")).unwrap();
        assert_eq!(info.worktree.as_deref(), Some("review"));
        assert_eq!(info.detached_at.as_deref(), Some("1f2e3d4"));
        assert_eq!(info.branch, None);
    }

    #[test]
    fn submodules_are_not_worktrees() {
        let (_dir, root) = tempdir();
        repo(&root, "ref: refs/heads/main\n");
        let modules = root.join(".git/modules/lib");
        fs::create_dir_all(&modules).unwrap();
        fs::write(modules.join("HEAD"), SHA).unwrap();
        fs::create_dir_all(root.join("vendor/lib")).unwrap();
        fs::write(
            root.join("vendor/lib/.git"),
            "gitdir: ../../.git/modules/lib\n",
        )
        .unwrap();
        assert_eq!(
            detect(&root.join("vendor/lib")),
            Some(GitInfo {
                repo_root: root.join("vendor/lib"),
                branch: None,
                detached_at: Some("1f2e3d4".into()),
                worktree: None,
                repo: Some("lib".into()),
                origin: None,
            })
        );
        // Outside the submodule, the superproject.
        assert_eq!(detect(&root.join("vendor")).unwrap().repo_root, root);
    }

    #[test]
    fn no_repository_or_unreadable_files() {
        let (_dir, root) = tempdir();
        fs::create_dir_all(root.join("plain")).unwrap();
        // Only meaningful when no ancestor of the temp dir is a repository.
        if root.ancestors().all(|a| !a.join(".git").exists()) {
            assert_eq!(detect(&root.join("plain")), None);
        }
        // A `.git` without HEAD, a `.git` file without `gitdir:`, a gitdir
        // that doesn't exist, garbage in HEAD.
        fs::create_dir_all(root.join("a/.git")).unwrap();
        assert_eq!(detect(&root.join("a")), None);
        fs::create_dir_all(root.join("b")).unwrap();
        fs::write(root.join("b/.git"), "not a gitdir line\n").unwrap();
        assert_eq!(detect(&root.join("b")), None);
        fs::create_dir_all(root.join("c")).unwrap();
        fs::write(
            root.join("c/.git"),
            "gitdir: /nonexistent/.git/worktrees/x\n",
        )
        .unwrap();
        assert_eq!(detect(&root.join("c")), None);
        repo(&root.join("d"), "something else\n");
        assert_eq!(detect(&root.join("d")), None);
    }

    #[test]
    fn head_parsing() {
        assert_eq!(
            parse_head("ref: refs/heads/main\n"),
            Some(Head::Branch("main".into()))
        );
        assert_eq!(
            parse_head("ref:refs/heads/a/b"),
            Some(Head::Branch("a/b".into()))
        );
        assert_eq!(
            parse_head("ref: refs/remotes/origin/main"),
            Some(Head::Branch("refs/remotes/origin/main".into()))
        );
        assert_eq!(
            parse_head("ref: refs/heads/.invalid\n"),
            Some(Head::Unknown)
        );
        assert_eq!(parse_head("ref: \n"), None);
        assert_eq!(parse_head(""), None);
        assert_eq!(
            parse_head(&SHA.to_uppercase()),
            Some(Head::Detached("1f2e3d4".into()))
        );
        assert_eq!(
            parse_head(&"ab".repeat(32)),
            Some(Head::Detached("abababa".into()))
        );
        assert_eq!(parse_head("1f2e3d4"), None);
        assert_eq!(parse_head(&"zz".repeat(20)), None);
    }

    /// `config` as the repository's `origin` at `url`.
    fn origin(git_dir: &Path, url: &str) {
        fs::write(
            git_dir.join("config"),
            format!(
                "[core]\n\trepositoryformatversion = 0\n\tbare = false\n\
                 [remote \"origin\"]\n\turl = {url}\n\
                 \tfetch = +refs/heads/*:refs/remotes/origin/*\n\
                 [branch \"main\"]\n\tremote = origin\n\tmerge = refs/heads/main\n"
            ),
        )
        .unwrap();
    }

    /// A linked worktree of the repository at `main` checked out at
    /// `checkout`, its admin dir `<main>/.git/worktrees/<name>`.
    fn linked(main: &Path, name: &str, checkout: &Path) {
        let admin = main.join(".git/worktrees").join(name);
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("HEAD"), format!("ref: refs/heads/{name}\n")).unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        fs::create_dir_all(checkout).unwrap();
        fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
    }

    #[test]
    fn the_repository_is_named_after_its_origin_else_its_main_folder() {
        let (_dir, root) = tempdir();
        // A main checkout whose folder isn't named like its remote.
        let main = root.join("repos/focal-md-main");
        repo(&main, "ref: refs/heads/main\n");
        origin(&main.join(".git"), "git@github.com:acme/focal-md.git");
        assert_eq!(detect(&main).unwrap().repo.as_deref(), Some("focal-md"));
        // Its linked worktrees read the common dir's config: one beside it,
        // one inside it, and one in an agent's data directory (OpenCode's).
        for checkout in [
            root.join("repos/focal-md-paper-links"),
            main.join(".claude/worktrees/paper"),
            root.join(".local/share/opencode/worktree/4f2a/brave-falcon"),
        ] {
            let name = checkout.file_name().unwrap().to_str().unwrap().to_string();
            linked(&main, &name, &checkout);
            let info = detect(&checkout).unwrap();
            assert_eq!(info.worktree.as_deref(), Some(name.as_str()));
            assert_eq!(info.repo.as_deref(), Some("focal-md"), "{checkout:?}");
        }
        // Without a remote, the main working tree's folder, from any of them.
        fs::write(main.join(".git/config"), "[core]\n\tbare = false\n").unwrap();
        assert_eq!(
            detect(&main).unwrap().repo.as_deref(),
            Some("focal-md-main")
        );
        assert_eq!(
            detect(&root.join("repos/focal-md-paper-links"))
                .unwrap()
                .repo
                .as_deref(),
            Some("focal-md-main")
        );
        // Another remote alone isn't the origin; no config at all reads as
        // empty.
        origin(&main.join(".git"), "https://github.com/acme/other.git");
        let config = fs::read_to_string(main.join(".git/config")).unwrap();
        fs::write(
            main.join(".git/config"),
            config.replace("[remote \"origin\"]", "[remote \"upstream\"]"),
        )
        .unwrap();
        assert_eq!(
            detect(&main).unwrap().repo.as_deref(),
            Some("focal-md-main")
        );
        fs::remove_file(main.join(".git/config")).unwrap();
        assert_eq!(
            detect(&main).unwrap().repo.as_deref(),
            Some("focal-md-main")
        );
    }

    #[test]
    fn bare_repositories_name_their_worktrees_after_themselves() {
        let (_dir, root) = tempdir();
        // `project.git` with a worktree elsewhere.
        let bare = root.join("srv/project.git");
        fs::create_dir_all(bare.join("worktrees/feature")).unwrap();
        fs::write(bare.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(bare.join("config"), "[core]\n\tbare = true\n").unwrap();
        let admin = bare.join("worktrees/feature");
        fs::write(admin.join("HEAD"), "ref: refs/heads/feature\n").unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        let checkout = root.join("work/feature");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        assert_eq!(detect(&checkout).unwrap().repo.as_deref(), Some("project"));
        // `<project>/.bare` beside its worktrees, the admin dir without a
        // `commondir`: the folder holding it.
        let project = root.join("tools");
        let admin = project.join(".bare/worktrees/main");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::create_dir_all(project.join("main")).unwrap();
        fs::write(
            project.join("main/.git"),
            "gitdir: ../.bare/worktrees/main\n",
        )
        .unwrap();
        let info = detect(&project.join("main")).unwrap();
        assert_eq!(info.worktree.as_deref(), Some("main"));
        assert_eq!(info.repo.as_deref(), Some("tools"));
    }

    #[test]
    fn a_submodule_is_a_repository_of_its_own() {
        let (_dir, root) = tempdir();
        let superproject = root.join("app");
        repo(&superproject, "ref: refs/heads/main\n");
        origin(
            &superproject.join(".git"),
            "https://github.com/acme/app.git",
        );
        let modules = superproject.join(".git/modules/vendor/lib");
        fs::create_dir_all(&modules).unwrap();
        fs::write(modules.join("HEAD"), SHA).unwrap();
        let lib = superproject.join("vendor/lib");
        fs::create_dir_all(&lib).unwrap();
        fs::write(lib.join(".git"), "gitdir: ../../.git/modules/vendor/lib\n").unwrap();
        // Without a remote, its own folder; with one, its remote's name.
        assert_eq!(detect(&lib).unwrap().repo.as_deref(), Some("lib"));
        origin(&modules, "https://github.com/acme/libfoo");
        assert_eq!(detect(&lib).unwrap().repo.as_deref(), Some("libfoo"));
        assert_eq!(detect(&superproject).unwrap().repo.as_deref(), Some("app"));
    }

    #[test]
    fn unreadable_or_odd_files_leave_the_name_out() {
        let (_dir, root) = tempdir();
        let main = root.join("main");
        repo(&main, "ref: refs/heads/main\n");
        // A config that isn't text.
        fs::write(main.join(".git/config"), b"[core]\0\x01\x02").unwrap();
        let info = detect(&main).unwrap();
        assert_eq!(info.branch.as_deref(), Some("main"));
        assert_eq!(info.repo, None);
        // A config that can't be read (a directory).
        fs::remove_file(main.join(".git/config")).unwrap();
        fs::create_dir_all(main.join(".git/config")).unwrap();
        assert_eq!(detect(&main).unwrap().repo, None);
        // A worktree whose `commondir` leads nowhere.
        let admin = main.join(".git/worktrees/lost");
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("HEAD"), "ref: refs/heads/lost\n").unwrap();
        fs::write(admin.join("commondir"), "/nonexistent/common\n").unwrap();
        fs::create_dir_all(root.join("lost")).unwrap();
        fs::write(
            root.join("lost/.git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        let info = detect(&root.join("lost")).unwrap();
        assert_eq!(info.branch.as_deref(), Some("lost"));
        assert_eq!(info.repo, None);
        // A garbled config without an origin: the folder's name; an origin
        // without a path, too.
        fs::remove_dir(main.join(".git/config")).unwrap();
        fs::write(
            main.join(".git/config"),
            "[remote \"origin\"\nurl = x\n=\n[[\n",
        )
        .unwrap();
        assert_eq!(detect(&main).unwrap().repo.as_deref(), Some("main"));
        origin(&main.join(".git"), "https://github.com");
        assert_eq!(detect(&main).unwrap().repo.as_deref(), Some("main"));
        // A huge config is read as far as its first 64 KiB.
        let mut big = String::from("[remote \"origin\"]\n\turl = https://h/o/huge.git\n");
        while big.len() < 2 * MAX_CONFIG_BYTES as usize {
            big.push_str("\t# padding padding padding padding padding\n");
        }
        fs::write(main.join(".git/config"), big).unwrap();
        assert_eq!(detect(&main).unwrap().repo.as_deref(), Some("huge"));
    }

    #[test]
    fn names_from_remote_urls() {
        for (url, name) in [
            ("git@github.com:org/rakumux.git", Some("rakumux")),
            ("git@github.com:rakumux.git", Some("rakumux")),
            ("github.com:org/rakumux", Some("rakumux")),
            ("https://github.com/org/focal-md", Some("focal-md")),
            ("https://github.com/org/focal-md/", Some("focal-md")),
            ("https://github.com/org/focal-md.git/", Some("focal-md")),
            (
                "https://user@gitlab.com/group/sub/deep-repo.git",
                Some("deep-repo"),
            ),
            (
                "https://dev.azure.com/org/project/_git/web-app",
                Some("web-app"),
            ),
            ("ssh://git@host:2222/org/repo.git", Some("repo")),
            ("ssh://host/~user/repo", Some("repo")),
            ("git://host.xz/path/to/repo.git", Some("repo")),
            ("file:///srv/git/shared.git", Some("shared")),
            ("/srv/git/shared.git", Some("shared")),
            ("/srv/git/shared/.git", Some("shared")),
            ("/srv/git/shared.git//", Some("shared")),
            ("../sibling", Some("sibling")),
            ("https://github.com", None),
            ("https://github.com/", None),
            ("", None),
            (".git", None),
        ] {
            assert_eq!(name_of_url(url).as_deref(), name, "{url}");
        }
    }

    #[test]
    fn origin_urls_from_config_text() {
        let url = |text: &str| origin_url(text);
        assert_eq!(
            url("[remote \"origin\"]\n\turl = git@h:o/r.git\n").as_deref(),
            Some("git@h:o/r.git")
        );
        // Section and key names ignore case; the remote's name doesn't.
        assert_eq!(
            url("[Remote \"origin\"]\nURL=a/b\n").as_deref(),
            Some("a/b")
        );
        assert_eq!(url("[remote \"Origin\"]\nurl = a/b\n"), None);
        // The old form, a key on the header's line, the first of several.
        assert_eq!(url("[remote.origin]\nurl = a/b\n").as_deref(), Some("a/b"));
        assert_eq!(
            url("[remote \"origin\"] url = a/first\nurl = a/second\n").as_deref(),
            Some("a/first")
        );
        // Another section ends it; other remotes and keys aren't it.
        assert_eq!(
            url("[remote \"origin\"]\n\tfetch = x\n[remote \"fork\"]\n\turl = a/fork\n"),
            None
        );
        assert_eq!(url("[remote \"origin\"]\n\tpushurl = a/push\n"), None);
        // Quotes, escapes and comments, as git reads them.
        assert_eq!(
            url("[remote \"origin\"]\n\turl = \"/srv/my repo;x\" ; a comment\n").as_deref(),
            Some("/srv/my repo;x")
        );
        assert_eq!(
            url("[remote \"origin\"]\n\turl = a/b # a comment\n").as_deref(),
            Some("a/b")
        );
        assert_eq!(
            url("[remote \"origin\"]\n\turl = a/\\\"q\\\"\n").as_deref(),
            Some("a/\"q\"")
        );
        // Commented out, empty, or continued on the next line: none.
        assert_eq!(url("[remote \"origin\"]\n#\turl = a/b\n;url = a/b\n"), None);
        assert_eq!(url("[remote \"origin\"]\n\turl =\n"), None);
        assert_eq!(url("[remote \"origin\"]\n\turl = a/\\\nb\n"), None);
    }

    /// Branch and worktree names are file contents and folder names: one
    /// that isn't readable (an escape sequence, a line break) is left out,
    /// as the repository's name is.
    #[test]
    fn unreadable_branch_and_worktree_names_are_left_out() {
        let (_dir, root) = tempdir();
        let main = root.join("main");
        repo(&main, "ref: refs/heads/main\u{1b}]0;PWNED\u{7}\n");
        let info = detect(&main).unwrap();
        assert_eq!((info.branch, info.detached_at), (None, None));
        assert_eq!(info.repo.as_deref(), Some("main"));

        let name = "fix\u{1b}]52;c;SGk=\u{7}";
        let admin = main.join(".git/worktrees").join(name);
        fs::create_dir_all(&admin).unwrap();
        fs::write(admin.join("HEAD"), "ref: refs/heads/fix\u{9b}31m\n").unwrap();
        fs::write(admin.join("commondir"), "../..\n").unwrap();
        let checkout = root.join("fix");
        fs::create_dir_all(&checkout).unwrap();
        fs::write(
            checkout.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        let info = detect(&checkout).unwrap();
        assert_eq!((info.branch, info.worktree), (None, None));
        // Still a linked worktree: named after the main working tree.
        assert_eq!(info.repo.as_deref(), Some("main"));
    }

    #[test]
    fn names_are_shown_only_when_readable() {
        assert!(displayable("focal-md"));
        assert!(displayable("日本語"));
        assert!(!displayable(""));
        assert!(!displayable("  "));
        assert!(!displayable("a\u{1b}[2Jb"));
        assert!(!displayable("bad\u{fffd}"));
        assert!(displayable(&"x".repeat(MAX_NAME_CHARS)));
        assert!(!displayable(&"x".repeat(MAX_NAME_CHARS + 1)));
    }

    #[test]
    fn gitdir_paths_are_normalized_lexically() {
        assert_eq!(
            normalize(Path::new("/r/vendor/lib/../../.git/./modules/lib")),
            PathBuf::from("/r/.git/modules/lib")
        );
        assert_eq!(
            parse_gitdir("gitdir:  /x/y \n"),
            Some(PathBuf::from("/x/y"))
        );
        assert_eq!(parse_gitdir("gitdir:\n"), None);
    }
}
