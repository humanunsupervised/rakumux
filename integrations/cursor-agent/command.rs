//! Cursor's command line.
use super::*;

pub(super) const COMMAND: AgentCommand = AgentCommand {
    label: "cursor-agent",
    programs: &["cursor-agent"],
    prompt: PromptBy::Argument,
    takes_prompt: &[],
    headless: None,
    unattended: "--force",
    arguments: &[
        switch("--force", "Run commands unless denied"),
        switch("--auto-review", "Run safe calls, ask about the rest"),
        switch("--approve-mcps", "Approve every MCP server"),
        switch("--trust", "Trust the folder"),
        option("--sandbox", "mode", "Run commands in a sandbox").values(&["enabled", "disabled"]),
        option("--mode", "mode", "Only plan, or only answer").values(&["plan", "ask"]),
        option("--model", "model", "The model"),
        option("--add-dir", "folder", "Another folder it may work in").again(),
        option("--worktree", "name", "Work in a new git worktree"),
        option("--worktree-base", "branch", "What the worktree starts from"),
        option("--plugin-dir", "folder", "Load a plugin").again(),
        switch("--print", "Print the answer and exit"),
    ],
};
