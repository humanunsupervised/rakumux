//! Amp's command line.
use super::*;

pub(super) const COMMAND: AgentCommand = AgentCommand {
    label: "amp",
    programs: &["amp"],
    prompt: PromptBy::Typed,
    takes_prompt: &["-x", "--execute"],
    headless: None,
    unattended: "",
    arguments: &[
        option("--mode", "mode", "The model and tools it uses")
            .values(&["low", "medium", "high", "ultra"]),
        switch("--fast", "Faster models, at a premium"),
        option("--features", "names", "Turn features on").values(&["fast", "plaid", "pro"]),
        option("--mcp-config", "file", "Load MCP servers"),
        option("--settings-file", "path", "Load these settings"),
        option("--visibility", "who", "Who sees the thread").values(&[
            "private",
            "unlisted",
            "workspace",
            "group",
        ]),
        switch("--no-ide", "No IDE connection"),
        switch("--no-notifications", "No notifications"),
    ],
};
