//! Droid's command line.
use super::*;

pub(super) const COMMAND: AgentCommand = AgentCommand {
    label: "droid",
    programs: &["droid"],
    prompt: PromptBy::Argument,
    takes_prompt: &[],
    headless: None,
    unattended: "--auto high",
    arguments: &[
        option("--auto", "level", "How much it does without asking")
            .values(&["low", "medium", "high"]),
        switch("--use-spec", "Start in spec mode"),
        option("--settings", "path", "Load more settings"),
        option("--append-system-prompt", "text", "Add to the system prompt"),
        option(
            "--append-system-prompt-file",
            "path",
            "Add a file to the system prompt",
        ),
        option("--worktree", "name", "Work in a git worktree"),
        switch("--disable-builtin-skills", "No built-in skills"),
    ],
};
