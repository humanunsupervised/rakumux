//! Claude Code's command line.
use super::*;

pub(super) const COMMAND: AgentCommand = AgentCommand {
    label: "claude",
    programs: &["claude"],
    prompt: PromptBy::Argument,
    takes_prompt: &[],
    headless: None,
    unattended: "--dangerously-skip-permissions",
    arguments: &[
        switch(
            "--dangerously-skip-permissions",
            "Skip every permission prompt",
        ),
        option("--permission-mode", "mode", "How permissions are asked for").values(&[
            "acceptEdits",
            "auto",
            "bypassPermissions",
            "dontAsk",
            "manual",
            "plan",
        ]),
        option("--model", "model", "The model").values(&["fable", "opus", "sonnet", "haiku"]),
        option("--effort", "level", "How hard it thinks")
            .values(&["low", "medium", "high", "xhigh", "max"]),
        option("--allowedTools", "tools", "Tools it may use without asking"),
        option("--disallowedTools", "tools", "Tools it may not use"),
        option("--add-dir", "folder", "Another folder it may work in").again(),
        option("--append-system-prompt", "text", "Add to the system prompt"),
        option("--agent", "name", "Run as this agent"),
        option("--mcp-config", "file", "Load MCP servers"),
        switch("--strict-mcp-config", "Only --mcp-config's MCP servers"),
        option("--settings", "file", "Load more settings"),
        option(
            "--fallback-model",
            "model",
            "The model when the first is busy",
        ),
        option("--worktree", "name", "Work in a new git worktree"),
        switch("--chrome", "Use Claude in Chrome"),
        switch("--print", "Print the answer and exit"),
        option(
            "--max-budget-usd",
            "amount",
            "Spend at most this (with --print)",
        ),
        switch("--verbose", "Show every turn in full"),
    ],
};
