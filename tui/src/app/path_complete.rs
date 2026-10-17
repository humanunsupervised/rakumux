//! Folder path completion: the folders that complete a typed path, for the
//! Folders modal. `~` means the home folder and a relative path starts at
//! a base folder. Only folders are offered (symbolic links to folders too);
//! dot-folders only once the typed name starts with `.`. Reading is capped,
//! so a huge folder never stalls a keystroke.

use std::path::{Path, PathBuf};

/// Most folders offered at once.
pub const MAX_SUGGESTIONS: usize = 8;
/// Most entries read from one folder.
const MAX_ENTRIES: usize = 4096;
/// Most symbolic links followed to see whether they point at a folder.
const MAX_LINKS: usize = 64;

/// The home folder (`$HOME`), when set.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
}

/// `input` as a path: `~` and `~/…` start at `home`, relative paths at
/// `base`.
pub fn expand(input: &str, base: &Path, home: Option<&Path>) -> PathBuf {
    if let Some(home) = home {
        if input == "~" {
            return home.to_path_buf();
        }
        if let Some(rest) = input.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    let path = Path::new(input);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// A folder that completes the typed path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirSuggestion {
    /// The folder's own name.
    pub name: String,
    /// The typed text completed with it and ending in `/`: what Tab puts
    /// in.
    pub value: String,
    /// Where it is: its parent folder (canonical when it can be) joined
    /// with its name.
    pub path: PathBuf,
}

/// The folders that complete a typed path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Suggestions {
    /// The first matching folders, by name.
    pub folders: Vec<DirSuggestion>,
    /// Matching folders left out of `folders`.
    pub more: usize,
    /// The typed text completed as far as every match agrees (the whole
    /// folder and a `/` when only one matches); `None` when that adds
    /// nothing.
    pub completion: Option<String>,
}

/// The typed text split into its folder part (up to and including the last
/// `/`) and the name being typed. `~` alone is read as `~/`.
fn split(input: &str) -> (String, &str) {
    if input == "~" {
        return ("~/".into(), "");
    }
    match input.rfind('/') {
        Some(i) => (input[..=i].to_string(), &input[i + 1..]),
        None => (String::new(), input),
    }
}

/// The folders inside the typed path's folder whose names start with its
/// last part: `~/co` offers `~/code/`. Names match case-sensitively; when
/// none does, case is ignored. At most `limit` are returned, sorted by name;
/// `more` counts the rest.
pub fn suggest(input: &str, base: &Path, home: Option<&Path>, limit: usize) -> Suggestions {
    let (dir_part, typed) = split(input);
    let raw = if dir_part.is_empty() {
        base.to_path_buf()
    } else {
        expand(&dir_part, base, home)
    };
    let Ok(entries) = std::fs::read_dir(&raw) else {
        return Suggestions::default();
    };
    let parent = std::fs::canonicalize(&raw).unwrap_or(raw);
    let show_hidden = typed.starts_with('.');
    let typed_lower = typed.to_lowercase();
    // Names matching with their case, and those matching only without.
    let mut exact: Vec<String> = Vec::new();
    let mut folded: Vec<String> = Vec::new();
    let mut links = 0;
    for entry in entries.take(MAX_ENTRIES).filter_map(Result::ok) {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        if name.starts_with('.') && !show_hidden {
            continue;
        }
        let matches_case = name.starts_with(typed);
        if !matches_case && !name.to_lowercase().starts_with(&typed_lower) {
            continue;
        }
        // The entry's type needs no extra system call on most systems; a
        // link is followed, a limited number of times.
        let is_dir = match entry.file_type() {
            Ok(kind) if kind.is_dir() => true,
            Ok(kind) if kind.is_symlink() && links < MAX_LINKS => {
                links += 1;
                std::fs::metadata(entry.path()).is_ok_and(|m| m.is_dir())
            }
            _ => false,
        };
        if !is_dir {
            continue;
        }
        if matches_case {
            exact.push(name);
        } else {
            folded.push(name);
        }
    }
    let mut names = if exact.is_empty() { folded } else { exact };
    names.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()).then(a.cmp(b)));
    let completion = match names.as_slice() {
        [] => None,
        [only] => Some(format!("{dir_part}{only}/")),
        [first, rest @ ..] => {
            let common = rest.iter().fold(first.clone(), |common, name| {
                common
                    .chars()
                    .zip(name.chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a)
                    .collect()
            });
            (common.chars().count() > typed.chars().count()).then(|| format!("{dir_part}{common}"))
        }
    };
    let more = names.len().saturating_sub(limit);
    let folders = names
        .into_iter()
        .take(limit)
        .map(|name| DirSuggestion {
            value: format!("{dir_part}{name}/"),
            path: parent.join(&name),
            name,
        })
        .collect();
    Suggestions {
        folders,
        more,
        completion,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `root/{alpha, alphabet, beta, Docs, .hidden}` folders, a file
    /// `alfile` and a link `alink` to `beta`.
    fn tree() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        for name in ["alpha", "alphabet", "beta", "Docs", ".hidden"] {
            std::fs::create_dir(root.join(name)).unwrap();
        }
        std::fs::write(root.join("alfile"), "x").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("beta"), root.join("alink")).unwrap();
        (dir, root)
    }

    fn names(suggestions: &Suggestions) -> Vec<&str> {
        suggestions
            .folders
            .iter()
            .map(|s| s.name.as_str())
            .collect()
    }

    #[test]
    fn folders_starting_with_the_typed_name_are_offered_in_order() {
        let (_dir, root) = tree();
        let typed = format!("{}/al", root.display());
        let found = suggest(&typed, Path::new("/"), None, MAX_SUGGESTIONS);
        // Folders and links to folders; not the file.
        assert_eq!(names(&found), ["alink", "alpha", "alphabet"]);
        assert_eq!(found.folders[1].value, format!("{}/alpha/", root.display()));
        assert_eq!(found.folders[1].path, root.join("alpha"));
        assert_eq!(found.more, 0);
        // Every match starts with "al" only: nothing to add.
        assert_eq!(found.completion, None);
        // A folder that ends the typed text lists everything in it.
        let found = suggest(&format!("{}/", root.display()), root.as_path(), None, 10);
        assert_eq!(
            names(&found),
            ["alink", "alpha", "alphabet", "beta", "Docs"]
        );
    }

    #[test]
    fn the_completion_goes_as_far_as_the_matches_agree() {
        let (_dir, root) = tree();
        let found = suggest("./alp", &root, None, MAX_SUGGESTIONS);
        assert_eq!(names(&found), ["alpha", "alphabet"]);
        assert_eq!(found.folders[0].value, "./alpha/");
        assert_eq!(found.completion.as_deref(), Some("./alpha"));
        // One match: the whole folder and a `/`, so its insides come next.
        let found = suggest("./alphab", &root, None, MAX_SUGGESTIONS);
        assert_eq!(found.completion.as_deref(), Some("./alphabet/"));
        // Relative paths start at the base folder.
        let found = suggest("be", &root, None, MAX_SUGGESTIONS);
        assert_eq!(names(&found), ["beta"]);
        assert_eq!(found.completion.as_deref(), Some("beta/"));
        assert!(suggest("./nothing", &root, None, 8).folders.is_empty());
        assert!(suggest("./missing/a", &root, None, 8).folders.is_empty());
    }

    #[test]
    fn typed_folders_complete_to_sub_folders() {
        let dir = tempfile::tempdir().unwrap();
        for d in ["server", "service", "docs", ".hidden", "docs/guide"] {
            std::fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        std::fs::write(dir.path().join("serve.txt"), "x").unwrap();
        let base = dir.path();
        let completion = |input: &str, base: &Path| suggest(input, base, None, 8).completion;
        assert_eq!(completion("d", base).as_deref(), Some("docs/"));
        assert_eq!(completion("docs/", base).as_deref(), Some("docs/guide/"));
        // Ambiguous: the common start, and the choices.
        let serv = suggest("se", base, None, 8);
        assert_eq!(serv.completion.as_deref(), Some("serv"));
        assert_eq!(names(&serv), ["server", "service"]);
        assert_eq!(suggest("zzz", base, None, 8), Suggestions::default());
        assert_eq!(completion(".h", base).as_deref(), Some(".hidden/"));
        let absolute = format!("{}/ser", base.display());
        assert_eq!(
            completion(&absolute, Path::new("/elsewhere")).as_deref(),
            Some(format!("{}/serv", base.display()).as_str())
        );
    }

    #[test]
    fn dot_folders_show_only_when_the_name_starts_with_a_dot() {
        let (_dir, root) = tree();
        assert!(!names(&suggest("./", &root, None, 10)).contains(&".hidden"));
        assert_eq!(names(&suggest("./.", &root, None, 10)), [".hidden"]);
        assert_eq!(names(&suggest(".h", &root, None, 10)), [".hidden"]);
    }

    #[test]
    fn case_is_ignored_only_when_nothing_matches_with_it() {
        let (_dir, root) = tree();
        let found = suggest("./doc", &root, None, MAX_SUGGESTIONS);
        assert_eq!(names(&found), ["Docs"]);
        assert_eq!(found.completion.as_deref(), Some("./Docs/"));
        std::fs::create_dir(root.join("documents")).unwrap();
        assert_eq!(names(&suggest("./doc", &root, None, 8)), ["documents"]);
    }

    #[test]
    fn tilde_means_the_home_folder() {
        let (_dir, home) = tree();
        let found = suggest("~/b", Path::new("/"), Some(&home), MAX_SUGGESTIONS);
        assert_eq!(names(&found), ["beta"]);
        assert_eq!(found.folders[0].value, "~/beta/");
        assert_eq!(found.folders[0].path, home.join("beta"));
        // `~` alone lists the home folder.
        let found = suggest("~", Path::new("/"), Some(&home), 10);
        assert_eq!(found.folders[0].value, "~/alink/");
        assert_eq!(found.folders.len(), 5);
        assert_eq!(expand("~", Path::new("/x"), Some(&home)), home);
        assert_eq!(
            expand("~/a/b", Path::new("/x"), Some(&home)),
            home.join("a/b")
        );
        assert_eq!(expand("a", Path::new("/x"), Some(&home)), Path::new("/x/a"));
        assert_eq!(expand("/a", Path::new("/x"), Some(&home)), Path::new("/a"));
        // Without a home folder, `~` is just a name.
        assert_eq!(expand("~/a", Path::new("/x"), None), Path::new("/x/~/a"));
    }

    #[test]
    fn the_list_is_capped_and_says_how_many_more_match() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..20 {
            std::fs::create_dir(dir.path().join(format!("dir{i:02}"))).unwrap();
        }
        let found = suggest("./d", dir.path(), None, MAX_SUGGESTIONS);
        assert_eq!(found.folders.len(), MAX_SUGGESTIONS);
        assert_eq!(found.folders[0].name, "dir00");
        assert_eq!(found.more, 20 - MAX_SUGGESTIONS);
        // The completion agrees with every match, not just the shown ones.
        assert_eq!(found.completion.as_deref(), Some("./dir"));
    }
}
