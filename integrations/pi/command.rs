//! Pi's command line.
use super::*;

pub(super) const COMMAND: AgentCommand = AgentCommand {
    label: "pi",
    programs: &["pi"],
    prompt: PromptBy::Argument,
    takes_prompt: &[],
    headless: None,
    unattended: "",
    arguments: &[
        option("--model", "model", "The model"),
        option("--provider", "name", "The model's provider"),
        option("--thinking", "level", "How hard it thinks")
            .values(&["off", "minimal", "low", "medium", "high", "xhigh", "max"]),
        switch("--approve", "Trust the folder's own files"),
        option("--tools", "tools", "Only these tools"),
        option("--exclude-tools", "tools", "Not these tools"),
        option("--append-system-prompt", "text", "Add to the system prompt"),
        option("--extension", "path", "Load an extension").again(),
        option("--skill", "path", "Load a skill").again(),
        switch("--no-mcp", "No MCP servers"),
        switch("--no-context-files", "Ignore AGENTS.md and CLAUDE.md"),
        switch("--offline", "No network as it starts"),
        switch("--print", "Print the answer and exit"),
    ],
};
