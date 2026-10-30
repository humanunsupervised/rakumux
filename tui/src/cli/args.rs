//! Command-line grammar of the `rakumux` subcommands.

use clap::{Args, Parser, Subcommand, ValueEnum};
use session_core::api::{ReadSource, TextFormat, WaitUntil};
use session_core::model::Lifecycle;
use std::path::PathBuf;

/// Subcommand names that make `dispatch` handle the invocation instead of
/// starting the TUI.
pub const SUBCOMMANDS: &[&str] = &[
    "new",
    "ls",
    "attach",
    "rename",
    "pin",
    "unpin",
    "kill",
    "rm",
    "folders",
    "projects",
    "daemon",
    "remote",
    "machine",
    "sprite",
    "account",
    "status-service",
    "session",
    "help",
];

#[derive(Parser, Debug)]
#[command(
    name = "rakumux",
    bin_name = "rakumux",
    about = "Sessions for coding agents: terminals grouped by the folder they run in",
    disable_version_flag = true
)]
pub struct CliArgs {
    /// Path to the rakumux-ptyd daemon binary
    #[arg(long, value_name = "PATH", global = true)]
    pub ptyd_path: Option<PathBuf>,

    /// Ignored (legacy launcher option)
    #[arg(long, value_name = "PATH", global = true, hide = true)]
    pub services_path: Option<String>,

    /// Print extra diagnostics on stderr
    #[arg(short, long, global = true)]
    pub debug: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum Command {
    /// Create a session in this folder and attach to it
    New(NewArgs),
    /// List the sessions of this folder (or of other folders)
    ///
    /// Marks: * the current session, ^ a pinned one.
    Ls(LsArgs),
    /// Attach to a session full-screen
    ///
    /// Keys after the prefix Ctrl-\\: d detach, > / < next / previous running
    /// session, n new session in the current directory, Ctrl-\\ a literal
    /// Ctrl-\\.
    Attach {
        /// Session name or id
        target: String,
    },
    /// Rename a session (the current one when only a name is given)
    Rename {
        /// `<name|id> <new name>`, or just `<new name>` inside a session
        #[arg(value_name = "NAME|ID", required = true, num_args = 1..=2)]
        args: Vec<String>,
    },
    /// Pin a session: it's listed in the pinned section, at the top of the list
    Pin {
        /// Session name or id
        target: String,
    },
    /// Unpin a session
    Unpin {
        /// Session name or id
        target: String,
    },
    /// Terminate a session's process (the record stays)
    Kill {
        /// Session name or id
        target: String,
        /// Also kill the sessions it created, recursively
        #[arg(long)]
        tree: bool,
    },
    /// Delete a session: terminate it and remove its record and scrollback
    Rm {
        /// Session name or id
        target: String,
    },
    /// List the folders sessions belong to, or add or remove one
    ///
    /// `--json` prints `{"folders": [...], "projects": [...]}`: the same
    /// list twice, `projects` kept for older scripts.
    #[command(alias = "projects", args_conflicts_with_subcommands = true)]
    Folders {
        #[command(subcommand)]
        command: Option<FoldersCommand>,
        /// Print JSON instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Other machines running Rakumux, whose sessions are listed and used here
    ///
    /// A machine is reached through its rakumux-api remote listener: its URL
    /// and its remote token (`rakumux remote token` there).
    Machine {
        #[command(subcommand)]
        command: MachineCommand,
    },
    /// Your own Fly.io Sprite for sessions: set it up and connect to it
    ///
    /// Uses your Sprites token (sprites.dev/account), kept on this computer
    /// only. The Sprite runs Rakumux behind its public URL, reached with a
    /// Rakumux token of its own.
    Sprite {
        #[command(subcommand)]
        command: SpriteCommand,
    },
    /// Manage the rakumux-ptyd daemon
    Daemon {
        #[command(subcommand)]
        command: DaemonCommand,
    },
    /// Remote access for the Rakumux app on your phone, over Tailscale
    ///
    /// Off until `rakumux remote on`. rakumux-api then also listens on
    /// this machine's Tailscale addresses (port 7337), for requests that
    /// carry the remote token.
    Remote {
        #[command(subcommand)]
        command: RemoteCommand,
    },
    /// Your Rakumux account: sign in for AI session titles and the agent feed
    ///
    /// Signed in, a background service titles each agent session in a few
    /// words (a name you gave stays) and keeps the feed.
    Account {
        #[command(subcommand)]
        command: AccountCommand,
    },
    /// Run the account and AI title service for this Rakumux home
    /// (started on demand)
    #[command(hide = true)]
    StatusService,
    /// JSON interface for agents and scripts
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
}

#[derive(Args, Debug, PartialEq, Default)]
pub struct LsArgs {
    /// Print JSON instead of a table; each session has `folder` (and `scope`,
    /// the same path)
    #[arg(long)]
    pub json: bool,
    /// List this folder instead of the current one; repeatable
    #[arg(
        long = "folder",
        alias = "project",
        value_name = "DIR",
        conflicts_with = "all"
    )]
    pub folders: Vec<PathBuf>,
    /// List every folder
    #[arg(short, long)]
    pub all: bool,
    /// Include dead sessions
    #[arg(long)]
    pub dead: bool,
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum SpriteCommand {
    /// Set up your Sprite (created when missing) and connect to it
    Connect {
        /// Your Sprites token; `-` reads it from stdin. Default: one found on
        /// this computer (SPRITES_TOKEN, a saved one, the sprite CLI's)
        #[arg(long)]
        token: Option<String>,
        /// The Sprite's name (default: rakumux)
        #[arg(long)]
        sprite: Option<String>,
        /// Accepted and ignored: replies from rakumux.com are typed into a
        /// signed-in Sprite's agents (scripts written for 0.1.32 and earlier
        /// still pass these)
        #[arg(long, hide = true)]
        replies: bool,
        /// Accepted and ignored, likewise
        #[arg(long, hide = true)]
        no_replies: bool,
    },
    /// Which Sprite sessions go to, and whether it's reachable
    Status,
    /// Forget the Sprite and the Sprites token here, revoking its feed
    /// credential (the Sprite stays)
    Disconnect,
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum MachineCommand {
    /// Add a machine: its name here, its rakumux-api URL and remote token
    Add {
        name: String,
        /// e.g. https://box.example or http://100.64.0.2:7337
        url: String,
        /// Its remote token; `-` reads it from stdin
        #[arg(long)]
        token: String,
    },
    /// List the machines added
    Ls {
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Forget a machine and the sessions listed from it (nothing there changes)
    Rm {
        /// Machine name or id
        target: String,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum FoldersCommand {
    /// Add a folder: it is listed even before it has sessions
    Add {
        /// The folder
        dir: PathBuf,
    },
    /// Forget a folder that has no sessions (the folder itself is untouched)
    #[command(alias = "remove")]
    Rm {
        /// The folder
        dir: PathBuf,
    },
}

#[derive(Args, Debug, PartialEq, Default)]
pub struct NewArgs {
    /// Session name (default: generated)
    #[arg(long)]
    pub name: Option<String>,
    /// Working directory (default: this folder); outside this folder it is
    /// the session's own folder
    #[arg(long, value_name = "DIR")]
    pub cwd: Option<PathBuf>,
    /// Command typed into the shell after it starts; repeatable
    #[arg(long = "cmd", value_name = "COMMAND")]
    pub cmds: Vec<String>,
    /// Create the session without attaching
    #[arg(long)]
    pub detach: bool,
    /// Run it on another machine (`rakumux machine ls`), in a folder named
    /// like this one in its home folder
    #[arg(long, value_name = "NAME|ID", conflicts_with = "cwd")]
    pub machine: Option<String>,
    /// Run it on your Sprite (`rakumux sprite connect` sets it up)
    #[arg(long, conflicts_with_all = ["cwd", "machine"])]
    pub sprite: bool,
    /// Program to run instead of the login shell
    #[arg(last = true, value_name = "ARGV")]
    pub argv: Vec<String>,
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum DaemonCommand {
    /// Show whether the daemon runs, and its statistics
    Status {
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Stop the daemon (ends every running session; they don't come back
    /// on the next start)
    Stop {
        /// Don't ask before ending running sessions
        #[arg(short, long)]
        yes: bool,
    },
    /// Start the daemon if it isn't running
    Start,
    /// Replace the daemon with a new one. Running sessions keep running
    /// (an older daemon that can't hand them over is stopped instead, after
    /// asking)
    Restart {
        /// Don't ask before ending running sessions
        #[arg(short, long)]
        yes: bool,
    },
    /// Print the daemon log
    Logs {
        /// Number of lines to show
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        /// Keep printing new lines
        #[arg(short, long)]
        follow: bool,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum RemoteCommand {
    /// Turn remote access on (it stays on) and print the URL, the token and
    /// a link that fills them in on the phone
    On {
        /// Port of the remote listener (default 7337, or the one set before)
        #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
        port: Option<u16>,
    },
    /// Turn remote access off
    Off,
    /// Show the URL, the token, the link and a QR code to pair the phone app
    /// (remote access must be on)
    Pair,
    /// Show whether remote access is on, its URLs and the token's start
    Status {
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Print the remote token
    Token {
        /// Make a new token (apps paired with the old one must be updated)
        #[arg(long)]
        rotate: bool,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum AccountCommand {
    /// Sign in with a code approved in any browser
    Login,
    /// Sign out on this machine (AI titles stop)
    Logout,
    /// Show who is signed in, whether AI session titles are on, and each
    /// live session's AI description
    Status {
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum SessionCommand {
    /// Receive agent hook JSON on stdin (advisory; never blocks an agent).
    Hook {
        #[arg(long, value_enum)]
        agent: HookAgent,
        /// The hook's event, for agents whose input doesn't name it
        #[arg(long, value_name = "NAME")]
        event: Option<String>,
    },
    /// Print an agent's hook configuration, to merge into its settings.
    HookConfig {
        #[arg(long, value_enum)]
        agent: HookAgent,
        /// Write it as a file of Rakumux's own the agent loads instead (GitHub Copilot, Grok)
        #[arg(long)]
        install: bool,
    },
    /// List this folder's sessions: {"sessions":[…]}
    List {
        /// Include dead sessions
        #[arg(long)]
        all: bool,
    },
    /// One session: {"session":{…}}
    Get {
        /// Session name or id
        target: String,
    },
    /// Read a session's screen or recent output
    Read {
        /// Session name or id
        target: String,
        #[arg(long, value_enum, default_value_t = SourceArg::Visible)]
        source: SourceArg,
        /// Number of rows for recent sources
        #[arg(long)]
        lines: Option<u32>,
        #[arg(long, value_enum, default_value_t = FormatArg::Text)]
        format: FormatArg,
    },
    /// Block until a session reaches a state (or the timeout passes)
    Wait {
        /// Session name or id
        target: String,
        #[arg(long, value_enum)]
        until: UntilArg,
        /// Substring to wait for with `--until output`
        #[arg(long)]
        pattern: Option<String>,
        /// Milliseconds
        #[arg(long = "timeout", value_name = "MS", default_value_t = 30_000)]
        timeout_ms: u64,
    },
    /// Report this session's own state and result (inside a session only)
    Report(ReportArgs),
    /// Claim agent conversations for this session (inside a session only),
    /// so tools reached through a shared agent service act as this session.
    /// Used by the OpenCode plugin.
    Bind {
        /// Agent kind, e.g. opencode
        #[arg(long)]
        agent: String,
        /// Conversation ids
        #[arg(required = true, value_name = "CONVERSATION")]
        conversations: Vec<String>,
    },
}

#[derive(Args, Debug, PartialEq, Default)]
pub struct ReportArgs {
    /// What the agent in this session is doing
    #[arg(long, value_enum)]
    pub state: Option<StateArg>,
    /// Short status line shown in the session list
    #[arg(long)]
    pub text: Option<String>,
    /// Result summary
    #[arg(long)]
    pub summary: Option<String>,
    /// File the result refers to; repeatable, needs --summary
    #[arg(long = "file", value_name = "PATH", requires = "summary")]
    pub files: Vec<String>,
    /// Agent running in this session, e.g. claude, codex
    #[arg(long)]
    pub agent_kind: Option<String>,
    /// The agent's own session/conversation id
    #[arg(long, requires = "agent_kind")]
    pub agent_session_id: Option<String>,
    /// The agent's own session/transcript path
    #[arg(long, requires = "agent_kind")]
    pub agent_session_path: Option<String>,
    /// Record the command after `--` as this session's resume command
    #[arg(long)]
    pub resume: bool,
    /// Resume command (after `--`)
    #[arg(last = true, value_name = "ARGV")]
    pub resume_argv: Vec<String>,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum HookAgent {
    Claude,
    Codex,
    Opencode,
    Copilot,
    CursorAgent,
    Pi,
    Grok,
    Kimi,
    Droid,
    Amp,
}

impl HookAgent {
    pub fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Opencode => "opencode",
            Self::Copilot => "copilot",
            Self::CursorAgent => "cursor-agent",
            Self::Pi => "pi",
            Self::Grok => "grok",
            Self::Kimi => "kimi",
            Self::Droid => "droid",
            Self::Amp => "amp",
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceArg {
    Visible,
    Recent,
    #[value(alias = "recent_unwrapped")]
    RecentUnwrapped,
}

impl From<SourceArg> for ReadSource {
    fn from(value: SourceArg) -> Self {
        match value {
            SourceArg::Visible => ReadSource::Visible,
            SourceArg::Recent => ReadSource::Recent,
            SourceArg::RecentUnwrapped => ReadSource::RecentUnwrapped,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatArg {
    Text,
    Ansi,
}

impl From<FormatArg> for TextFormat {
    fn from(value: FormatArg) -> Self {
        match value {
            FormatArg::Text => TextFormat::Text,
            FormatArg::Ansi => TextFormat::Ansi,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum UntilArg {
    Settled,
    Idle,
    Blocked,
    Working,
    Exited,
    Reported,
    Output,
}

impl From<UntilArg> for WaitUntil {
    fn from(value: UntilArg) -> Self {
        match value {
            UntilArg::Settled => WaitUntil::Settled,
            UntilArg::Idle => WaitUntil::Idle,
            UntilArg::Blocked => WaitUntil::Blocked,
            UntilArg::Working => WaitUntil::Working,
            UntilArg::Exited => WaitUntil::Exited,
            UntilArg::Reported => WaitUntil::Reported,
            UntilArg::Output => WaitUntil::Output,
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum StateArg {
    Working,
    Blocked,
    Done,
    Idle,
}

impl From<StateArg> for Lifecycle {
    fn from(value: StateArg) -> Self {
        match value {
            StateArg::Working => Lifecycle::Working,
            StateArg::Blocked => Lifecycle::Blocked,
            StateArg::Done => Lifecycle::Done,
            StateArg::Idle => Lifecycle::Idle,
        }
    }
}

/// Index of the subcommand in `args` (argv[0] first) when the first
/// non-global argument names one; `None` means "start the TUI".
pub fn subcommand_index(args: &[String]) -> Option<usize> {
    let mut i = 1;
    while i < args.len() {
        let arg = args[i].as_str();
        match arg {
            "--ptyd-path" | "--services-path" => i += 2,
            "-d" | "--debug" => i += 1,
            _ if arg.starts_with("--ptyd-path=") || arg.starts_with("--services-path=") => i += 1,
            _ => return SUBCOMMANDS.contains(&arg).then_some(i),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(line: &str) -> Vec<String> {
        std::iter::once("tui")
            .chain(line.split_whitespace())
            .map(str::to_owned)
            .collect()
    }

    fn parse(line: &str) -> CliArgs {
        CliArgs::try_parse_from(argv(line)).unwrap()
    }

    #[test]
    fn tui_invocations_are_not_subcommands() {
        for line in [
            "",
            "--debug",
            "-d --services-path /x/services.sh",
            "--help",
            "--version",
            "--new-tmux --cmd ls",
            "--ptyd-path /p",
            "something-else",
        ] {
            assert_eq!(subcommand_index(&argv(line)), None, "{line:?}");
        }
    }

    #[test]
    fn global_options_may_precede_the_subcommand() {
        assert_eq!(subcommand_index(&argv("ls")), Some(1));
        assert_eq!(
            subcommand_index(&argv("--services-path /s --ptyd-path /p -d ls")),
            Some(6)
        );
        assert_eq!(
            subcommand_index(&argv("--ptyd-path=/p session list")),
            Some(2)
        );
        let cli = parse("--ptyd-path /p --services-path /s --debug ls --json");
        assert_eq!(cli.ptyd_path, Some(PathBuf::from("/p")));
        assert!(cli.debug);
        assert_eq!(
            cli.command,
            Command::Ls(LsArgs {
                json: true,
                ..LsArgs::default()
            })
        );
        // Global options are accepted after the subcommand too.
        assert!(parse("ls --all -d").debug);
    }

    #[test]
    fn hooks_name_their_agent_and_optionally_the_event() {
        assert_eq!(
            parse("session hook --agent copilot --event agentStop").command,
            Command::Session {
                command: SessionCommand::Hook {
                    agent: HookAgent::Copilot,
                    event: Some("agentStop".into()),
                }
            }
        );
        for name in [
            "claude",
            "codex",
            "opencode",
            "copilot",
            "cursor-agent",
            "pi",
            "grok",
            "kimi",
            "droid",
            "amp",
        ] {
            let Command::Session {
                command: SessionCommand::Hook { agent, event },
            } = parse(&format!("session hook --agent {name}")).command
            else {
                panic!("{name}");
            };
            assert_eq!((agent.name(), event), (name, None));
        }
        assert!(CliArgs::try_parse_from(argv("session hook --agent gemini")).is_err());
        assert_eq!(
            parse("session hook-config --agent grok --install").command,
            Command::Session {
                command: SessionCommand::HookConfig {
                    agent: HookAgent::Grok,
                    install: true,
                }
            }
        );
    }

    #[test]
    fn ls_takes_folders_or_all() {
        assert_eq!(
            parse("ls --folder a --folder /b --dead").command,
            Command::Ls(LsArgs {
                folders: vec!["a".into(), "/b".into()],
                dead: true,
                ..LsArgs::default()
            })
        );
        // `--project` is a hidden alias, and the two mix.
        assert_eq!(
            parse("ls --project a --folder /b").command,
            Command::Ls(LsArgs {
                folders: vec!["a".into(), "/b".into()],
                ..LsArgs::default()
            })
        );
        assert_eq!(
            parse("ls -a").command,
            Command::Ls(LsArgs {
                all: true,
                ..LsArgs::default()
            })
        );
        for line in ["ls --all --folder a", "ls --all --project a"] {
            let error = CliArgs::try_parse_from(argv(line)).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{line}");
        }
    }

    #[test]
    fn help_says_folder_and_hides_the_project_aliases() {
        use clap::CommandFactory;
        let mut command = CliArgs::command();
        let help = command.render_long_help().to_string();
        assert!(help.contains("folders"), "{help}");
        assert!(!help.to_lowercase().contains("project"), "{help}");
        let ls = command
            .find_subcommand_mut("ls")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(ls.contains("--folder <DIR>"), "{ls}");
        assert!(ls.contains("List every folder"), "{ls}");
        assert!(!ls.to_lowercase().contains("project"), "{ls}");
        let folders = command
            .find_subcommand_mut("folders")
            .unwrap()
            .render_long_help()
            .to_string();
        assert!(!folders.contains("alias"), "{folders}");
        assert!(folders.contains(r#"{"folders": [...], "projects": [...]}"#));
    }

    #[test]
    fn folders_pin_and_unpin() {
        for word in ["folders", "projects"] {
            assert_eq!(subcommand_index(&argv(word)), Some(1));
            assert_eq!(
                parse(&format!("{word} --json")).command,
                Command::Folders {
                    command: None,
                    json: true
                },
                "{word}"
            );
            assert_eq!(
                parse(&format!("{word} add ../web")).command,
                Command::Folders {
                    command: Some(FoldersCommand::Add {
                        dir: "../web".into()
                    }),
                    json: false
                },
                "{word}"
            );
            for verb in ["rm", "remove"] {
                assert_eq!(
                    parse(&format!("{word} {verb} /old")).command,
                    Command::Folders {
                        command: Some(FoldersCommand::Rm { dir: "/old".into() }),
                        json: false
                    },
                    "{word} {verb}"
                );
            }
        }
        assert_eq!(
            parse("pin api").command,
            Command::Pin {
                target: "api".into()
            }
        );
        assert_eq!(
            parse("unpin 01ABC").command,
            Command::Unpin {
                target: "01ABC".into()
            }
        );
        for line in [
            "folders add",
            "folders --json add x",
            "projects add",
            "pin",
            "unpin a b",
        ] {
            let error = CliArgs::try_parse_from(argv(line)).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{line:?}");
        }
    }

    #[test]
    fn new_takes_options_and_a_trailing_argv() {
        let cli = parse("new --name web --cwd sub --cmd npm --cmd ls --detach -- bash -l");
        assert_eq!(
            cli.command,
            Command::New(NewArgs {
                name: Some("web".into()),
                cwd: Some("sub".into()),
                cmds: vec!["npm".into(), "ls".into()],
                detach: true,
                argv: vec!["bash".into(), "-l".into()],
                machine: None,
                sprite: false,
            })
        );
        assert_eq!(parse("new").command, Command::New(NewArgs::default()));
    }

    #[test]
    fn rename_takes_one_or_two_names() {
        assert_eq!(
            parse("rename build").command,
            Command::Rename {
                args: vec!["build".into()]
            }
        );
        assert_eq!(
            parse("rename old new").command,
            Command::Rename {
                args: vec!["old".into(), "new".into()]
            }
        );
        assert!(CliArgs::try_parse_from(argv("rename")).is_err());
        assert!(CliArgs::try_parse_from(argv("rename a b c")).is_err());
    }

    #[test]
    fn kill_rm_attach_and_daemon() {
        assert_eq!(
            parse("kill api --tree").command,
            Command::Kill {
                target: "api".into(),
                tree: true
            }
        );
        assert_eq!(
            parse("rm api").command,
            Command::Rm {
                target: "api".into()
            }
        );
        assert_eq!(
            parse("attach 01ABC").command,
            Command::Attach {
                target: "01ABC".into()
            }
        );
        assert_eq!(
            parse("daemon logs -n 5 -f").command,
            Command::Daemon {
                command: DaemonCommand::Logs {
                    lines: 5,
                    follow: true
                }
            }
        );
        assert_eq!(
            parse("daemon stop --yes").command,
            Command::Daemon {
                command: DaemonCommand::Stop { yes: true }
            }
        );
    }

    #[test]
    fn remote_commands_parse() {
        assert_eq!(subcommand_index(&argv("remote on")), Some(1));
        assert_eq!(
            parse("remote on --port 7400").command,
            Command::Remote {
                command: RemoteCommand::On { port: Some(7400) }
            }
        );
        assert_eq!(
            parse("remote on").command,
            Command::Remote {
                command: RemoteCommand::On { port: None }
            }
        );
        assert_eq!(
            parse("remote off").command,
            Command::Remote {
                command: RemoteCommand::Off
            }
        );
        assert_eq!(
            parse("remote status --json").command,
            Command::Remote {
                command: RemoteCommand::Status { json: true }
            }
        );
        assert_eq!(
            parse("remote token --rotate").command,
            Command::Remote {
                command: RemoteCommand::Token { rotate: true }
            }
        );
        for line in [
            "remote",
            "remote on --port 0",
            "remote on --port x",
            "remote up",
        ] {
            let error = CliArgs::try_parse_from(argv(line)).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{line:?}");
        }
    }

    #[test]
    fn session_commands_parse() {
        assert_eq!(
            parse("session read api --source recent-unwrapped --lines 120 --format ansi").command,
            Command::Session {
                command: SessionCommand::Read {
                    target: "api".into(),
                    source: SourceArg::RecentUnwrapped,
                    lines: Some(120),
                    format: FormatArg::Ansi,
                }
            }
        );
        assert!(matches!(
            parse("session read api --source recent_unwrapped").command,
            Command::Session {
                command: SessionCommand::Read {
                    source: SourceArg::RecentUnwrapped,
                    ..
                }
            }
        ));
        assert_eq!(
            parse("session wait api --until output --pattern ok --timeout 5").command,
            Command::Session {
                command: SessionCommand::Wait {
                    target: "api".into(),
                    until: UntilArg::Output,
                    pattern: Some("ok".into()),
                    timeout_ms: 5,
                }
            }
        );
        let report = parse(
            "session report --state done --text t --summary s --file a.rs --file b.rs \
             --agent-kind claude --agent-session-id X --resume -- claude --resume X",
        );
        assert_eq!(
            report.command,
            Command::Session {
                command: SessionCommand::Report(ReportArgs {
                    state: Some(StateArg::Done),
                    text: Some("t".into()),
                    summary: Some("s".into()),
                    files: vec!["a.rs".into(), "b.rs".into()],
                    agent_kind: Some("claude".into()),
                    agent_session_id: Some("X".into()),
                    agent_session_path: None,
                    resume: true,
                    resume_argv: vec!["claude".into(), "--resume".into(), "X".into()],
                })
            }
        );
    }

    #[test]
    fn usage_errors_exit_with_2() {
        for line in [
            "kill",
            "session wait api",
            "session wait api --until never",
            "session report --file a.rs",
            "session report --agent-session-id X",
            "session report --state finished",
            "ls --bogus",
            "daemon",
        ] {
            let error = CliArgs::try_parse_from(argv(line)).unwrap_err();
            assert_eq!(error.exit_code(), 2, "{line:?}");
        }
        assert_eq!(
            CliArgs::try_parse_from(argv("help"))
                .unwrap_err()
                .exit_code(),
            0
        );
    }
}
