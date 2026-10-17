//! Folders: listing them, and turning a `folder` argument (a directory or
//! the name of a known folder) into the daemon's canonical scope. The
//! daemon's protocol calls folders projects (`project_list`, `ProjectView`)
//! and scopes; the API says folder and still accepts `project` as an alias.

use crate::dto::Folder;
use crate::error::HttpError;
use serde_json::json;
use session_core::api::ErrorCode;
use session_core::client::SessionApi;
use session_core::model::ScopeRoot;
use std::io;
use std::path::{Path, PathBuf};

/// The folders the caller can see (`unsupported` until the daemon has the
/// `projects` capability).
pub fn list(api: &dyn SessionApi) -> Result<Vec<Folder>, HttpError> {
    Ok(api.projects()?.into_iter().map(Folder::from).collect())
}

/// The folder a request names with `folder` and/or its deprecated alias
/// `project`: `folder` wins; both given and different is a 400.
pub fn pick(folder: Option<String>, project: Option<String>) -> Result<Option<String>, HttpError> {
    match (folder, project) {
        (Some(folder), Some(project)) if folder.trim() != project.trim() => Err(both_differ()),
        (Some(folder), _) => Ok(Some(folder)),
        (None, project) => Ok(project),
    }
}

/// Like [`pick`] for repeatable query parameters: the two lists must name
/// the same folders (in any order) when both are given.
pub fn pick_all(folders: Vec<String>, projects: Vec<String>) -> Result<Vec<String>, HttpError> {
    if folders.is_empty() {
        return Ok(projects);
    }
    if !projects.is_empty() {
        let set = |values: &[String]| -> std::collections::BTreeSet<String> {
            values.iter().map(|v| v.trim().to_owned()).collect()
        };
        if set(&folders) != set(&projects) {
            return Err(both_differ());
        }
    }
    Ok(folders)
}

fn both_differ() -> HttpError {
    HttpError::invalid(
        "`folder` and `project` name different folders; pass only `folder` (`project` is its \
         deprecated alias)",
    )
}

/// Resolve a `folder` argument: an absolute directory (`~/` allowed) is
/// canonicalized; anything without a `/` is looked up by folder name.
pub fn resolve(api: &dyn SessionApi, input: &str) -> Result<ScopeRoot, HttpError> {
    let input = input.trim();
    if input.is_empty() {
        return Err(HttpError::invalid("folder must not be empty"));
    }
    let path = expand_home(input);
    if path.is_absolute() {
        return resolve_directory(&path);
    }
    if input.contains('/') || input == "." || input == ".." {
        return Err(HttpError::invalid(format!(
            "folder {input:?} must be an absolute directory or the name of a known folder"
        )));
    }
    let folders = match api.projects() {
        Ok(folders) => folders,
        Err(error) if error.code == ErrorCode::Unsupported => {
            return Err(HttpError::invalid(format!(
                "folder {input:?} is not an absolute directory, and this Rakumux daemon \
                 can't look folders up by name yet; pass the folder's directory"
            )))
        }
        Err(error) => return Err(error.into()),
    };
    let mut matches: Vec<ScopeRoot> = folders
        .into_iter()
        .filter(|folder| folder.name == input)
        .map(|folder| folder.path)
        .collect();
    match matches.len() {
        0 => Err(HttpError::not_found(format!("no folder named {input:?}"))),
        1 => Ok(matches.remove(0)),
        _ => {
            let candidates: Vec<_> = matches.iter().map(|scope| scope.path()).collect();
            Err(HttpError::invalid(format!(
                "{} folders are named {input:?}; pass the directory instead",
                matches.len()
            ))
            .with_details(json!({ "candidates": candidates })))
        }
    }
}

fn resolve_directory(path: &Path) -> Result<ScopeRoot, HttpError> {
    let scope = ScopeRoot::resolve(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => {
            HttpError::not_found(format!("folder {} does not exist", path.display()))
        }
        _ => HttpError::invalid(format!("folder {}: {error}", path.display())),
    })?;
    if !scope.path().is_dir() {
        return Err(HttpError::invalid(format!(
            "folder {} is not a directory",
            path.display()
        )));
    }
    Ok(scope)
}

/// `~` and `~/…` relative to `$HOME`; anything else unchanged.
fn expand_home(input: &str) -> PathBuf {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    if input == "~" {
        if let Some(home) = home() {
            return home;
        }
    }
    if let Some(rest) = input.strip_prefix("~/") {
        if let Some(home) = home() {
            return home.join(rest);
        }
    }
    PathBuf::from(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directories_resolve_to_canonical_paths() {
        let dir = tempfile::tempdir().unwrap();
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let nested = canonical.join("a");
        std::fs::create_dir(&nested).unwrap();
        let input = format!("{}/a/../a", canonical.display());
        assert_eq!(resolve_directory(Path::new(&input)).unwrap().path(), nested);
        let missing = resolve_directory(&canonical.join("missing")).unwrap_err();
        assert_eq!(missing.status.as_u16(), 404);
        assert!(missing.body.message.starts_with("folder "));
        let file = canonical.join("f");
        std::fs::write(&file, "").unwrap();
        assert_eq!(resolve_directory(&file).unwrap_err().status.as_u16(), 400);
    }

    #[test]
    fn home_is_expanded() {
        if let Some(home) = std::env::var_os("HOME") {
            assert_eq!(expand_home("~/x"), PathBuf::from(home).join("x"));
        }
        assert_eq!(expand_home("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_home("name"), PathBuf::from("name"));
    }

    #[test]
    fn folder_wins_over_its_project_alias_unless_they_differ() {
        let some = |s: &str| Some(s.to_owned());
        assert_eq!(pick(None, None).unwrap(), None);
        assert_eq!(pick(some("/a"), None).unwrap(), some("/a"));
        assert_eq!(pick(None, some("/a")).unwrap(), some("/a"));
        assert_eq!(pick(some("/a"), some(" /a ")).unwrap(), some("/a"));
        let error = pick(some("/a"), some("/b")).unwrap_err();
        assert_eq!(error.status.as_u16(), 400);
        assert_eq!(error.code(), "invalid_argument");
        assert!(error.body.message.contains("`folder` and `project`"));

        let list = |values: &[&str]| values.iter().map(|v| v.to_string()).collect::<Vec<_>>();
        assert_eq!(pick_all(list(&[]), list(&[])).unwrap(), list(&[]));
        assert_eq!(pick_all(list(&["/a"]), list(&[])).unwrap(), list(&["/a"]));
        assert_eq!(pick_all(list(&[]), list(&["/b"])).unwrap(), list(&["/b"]));
        assert_eq!(
            pick_all(list(&["/a", "/b"]), list(&["/b", "/a"])).unwrap(),
            list(&["/a", "/b"])
        );
        let error = pick_all(list(&["/a"]), list(&["/a", "/b"])).unwrap_err();
        assert_eq!(error.status.as_u16(), 400);
    }
}
