//! GitHub Copilot's command line.
use super::*;

pub(super) const COMMAND: AgentCommand = AgentCommand {
    label: "copilot",
    programs: &["copilot"],
    prompt: PromptBy::Flag("--interactive"),
    takes_prompt: &["-i", "--interactive", "-p", "--prompt"],
    headless: None,
    unattended: "--allow-all --no-ask-user",
    arguments: &[
        switch("--allow-all", "Allow every tool, path and URL"),
        switch("--no-ask-user", "Never ask you questions"),
        switch("--autopilot", "Keep going on its own"),
        option(
            "--max-autopilot-continues",
            "count",
            "How often autopilot continues",
        ),
        option("--mode", "mode", "The mode it starts in").values(&[
            "interactive",
            "plan",
            "autopilot",
        ]),
        option("--model", "model", "The model").values(&["auto"]),
        option("--reasoning-effort", "level", "How hard it thinks")
            .values(&["none", "minimal", "low", "medium", "high", "xhigh", "max"]),
        switch("--allow-all-tools", "Allow every tool"),
        switch("--allow-all-paths", "Allow every path"),
        switch("--allow-all-urls", "Allow every URL"),
        option("--allow-tool", "tools", "Allow these tools").again(),
        option("--deny-tool", "tools", "Deny these tools").again(),
        option("--add-dir", "folder", "Another folder it may work in").again(),
        option("--agent", "name", "Run as this agent"),
        option("--max-ai-credits", "credits", "Spend at most this"),
        option("--additional-mcp-config", "json", "Load MCP servers").again(),
        switch("--no-custom-instructions", "Ignore instruction files"),
    ],
};
