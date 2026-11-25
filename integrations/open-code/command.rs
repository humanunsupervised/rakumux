//! OpenCode's command line.
use super::*;

pub(super) const COMMAND: AgentCommand = AgentCommand {
    label: "opencode",
    programs: &["opencode", "opencode2"],
    prompt: PromptBy::Typed,
    takes_prompt: &["--prompt"],
    headless: Some("run"),
    unattended: "--auto",
    arguments: &[
        switch("--auto", "Allow whatever isn't denied"),
        switch("--standalone", "Run a server of its own"),
        option("--server", "url", "Use the server at this address"),
        switch("--print-logs", "Print logs"),
        option("--log-level", "level", "Which logs").values(&["debug", "info", "warn", "error"]),
    ],
};
