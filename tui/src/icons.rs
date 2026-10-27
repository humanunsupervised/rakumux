//! The marks for folders, git branches and the list's App and Status
//! tabs, in the style the user picks
//! (`sessions.icons`, Settings ▸ General ▸ Icons): plain symbols that every
//! terminal font has, emoji (needs a colour emoji font, which macOS, Windows
//! and most Linux desktops have), or Nerd Font icons (needs a Nerd Font set
//! in the terminal). A TUI can't choose the terminal's font, so the user
//! picks what their terminal shows.

use serde::{Deserialize, Serialize};

/// Which glyphs mark folders and branches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Icons {
    /// `▤` and `⎇`: in common terminal fonts.
    #[default]
    Plain,
    /// 📁 and 🌿, two columns wide each.
    Emoji,
    /// Nerd Font's folder and git-branch icons.
    Nerd,
}

impl Icons {
    /// In the order the setting cycles through them.
    pub const ALL: [Icons; 3] = [Icons::Plain, Icons::Emoji, Icons::Nerd];

    pub fn label(self) -> &'static str {
        match self {
            Icons::Plain => "Plain",
            Icons::Emoji => "Emoji",
            Icons::Nerd => "Nerd Font",
        }
    }

    /// Marks a folder: the folder filter (list and tab bar) and the
    /// new-session dialog's folder.
    pub fn folder(self) -> &'static str {
        match self {
            Icons::Plain => "▤",
            Icons::Emoji => "📁",
            // nf-fa-folder
            Icons::Nerd => "\u{f07b}",
        }
    }

    /// Marks the list's App tab: the agent or program sessions run.
    pub fn app(self) -> &'static str {
        match self {
            Icons::Plain => "◇",
            Icons::Emoji => "🤖",
            // nf-fa-cube
            Icons::Nerd => "\u{f1b2}",
        }
    }

    /// Marks the feed: what agents said when they finished.
    pub fn feed(self) -> &'static str {
        match self {
            Icons::Plain => "≡",
            Icons::Emoji => "📰",
            // nf-fa-rss
            Icons::Nerd => "\u{f09e}",
        }
    }

    /// Marks the list's Status tab: what sessions are doing.
    pub fn status(self) -> &'static str {
        match self {
            Icons::Plain => "◔",
            Icons::Emoji => "🚦",
            // nf-fa-tasks
            Icons::Nerd => "\u{f0ae}",
        }
    }

    /// Marks the coding agents a session has working: `◈ 3` subagents.
    pub fn agents(self) -> &'static str {
        match self {
            Icons::Plain => "◈",
            Icons::Emoji => "🤖",
            // nf-md-robot
            Icons::Nerd => "\u{f06a9}",
        }
    }

    /// Marks a git branch: `⎇ main`.
    pub fn branch(self) -> &'static str {
        match self {
            Icons::Plain => "⎇",
            Icons::Emoji => "🌿",
            // nf-dev-git_branch
            Icons::Nerd => "\u{e725}",
        }
    }

    /// Marks done's sub-sections, by when their sessions were last read:
    /// `◷ last hour`.
    pub fn clock(self) -> &'static str {
        match self {
            Icons::Plain => "◷",
            Icons::Emoji => "🕒",
            // nf-fa-clock_o
            Icons::Nerd => "\u{f017}",
        }
    }

    /// The next style, wrapping.
    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|i| *i == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::str_width;

    #[test]
    fn each_style_has_a_folder_and_a_branch_and_they_cycle() {
        assert_eq!(Icons::default(), Icons::Plain);
        assert_eq!((Icons::Plain.folder(), Icons::Plain.branch()), ("▤", "⎇"));
        assert_eq!((Icons::Emoji.folder(), Icons::Emoji.branch()), ("📁", "🌿"));
        assert_eq!(Icons::Nerd.folder(), "\u{f07b}");
        assert_eq!(Icons::Plain.next(), Icons::Emoji);
        assert_eq!(Icons::Nerd.next(), Icons::Plain);
        // Emoji take two columns; layout code must measure, not count.
        assert_eq!(str_width(Icons::Emoji.folder()), 2);
        assert_eq!(str_width(Icons::Plain.folder()), 1);
        // The list's tabs have a mark in every style.
        for icons in Icons::ALL {
            assert!(!icons.app().is_empty() && !icons.status().is_empty());
        }
        assert_eq!(str_width(Icons::Emoji.app()), 2);
        assert_eq!((Icons::Plain.app(), Icons::Plain.status()), ("◇", "◔"));
        assert_eq!(
            (Icons::Plain.clock(), str_width(Icons::Plain.clock())),
            ("◷", 1)
        );
        assert_eq!(str_width(Icons::Emoji.clock()), 2);
        assert_eq!(
            serde_json::to_value(Icons::Nerd).unwrap(),
            serde_json::json!("nerd")
        );
    }
}
