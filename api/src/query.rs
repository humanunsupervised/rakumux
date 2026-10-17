//! Query-string parameters of the routes. Unknown parameters are rejected so
//! a typo never silently widens a query.

use crate::dto::{ContentFormat, ContentSource};
use crate::error::HttpError;
use crate::folders;
use session_core::api::Role;

/// Most lines a content read may ask for.
pub const MAX_LINES: u32 = 10_000;
/// Lines a scrollback read returns when `lines` is omitted.
pub const DEFAULT_SCROLLBACK_LINES: u32 = 200;

/// `GET /v1/sessions` parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ListQuery {
    /// `folder` (repeatable; `project` is its deprecated alias):
    /// directories or folder names.
    pub folders: Vec<String>,
    /// Exact session name.
    pub name: Option<String>,
    /// Case-insensitive substring of the session name.
    pub q: Option<String>,
    /// Include sessions whose terminal is gone (status `dead`).
    pub include_dead: bool,
    /// Every folder's sessions (user callers; not with `folder`).
    pub all: bool,
}

/// `GET /v1/sessions/{id}/attach` parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttachQuery {
    /// `observer` (the default) or `controller`.
    pub role: Role,
    /// The client's terminal size; required for controllers, 0 when an
    /// observer leaves it out.
    pub cols: u16,
    pub rows: u16,
}

/// Largest terminal side a client may ask for.
pub const MAX_TERMINAL_SIDE: u16 = 1000;

/// `GET /v1/sessions/{id}/content` parameters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContentQuery {
    pub source: ContentSource,
    /// Last N lines; `None` = the whole screen, or the scrollback default.
    pub lines: Option<u32>,
    pub format: ContentFormat,
}

impl ContentQuery {
    /// Lines to request from the daemon.
    pub fn effective_lines(&self) -> Option<u32> {
        match (self.lines, self.source) {
            (Some(lines), _) => Some(lines),
            (None, ContentSource::Scrollback) => Some(DEFAULT_SCROLLBACK_LINES),
            (None, ContentSource::Screen) => None,
        }
    }
}

pub fn parse_list(query: Option<&str>) -> Result<ListQuery, HttpError> {
    let mut parsed = ListQuery::default();
    let mut projects = Vec::new();
    for (key, value) in pairs(query) {
        match key.as_str() {
            "folder" | "project" => {
                if value.trim().is_empty() {
                    return Err(HttpError::invalid(format!("{key} must not be empty")));
                }
                match key.as_str() {
                    "folder" => parsed.folders.push(value),
                    _ => projects.push(value),
                }
            }
            "name" => set_once(&mut parsed.name, "name", value)?,
            "q" => set_once(&mut parsed.q, "q", value)?,
            "include_dead" => parsed.include_dead = parse_bool("include_dead", &value)?,
            "all" => parsed.all = parse_bool("all", &value)?,
            other => return Err(unknown(other, "folder, name, q, include_dead, all")),
        }
    }
    parsed.folders = folders::pick_all(std::mem::take(&mut parsed.folders), projects)?;
    if parsed.all && !parsed.folders.is_empty() {
        return Err(HttpError::invalid(
            "all=true lists every folder; leave out `folder`",
        ));
    }
    Ok(parsed)
}

pub fn parse_attach(query: Option<&str>) -> Result<AttachQuery, HttpError> {
    let mut role = None;
    let mut cols = None;
    let mut rows = None;
    for (key, value) in pairs(query) {
        match key.as_str() {
            "role" => {
                let parsed = match value.as_str() {
                    "observer" => Role::Observer,
                    "controller" => Role::Controller,
                    _ => {
                        return Err(HttpError::invalid(
                            "role must be `observer` or `controller`",
                        ))
                    }
                };
                if role.replace(parsed).is_some() {
                    return Err(HttpError::invalid("role may be given only once"));
                }
            }
            "cols" | "rows" => {
                let side = value
                    .parse::<u16>()
                    .ok()
                    .filter(|side| (1..=MAX_TERMINAL_SIDE).contains(side))
                    .ok_or_else(|| {
                        HttpError::invalid(format!(
                            "{key} must be an integer from 1 to {MAX_TERMINAL_SIDE}"
                        ))
                    })?;
                let slot = if key == "cols" { &mut cols } else { &mut rows };
                if slot.replace(side).is_some() {
                    return Err(HttpError::invalid(format!("{key} may be given only once")));
                }
            }
            other => return Err(unknown(other, "role, cols, rows")),
        }
    }
    let role = role.unwrap_or(Role::Observer);
    if role == Role::Controller && (cols.is_none() || rows.is_none()) {
        return Err(HttpError::invalid(
            "a controller sets the terminal size: pass cols and rows",
        ));
    }
    Ok(AttachQuery {
        role,
        cols: cols.unwrap_or(0),
        rows: rows.unwrap_or(0),
    })
}

pub fn parse_content(query: Option<&str>) -> Result<ContentQuery, HttpError> {
    let mut parsed = ContentQuery::default();
    for (key, value) in pairs(query) {
        match key.as_str() {
            "source" => {
                parsed.source = match value.as_str() {
                    "screen" => ContentSource::Screen,
                    "scrollback" => ContentSource::Scrollback,
                    _ => {
                        return Err(HttpError::invalid(
                            "source must be `screen` or `scrollback`",
                        ))
                    }
                }
            }
            "format" => {
                parsed.format = match value.as_str() {
                    "text" => ContentFormat::Text,
                    "ansi" => ContentFormat::Ansi,
                    _ => return Err(HttpError::invalid("format must be `text` or `ansi`")),
                }
            }
            "lines" => {
                let lines = value
                    .parse::<u32>()
                    .ok()
                    .filter(|lines| (1..=MAX_LINES).contains(lines))
                    .ok_or_else(|| {
                        HttpError::invalid(format!(
                            "lines must be an integer from 1 to {MAX_LINES}"
                        ))
                    })?;
                parsed.lines = Some(lines);
            }
            other => return Err(unknown(other, "source, lines, format")),
        }
    }
    Ok(parsed)
}

/// Reject any query parameter (routes that take none).
pub fn parse_none(query: Option<&str>) -> Result<(), HttpError> {
    match pairs(query).into_iter().next() {
        Some((key, _)) => Err(unknown(&key, "none")),
        None => Ok(()),
    }
}

fn pairs(query: Option<&str>) -> Vec<(String, String)> {
    form_urlencoded::parse(query.unwrap_or("").as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn set_once(slot: &mut Option<String>, key: &str, value: String) -> Result<(), HttpError> {
    if slot.is_some() {
        return Err(HttpError::invalid(format!("{key} may be given only once")));
    }
    *slot = Some(value);
    Ok(())
}

fn parse_bool(key: &str, value: &str) -> Result<bool, HttpError> {
    match value {
        "true" | "1" | "" => Ok(true),
        "false" | "0" => Ok(false),
        _ => Err(HttpError::invalid(format!("{key} must be true or false"))),
    }
}

fn unknown(key: &str, known: &str) -> HttpError {
    HttpError::invalid(format!("unknown query parameter `{key}` (known: {known})"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_parameters_parse_and_repeat() {
        let query = parse_list(Some(
            "folder=%2Frepo%2Fa&folder=/repo/b&name=api%20work&q=API&include_dead=true",
        ))
        .unwrap();
        assert_eq!(query.folders, vec!["/repo/a", "/repo/b"]);
        assert_eq!(query.name.as_deref(), Some("api work"));
        assert_eq!(query.q.as_deref(), Some("API"));
        assert!(query.include_dead);
        assert_eq!(parse_list(None).unwrap(), ListQuery::default());
    }

    #[test]
    fn project_is_an_alias_of_folder() {
        let alias = parse_list(Some("project=/repo/a&project=/repo/b")).unwrap();
        assert_eq!(alias.folders, vec!["/repo/a", "/repo/b"]);
        // Both, naming the same folders: fine, `folder` wins.
        let both = parse_list(Some(
            "folder=/repo/b&folder=/repo/a&project=/repo/a&project=/repo/b",
        ))
        .unwrap();
        assert_eq!(both.folders, vec!["/repo/b", "/repo/a"]);
        // Both, naming different folders: refused.
        let error = parse_list(Some("folder=/repo/a&project=/repo/b")).unwrap_err();
        assert_eq!(error.status.as_u16(), 400);
        assert!(error.body.message.contains("`folder` and `project`"));
        let error = parse_list(Some("nme=x")).unwrap_err();
        assert!(error.body.message.contains("known: folder, name"));
    }

    #[test]
    fn bad_list_parameters_are_rejected() {
        for bad in [
            "nme=x",
            "include_dead=maybe",
            "name=a&name=b",
            "folder=",
            "project=",
        ] {
            let error = parse_list(Some(bad)).unwrap_err();
            assert_eq!(error.status.as_u16(), 400, "{bad}");
        }
    }

    #[test]
    fn all_lists_every_folder_and_excludes_folder() {
        let query = parse_list(Some("all=true&include_dead=true&q=x")).unwrap();
        assert!(query.all && query.include_dead);
        assert!(!parse_list(Some("all=false")).unwrap().all);
        assert!(!parse_list(None).unwrap().all);
        for bad in ["all=yes", "all=true&folder=/a", "all=1&project=/a"] {
            let error = parse_list(Some(bad)).unwrap_err();
            assert_eq!(error.status.as_u16(), 400, "{bad}");
        }
    }

    #[test]
    fn attach_parameters_need_a_size_for_controllers() {
        assert_eq!(
            parse_attach(None).unwrap(),
            AttachQuery {
                role: Role::Observer,
                cols: 0,
                rows: 0
            }
        );
        assert_eq!(
            parse_attach(Some("role=controller&cols=80&rows=24")).unwrap(),
            AttachQuery {
                role: Role::Controller,
                cols: 80,
                rows: 24
            }
        );
        assert_eq!(
            parse_attach(Some("role=observer&cols=40&rows=20"))
                .unwrap()
                .cols,
            40
        );
        for bad in [
            "role=controller",
            "role=controller&cols=80",
            "role=owner",
            "role=observer&role=controller",
            "cols=0",
            "cols=1001",
            "rows=x",
            "cols=1&cols=2",
            "size=80x24",
        ] {
            let error = parse_attach(Some(bad)).unwrap_err();
            assert_eq!(error.status.as_u16(), 400, "{bad}");
        }
    }

    #[test]
    fn content_parameters_have_defaults_and_bounds() {
        let default = parse_content(None).unwrap();
        assert_eq!(default.source, ContentSource::Screen);
        assert_eq!(default.effective_lines(), None);
        let scrollback = parse_content(Some("source=scrollback&format=ansi")).unwrap();
        assert_eq!(scrollback.effective_lines(), Some(DEFAULT_SCROLLBACK_LINES));
        assert_eq!(scrollback.format, ContentFormat::Ansi);
        assert_eq!(parse_content(Some("lines=5")).unwrap().lines, Some(5));
        for bad in [
            "lines=0",
            "lines=10001",
            "lines=x",
            "source=all",
            "format=html",
            "x=1",
        ] {
            assert!(parse_content(Some(bad)).is_err(), "{bad}");
        }
        assert!(parse_none(Some("a=1")).is_err());
        assert!(parse_none(Some("")).is_ok());
    }
}
