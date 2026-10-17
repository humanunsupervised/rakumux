//! Prompt construction and output clean-up for AI session naming, ported
//! from the shell `ai-generate-name-from-base64` service.

use super::NamingInput;

/// Most recent history entries sent to the provider (the shell pipeline kept
/// `tail -n 50` of the history file).
pub(crate) const HISTORY_LIMIT: usize = 50;

/// Longest name the clean-up keeps, in characters.
pub(crate) const MAX_NAME_LEN: usize = 100;

const PROMPT_INTRO: &str = "Analyze this terminal session and create a descriptive name based ONLY on the console output and commands executed.

Terminal output:
";

const PROMPT_INSTRUCTIONS: &str = "

IMPORTANT: Look carefully at the terminal output for:
1. Commands that were executed (lines starting with $ or > or commands after prompts)
2. Application output patterns (errors, build messages, server logs)
3. Tool-specific output (git commands, npm/yarn, docker, ssh, etc.)
4. AI coding assistant interactions (Claude Code, OpenCode, Cursor, etc.)
5. Git-specific patterns and outputs

CRITICAL GIT DETECTION: Check for ANY of these Git indicators in the last outputs:
- Git commands: git status, git diff, git add, git commit, git push, git pull, git checkout, git branch, git merge, git rebase, git log, git stash, etc.
- Git output patterns: \"On branch\", \"Your branch is\", \"Changes not staged\", \"Changes to be committed\", \"Untracked files\", \"modified:\", \"deleted:\", \"new file:\", \"renamed:\"
- Git diff output: Lines starting with +, -, @@, diff --git
- Git merge/rebase messages: \"CONFLICT\", \"Merge branch\", \"Rebase\", \"Cherry-pick\"
- Git status indicators: \"nothing to commit\", \"working tree clean\", \"ahead of\", \"behind\"
- Git error messages: \"fatal:\", \"error:\", \"warning:\" (when preceded by git commands)

DESCRIPTION ONLY: Return a short description of the activity.
- Do NOT include the app/tool name or any prefix in the output.
- If Git patterns are detected, the description should still reflect the git activity, but do not add \"git_\".

Describe what the user was actually doing based on the console output, not just the current state.

The name should be max 100 chars total, use underscores only (no colons or special chars).

Respond with ONLY the session name, nothing else.";

/// The terminal transcript shown to the model: the last [`HISTORY_LIMIT`]
/// commands as `$ <command>` lines, a blank line, then the visible screen
/// without trailing blanks. `None` when there is neither history nor screen
/// text.
pub(crate) fn build_content(input: &NamingInput) -> Option<String> {
    let commands: Vec<&str> = input
        .history
        .iter()
        .map(|command| command.trim())
        .filter(|command| !command.is_empty())
        .collect();
    let commands = &commands[commands.len().saturating_sub(HISTORY_LIMIT)..];
    let screen = trim_screen(&input.screen);
    if commands.is_empty() && screen.is_empty() {
        return None;
    }

    let mut content = String::new();
    for command in commands {
        content.push_str("$ ");
        content.push_str(command);
        content.push('\n');
    }
    if !screen.is_empty() {
        if !content.is_empty() {
            content.push('\n');
        }
        content.push_str(&screen);
    }
    Some(content.trim_end().to_string())
}

/// The full naming prompt for a transcript, word for word as the shell
/// service sent it.
pub(crate) fn build_prompt(content: &str) -> String {
    format!("{PROMPT_INTRO}{content}{PROMPT_INSTRUCTIONS}")
}

/// Turn the model's reply into a session name with the shell service's rules:
/// spaces become `_`, everything but ASCII letters, digits, `_` and `-` is
/// dropped, the result is cut to [`MAX_NAME_LEN`] characters and leading `_`
/// are stripped. Only the first non-empty line of the reply is used. `None`
/// when nothing usable is left.
pub(crate) fn clean_name(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|line| !line.is_empty())?;
    let name: String = line
        .chars()
        .map(|c| if c == ' ' { '_' } else { c })
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .take(MAX_NAME_LEN)
        .collect();
    let name = name.trim_start_matches('_');
    (!name.is_empty()).then(|| name.to_string())
}

/// Screen text without trailing whitespace per row and without leading or
/// trailing blank rows.
fn trim_screen(screen: &str) -> String {
    let rows: Vec<&str> = screen.lines().map(str::trim_end).collect();
    let first = rows.iter().position(|row| !row.is_empty());
    let last = rows.iter().rposition(|row| !row.is_empty());
    match (first, last) {
        (Some(first), Some(last)) => rows[first..=last].join("\n"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(history: &[&str], screen: &str) -> NamingInput {
        NamingInput {
            history: history.iter().map(|s| s.to_string()).collect(),
            screen: screen.to_string(),
        }
    }

    #[test]
    fn content_lists_commands_then_screen() {
        let content = build_content(&input(
            &["cargo build", "  cargo test  "],
            "\n\n$ cargo test   \nrunning 3 tests\n\n\n",
        ))
        .unwrap();
        assert_eq!(
            content,
            "$ cargo build\n$ cargo test\n\n$ cargo test\nrunning 3 tests"
        );
    }

    #[test]
    fn content_with_only_history_or_only_screen() {
        assert_eq!(
            build_content(&input(&["ls", "", "   "], "")).unwrap(),
            "$ ls"
        );
        assert_eq!(
            build_content(&input(&[], "  vim main.rs  \n")).unwrap(),
            "  vim main.rs"
        );
    }

    #[test]
    fn content_is_none_without_anything_to_go_on() {
        assert_eq!(build_content(&input(&[], "")), None);
        assert_eq!(build_content(&input(&["  ", ""], " \n \n")), None);
    }

    #[test]
    fn content_keeps_the_most_recent_commands() {
        let history: Vec<String> = (0..80).map(|i| format!("cmd{i}")).collect();
        let content = build_content(&NamingInput {
            history,
            screen: String::new(),
        })
        .unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), HISTORY_LIMIT);
        assert_eq!(lines.first(), Some(&"$ cmd30"));
        assert_eq!(lines.last(), Some(&"$ cmd79"));
    }

    #[test]
    fn prompt_wraps_the_transcript() {
        let prompt = build_prompt("$ npm run dev\nready on :3000");
        assert!(prompt.starts_with(
            "Analyze this terminal session and create a descriptive name based ONLY on the console output and commands executed.\n\nTerminal output:\n$ npm run dev\nready on :3000\n\nIMPORTANT: Look carefully"
        ));
        assert!(prompt.contains(
            "- Git status indicators: \"nothing to commit\", \"working tree clean\", \"ahead of\", \"behind\"\n"
        ));
        assert!(prompt.contains(
            "The name should be max 100 chars total, use underscores only (no colons or special chars).\n\n"
        ));
        assert!(prompt.ends_with("Respond with ONLY the session name, nothing else."));
    }

    #[test]
    fn clean_name_applies_the_shell_rules() {
        assert_eq!(
            clean_name("fixing auth bug").as_deref(),
            Some("fixing_auth_bug")
        );
        assert_eq!(
            clean_name("\"running: next.js dev-server!\"").as_deref(),
            Some("running_nextjs_dev-server")
        );
        assert_eq!(
            clean_name("  __reviewing changes  ").as_deref(),
            Some("reviewing_changes")
        );
        assert_eq!(
            clean_name("café ünïcode build").as_deref(),
            Some("caf_ncode_build")
        );
    }

    #[test]
    fn clean_name_uses_the_first_non_empty_line() {
        assert_eq!(
            clean_name("\n\nresolving_merge_conflicts\nBecause the output shows...").as_deref(),
            Some("resolving_merge_conflicts")
        );
    }

    #[test]
    fn clean_name_truncates_to_the_limit() {
        let long = "a".repeat(250);
        assert_eq!(clean_name(&long).unwrap().len(), MAX_NAME_LEN);
        let name = clean_name(&format!("_{}", "b c ".repeat(60))).unwrap();
        assert!(name.len() <= MAX_NAME_LEN);
        assert!(name.starts_with("b_c_"));
    }

    #[test]
    fn clean_name_rejects_unusable_replies() {
        assert_eq!(clean_name(""), None);
        assert_eq!(clean_name("   \n  "), None);
        assert_eq!(clean_name("!!! ::: ???"), None);
        assert_eq!(clean_name("___"), None);
    }
}
