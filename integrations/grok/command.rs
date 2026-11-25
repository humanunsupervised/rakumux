//! Grok Build's command line.
use super::*;

pub(super) const COMMAND: AgentCommand = AgentCommand {
    label: "grok",
    programs: &["grok"],
    prompt: PromptBy::Argument,
    takes_prompt: &["-p", "--single"],
    headless: None,
    unattended: "--always-approve",
    arguments: &[
        switch("--always-approve", "Approve every tool"),
        option("--permission-mode", "mode", "How permissions are asked for").values(&[
            "default",
            "acceptEdits",
            "auto",
            "dontAsk",
            "bypassPermissions",
            "plan",
        ]),
        option("--model", "model", "The model"),
        option("--reasoning-effort", "level", "How hard it thinks")
            .values(&["low", "medium", "high"]),
        option("--max-turns", "count", "At most this many turns"),
        option("--allow", "rule", "Allow what matches").again(),
        option("--deny", "rule", "Deny what matches").again(),
        option("--sandbox", "profile", "What its commands may touch"),
        option("--rules", "text", "Add to the system prompt"),
        option("--add-dir", "folder", "Another folder it may work in").again(),
        option("--agent", "name", "Run as this agent"),
        switch("--no-subagents", "No subagents"),
        switch("--no-plan", "No plan mode"),
        switch("--disable-web-search", "No web"),
        option("--worktree", "name", "Work in a new git worktree"),
    ],
};
