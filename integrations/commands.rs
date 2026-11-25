//! The coding agents' command lines, for heartbeats: the program that starts
//! each, how it takes a prompt as it starts, the arguments worth suggesting,
//! and those that let it run with nobody there to answer it. Each agent's are
//! in `integrations/<agent>/command.rs`, from its own `--help`.
//!
//! A run's command is the heartbeat's with its prompt as the last argument
//! ([`with_prompt`]): `claude --dangerously-skip-permissions -- "<prompt>"`.
//! An agent that takes none as it starts ([`PromptBy::Typed`]) has it typed
//! once its own prompt shows.

#[path = "amp/command.rs"]
mod amp;
#[path = "claude-code/command.rs"]
mod claude;
#[path = "codex/command.rs"]
mod codex;
#[path = "copilot/command.rs"]
mod copilot;
#[path = "cursor-agent/command.rs"]
mod cursor;
#[path = "droid/command.rs"]
mod droid;
#[path = "grok/command.rs"]
mod grok;
#[path = "kimi/command.rs"]
mod kimi;
#[path = "open-code/command.rs"]
mod opencode;
#[path = "pi/command.rs"]
mod pi;

/// How an agent takes a prompt as it starts an interactive session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptBy {
    /// As its last argument, after `--`.
    Argument,
    /// As this option's value.
    Flag(&'static str),
    /// It doesn't: the prompt is typed into it.
    Typed,
}

/// One argument of an agent's command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Argument {
    pub flag: &'static str,
    /// What its value is, when it takes one.
    pub value: Option<&'static str>,
    /// The values it takes, when they're known.
    pub values: &'static [&'static str],
    pub about: &'static str,
    /// It may be given more than once.
    pub repeats: bool,
}

const fn switch(flag: &'static str, about: &'static str) -> Argument {
    Argument {
        flag,
        value: None,
        values: &[],
        about,
        repeats: false,
    }
}

const fn option(flag: &'static str, value: &'static str, about: &'static str) -> Argument {
    Argument {
        value: Some(value),
        ..switch(flag, about)
    }
}

impl Argument {
    const fn values(self, values: &'static [&'static str]) -> Self {
        Self { values, ..self }
    }

    const fn again(self) -> Self {
        Self {
            repeats: true,
            ..self
        }
    }
}

/// An agent's command line.
#[derive(Debug, PartialEq, Eq)]
pub struct AgentCommand {
    /// The agent, as Rakumux names it.
    pub label: &'static str,
    /// The programs that start it; the first is the one to run.
    pub programs: &'static [&'static str],
    pub prompt: PromptBy,
    /// Options whose value is the prompt: a command that ends with one takes
    /// it there.
    pub takes_prompt: &'static [&'static str],
    /// Its subcommand that runs a prompt (an argument) and exits, when the
    /// interactive command doesn't take one the same way.
    pub headless: Option<&'static str>,
    /// Arguments with which it doesn't stop to ask.
    pub unattended: &'static str,
    pub arguments: &'static [Argument],
}

impl AgentCommand {
    /// The program to run.
    pub fn program(&self) -> &'static str {
        self.programs.first().copied().unwrap_or(self.label)
    }

    /// The argument `flag` names.
    pub fn argument(&self, flag: &str) -> Option<&'static Argument> {
        self.arguments.iter().find(|known| known.flag == flag)
    }
}

const AIDER: AgentCommand = AgentCommand {
    label: "aider",
    programs: &["aider"],
    prompt: PromptBy::Typed,
    takes_prompt: &["-m", "--message"],
    headless: None,
    unattended: "--yes-always",
    arguments: &[
        switch("--yes-always", "Say yes to everything"),
        option("--model", "model", "The model"),
        switch("--architect", "Plan with one model, edit with another"),
        option("--editor-model", "model", "The model that edits"),
        option("--reasoning-effort", "level", "How hard it thinks")
            .values(&["low", "medium", "high"]),
        switch("--auto-test", "Run the tests after changes"),
        option("--test-cmd", "command", "What runs the tests"),
        switch("--no-auto-commits", "Don't commit changes"),
        option("--read", "file", "A file to read").again(),
        option("--file", "file", "A file to edit").again(),
    ],
};

/// The agents a heartbeat runs.
pub const AGENTS: &[AgentCommand] = &[
    claude::COMMAND,
    codex::COMMAND,
    opencode::COMMAND,
    copilot::COMMAND,
    cursor::COMMAND,
    pi::COMMAND,
    grok::COMMAND,
    kimi::COMMAND,
    droid::COMMAND,
    amp::COMMAND,
    AIDER,
];

/// The agent Rakumux names `label`.
pub fn agent(label: &str) -> Option<&'static AgentCommand> {
    AGENTS.iter().find(|agent| agent.label == label)
}

/// The agent a command starts (`FOO=1 ~/bin/claude --x`): its first word
/// that isn't an environment assignment.
pub fn agent_of(command: &str) -> Option<&'static AgentCommand> {
    let program = words(command).next()?;
    let program = program.rsplit('/').next().unwrap_or(program);
    AGENTS
        .iter()
        .find(|agent| agent.programs.contains(&program))
}

/// The command's words, from its program on.
fn words(command: &str) -> impl Iterator<Item = &str> {
    command
        .split_whitespace()
        .skip_while(|word| word.contains('='))
}

/// `command` with `prompt` (one word, as a shell reads it) where its agent
/// takes it. None when the prompt is typed instead: the agent takes none as
/// it starts, isn't known, or the line is more than one command
/// ([`problem`]).
pub fn with_prompt(command: &str, prompt: &str) -> Option<String> {
    let command = command.trim();
    let agent = agent_of(command)?;
    if problem(command).is_some() {
        return None;
    }
    let mut arguments = words(command).skip(1);
    let first = arguments.next();
    let last = arguments.last().or(first);
    if last.is_some_and(|word| agent.takes_prompt.contains(&word)) {
        return Some(format!("{command} {prompt}"));
    }
    let by = match first {
        Some(word) if agent.headless == Some(word) => PromptBy::Argument,
        _ => agent.prompt,
    };
    match by {
        PromptBy::Argument => Some(format!("{command} -- {prompt}")),
        PromptBy::Flag(flag) => Some(format!("{command} {flag} {prompt}")),
        PromptBy::Typed => None,
    }
}

/// Why `command` can't have an argument added at its end: a quote left open,
/// or something a shell reads as more than one command's words.
pub fn problem(command: &str) -> Option<&'static str> {
    let mut quote: Option<char> = None;
    let mut chars = command.chars();
    let mut word_starts = true;
    while let Some(c) = chars.next() {
        match (c, quote) {
            ('\\', None | Some('"')) => {
                if chars.next().is_none() {
                    return Some("it ends with a \\");
                }
            }
            (c, Some(open)) if c == open => quote = None,
            (_, Some(_)) => {}
            ('\'' | '"', None) => quote = Some(c),
            ('|' | '&' | ';' | '<' | '>' | '(' | ')' | '`' | '\n', None) => {
                return Some("| & ; < > ( ) ` go in quotes")
            }
            ('#', None) if word_starts => return Some("a word that starts with # goes in quotes"),
            _ => {}
        }
        word_starts = quote.is_none() && c.is_whitespace();
    }
    quote.map(|_| "a quote isn't closed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_prompt_goes_where_its_agent_takes_it() {
        let with = |command: &str| with_prompt(command, "\"$P\"");
        assert_eq!(
            with("claude --dangerously-skip-permissions ").as_deref(),
            Some("claude --dangerously-skip-permissions -- \"$P\"")
        );
        assert_eq!(
            with("FOO=1 ~/bin/codex exec --model x").as_deref(),
            Some("FOO=1 ~/bin/codex exec --model x -- \"$P\"")
        );
        assert_eq!(
            with("copilot --allow-all").as_deref(),
            Some("copilot --allow-all --interactive \"$P\"")
        );
        assert_eq!(
            with("copilot --allow-all -p").as_deref(),
            Some("copilot --allow-all -p \"$P\"")
        );
        assert_eq!(with("grok -p").as_deref(), Some("grok -p \"$P\""));
        assert_eq!(with("amp -x").as_deref(), Some("amp -x \"$P\""));
        assert_eq!(
            with("opencode run --auto").as_deref(),
            Some("opencode run --auto -- \"$P\"")
        );
        // Typed into these, or into what isn't one known command.
        for command in [
            "opencode --auto",
            "kimi --auto",
            "amp",
            "my-agent --x",
            "claude | tee log",
            "cd web && claude",
            "claude --append-system-prompt \"be brief",
            "",
        ] {
            assert_eq!(with(command), None, "{command}");
        }
    }

    #[test]
    fn a_command_is_one_command_with_its_quotes_closed() {
        for fine in [
            "claude --allowedTools 'Bash(git *)' --x \"a; b\"",
            "claude --append-system-prompt it\\'s",
            "codex -c model=\"o3\" --add-dir ~/a#b $EXTRA",
        ] {
            assert_eq!(problem(fine), None, "{fine}");
        }
        for wrong in ["claude 'x", "claude \"x\\\"", "claude \\", "claude; ls"] {
            assert!(problem(wrong).is_some(), "{wrong}");
        }
        assert!(problem("claude # --x").is_some());
    }

    #[test]
    fn every_agent_is_found_by_its_label_and_programs_and_its_defaults_are_its_arguments() {
        for known in AGENTS {
            assert_eq!(agent(known.label), Some(known));
            for program in known.programs {
                assert_eq!(agent_of(program), Some(known));
            }
            let mut words = known.unattended.split_whitespace();
            while let Some(word) = words.next() {
                let argument = known.argument(word).unwrap_or_else(|| panic!("{word}"));
                if argument.value.is_some() {
                    assert!(argument.values.contains(&words.next().unwrap()));
                }
            }
            let mut flags: Vec<_> = known.arguments.iter().map(|a| a.flag).collect();
            flags.sort();
            flags.dedup();
            assert_eq!(flags.len(), known.arguments.len(), "{}", known.label);
        }
    }
}
