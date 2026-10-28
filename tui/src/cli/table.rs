//! Plain-text tables and cell formats for the human-facing commands (`ls`,
//! `folders`).

use session_core::model::GitInfo;

/// A column: its header and one cell per row.
pub(crate) struct Column {
    header: &'static str,
    cells: Vec<String>,
    /// Left out when every cell is empty.
    optional: bool,
}

impl Column {
    pub fn new(header: &'static str, cells: Vec<String>) -> Self {
        Self {
            header,
            cells,
            optional: false,
        }
    }

    /// A column shown only when some row has a value.
    pub fn optional(header: &'static str, cells: Vec<String>) -> Self {
        Self {
            optional: true,
            ..Self::new(header, cells)
        }
    }

    fn shown(&self) -> bool {
        !self.optional || self.cells.iter().any(|cell| !cell.is_empty())
    }
}

/// Columns aligned two spaces apart, one line per row after the header,
/// without trailing spaces.
pub(crate) fn render(columns: Vec<Column>) -> String {
    let columns: Vec<Column> = columns.into_iter().filter(Column::shown).collect();
    let widths: Vec<usize> = columns
        .iter()
        .map(|column| {
            column
                .cells
                .iter()
                .map(|cell| cell.chars().count())
                .chain([column.header.chars().count()])
                .max()
                .unwrap_or(0)
        })
        .collect();
    let rows = columns.first().map_or(0, |column| column.cells.len());
    let mut text = String::new();
    for row in 0..=rows {
        let mut line = String::new();
        for (i, column) in columns.iter().enumerate() {
            let cell = match row {
                0 => column.header,
                _ => column.cells[row - 1].as_str(),
            };
            if i > 0 {
                line.push_str("  ");
            }
            line.push_str(cell);
            line.extend(std::iter::repeat_n(
                ' ',
                widths[i].saturating_sub(cell.chars().count()),
            ));
        }
        text.push_str(line.trim_end());
        text.push('\n');
    }
    text
}

/// Unix seconds now.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// How long ago `then` was, coarsely: `now`, `5m ago`, `2h ago`, `3d ago`.
pub(crate) fn ago(now: i64, then: i64) -> String {
    let secs = (now - then).max(0);
    match secs {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// Branch (or detached commit) and linked worktree:
/// `main`, `detached 1a2b3c4`, `fix (worktree fix-login)`.
pub(crate) fn git_text(git: &GitInfo) -> String {
    let head = match (&git.branch, &git.detached_at) {
        (Some(branch), _) => branch.clone(),
        (None, Some(id)) => format!("detached {id}"),
        (None, None) => "unknown".into(),
    };
    match &git.worktree {
        Some(worktree) => format!("{head} (worktree {worktree})"),
        None => head,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_align_and_empty_optional_ones_drop() {
        let text = render(vec![
            Column::optional("", vec!["*".into(), "".into()]),
            Column::new("NAME", vec!["api".into(), "web-server".into()]),
            Column::optional("GIT", vec!["".into(), "".into()]),
            Column::new("ID", vec!["1".into(), "22".into()]),
        ]);
        assert_eq!(
            text,
            "   NAME        ID\n\
             *  api         1\n   \
             web-server  22\n"
        );
        assert_eq!(render(vec![Column::new("A", vec![])]), "A\n");
    }

    #[test]
    fn times_and_git() {
        assert_eq!(ago(1000, 990), "now");
        assert_eq!(ago(1000, 1000 - 125), "2m ago");
        assert_eq!(ago(100_000, 100_000 - 7200), "2h ago");
        assert_eq!(ago(1_000_000, 0), "11d ago");
        let mut git = GitInfo {
            repo_root: "/r".into(),
            branch: Some("main".into()),
            detached_at: None,
            worktree: None,
            repo: None,
            origin: None,
        };
        assert_eq!(git_text(&git), "main");
        git.worktree = Some("fix-login".into());
        assert_eq!(git_text(&git), "main (worktree fix-login)");
        git.branch = None;
        git.detached_at = Some("1a2b3c4".into());
        git.worktree = None;
        assert_eq!(git_text(&git), "detached 1a2b3c4");
        git.detached_at = None;
        assert_eq!(git_text(&git), "unknown");
    }
}
