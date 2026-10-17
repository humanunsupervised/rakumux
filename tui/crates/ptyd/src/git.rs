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

use session_core::model::GitInfo;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Largest `.git` file or `HEAD` read; git's own are a line long.
const MAX_FILE_BYTES: u64 = 4096;
/// Length of `GitInfo::detached_at`.
const SHORT_ID: usize = 7;
/// Branch name git writes into `HEAD` of reftable repositories, whose
/// real HEAD lives in the reftable.
const REFTABLE_PLACEHOLDER: &str = ".invalid";

/// Git context of `dir` (absolute). `None` outside a repository, or when
/// the nearest `.git` or its `HEAD` can't be read or parsed.
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
    Some(GitInfo {
        repo_root: root.to_path_buf(),
        branch,
        detached_at,
        worktree,
    })
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
