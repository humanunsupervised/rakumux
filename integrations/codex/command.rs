//! Codex's command line.
use super::*;

pub(super) const COMMAND: AgentCommand = AgentCommand {
    label: "codex",
    programs: &["codex"],
    prompt: PromptBy::Argument,
    takes_prompt: &[],
    headless: None,
    unattended: "--dangerously-bypass-approvals-and-sandbox",
    arguments: &[
        switch(
            "--dangerously-bypass-approvals-and-sandbox",
            "Never ask, no sandbox",
        ),
        option(
            "--ask-for-approval",
            "policy",
            "When it asks before a command",
        )
        .values(&["never", "on-request"]),
        option("--sandbox", "mode", "What its commands may touch").values(&[
            "read-only",
            "workspace-write",
            "danger-full-access",
        ]),
        switch("--approve-for-me", "Review approvals automatically"),
        option("--model", "model", "The model"),
        option("--config", "key=value", "Set a configuration value").again(),
        option("--profile", "name", "Use a configuration profile"),
        switch("--search", "Search the web"),
        option("--add-dir", "folder", "Another folder it may write to").again(),
        option("--image", "file", "Attach an image").again(),
        switch("--worktree", "Work in a new git worktree"),
        switch("--oss", "Use an open-source provider"),
        option("--local-provider", "name", "The local provider").values(&["lmstudio", "ollama"]),
        option("--enable", "feature", "Turn a feature on").again(),
        option("--disable", "feature", "Turn a feature off").again(),
        switch("--no-alt-screen", "Keep the terminal's scrollback"),
    ],
};
