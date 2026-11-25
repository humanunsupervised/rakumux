//! Kimi Code's command line.
use super::*;

pub(super) const COMMAND: AgentCommand = AgentCommand {
    label: "kimi",
    programs: &["kimi"],
    prompt: PromptBy::Typed,
    takes_prompt: &["-p", "--prompt"],
    headless: None,
    unattended: "--auto",
    arguments: &[
        switch("--auto", "Never ask"),
        switch("--yolo", "Ask only about risky actions"),
        switch("--plan", "Start in plan mode"),
        option("--model", "model", "The model"),
        option("--agent", "name", "Run as this agent"),
        option("--agent-file", "path", "Run as the agent in this file"),
        option("--add-dir", "folder", "Another folder it may work in").again(),
        option("--skills-dir", "folder", "Load skills from here").again(),
    ],
};
