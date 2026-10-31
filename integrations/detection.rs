//! Agent detection: which coding agent runs in a terminal and what it is
//! doing, from the foreground process and the visible screen. Pure functions;
//! the control layer combines the result with self-reports and hook events.
//!
//! **Which agent.** The foreground process's name and command line are
//! matched against known executables (`claude`, `codex`, …) and package
//! paths (`node_modules/@anthropic-ai/claude-code/…`), seeing through
//! interpreters and launchers (`node …/codex.js`, `bun …/opencode`,
//! `python -m aider`, `npx @openai/codex`, `sh -c 'exec claude'`). Then
//! the file it runs, matched the same way, for programs that renamed
//! themselves (Cursor's bundled Node shows as `MainThread`, Kimi Code as
//! `kimi-code`). The bare name `agent`, which both Cursor and Grok install,
//! is never an agent by itself: only the file behind it says which. The
//! terminal title is used only when there is no process information or the
//! foreground is an opaque wrapper (an unrecognised script under a runtime,
//! `ssh`, `docker`, …). Shells and other programs mean no agent, even if a
//! stale agent title is still set.
//!
//! **What it's doing.** Each agent has markers in regions of the visible
//! screen (mostly its bottom, where the live UI is) and the title. They are
//! checked in order of strength: a visible prompt that needs the user
//! (`Blocked`) beats a busy indicator (`Working`), which beats the agent's
//! idle input chrome (`Idle`). With no marker, recent output counts as
//! `Working` for agents whose busy output has no marker (aider streaming);
//! otherwise the agent's quiet state applies: `Unknown` where the idle
//! chrome is reliable (so its absence means an unrecognised screen), `Idle`
//! where idleness is just the absence of busy markers.
//!
//! Marker evidence comes from screens captured from the agents and from the
//! agents' own UI strings; fixtures live in `tests/fixtures/detect/`.

use crate::host::ProcessInfo;
use session_core::model::Lifecycle;

#[path = "amp/detection.rs"]
mod amp;
#[path = "claude-code/detection.rs"]
mod claude;
#[path = "codex/detection.rs"]
mod codex;
#[path = "copilot/detection.rs"]
mod copilot;
#[path = "cursor-agent/detection.rs"]
mod cursor;
#[path = "droid/detection.rs"]
mod droid;
#[path = "grok/detection.rs"]
mod grok;
#[path = "kimi/detection.rs"]
mod kimi;
#[path = "open-code/detection.rs"]
mod opencode;
#[path = "pi/detection.rs"]
mod pi;
use amp::PROFILE as AMP;
use claude::PROFILE as CLAUDE;
#[cfg(test)]
use claude::{is_claude_activity_line, is_numbered_yes_option};
use codex::PROFILE as CODEX;
use copilot::PROFILE as COPILOT;
#[cfg(test)]
use cursor::is_cursor_activity_line;
use cursor::PROFILE as CURSOR_AGENT;
use droid::PROFILE as DROID;
use grok::PROFILE as GROK;
use kimi::PROFILE as KIMI;
#[cfg(test)]
use kimi::{has_kimi_context_usage, is_kimi_activity_line};
#[cfg(test)]
use opencode::has_opencode_progress_bar;
use opencode::PROFILE as OPENCODE;
use pi::PROFILE as PI;
#[cfg(test)]
use pi::{has_pi_context_usage, is_pi_loader_line, is_pi_rule_loader};

#[derive(Debug, Clone, Copy)]
pub struct DetectInput<'a> {
    /// Visible screen text (plain).
    pub screen: &'a str,
    pub foreground: Option<&'a ProcessInfo>,
    pub alternate_screen: bool,
    pub title: &'a str,
    /// Milliseconds since the last output, if any output happened.
    pub ms_since_output: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    /// "claude", "codex", "opencode", "pi", "gemini", … or None for no agent.
    pub agent: Option<String>,
    /// `Lifecycle::None` when no agent; `Unknown` when an agent is running
    /// but its state isn't recognisable. Never `Done` (the control layer
    /// derives `Done` from Working → Idle transitions).
    pub lifecycle: Lifecycle,
}

/// Output this recent counts as activity. Below the host's 500 ms settle
/// delay, so the trailing `HostEvent::Output` always sees a settled screen.
const RECENT_OUTPUT_MS: u64 = 400;

pub fn detect(input: &DetectInput<'_>) -> Detection {
    let Some(profile) = identify(input.foreground, input.title) else {
        return Detection {
            agent: None,
            lifecycle: Lifecycle::None,
        };
    };
    Detection {
        agent: Some(profile.label.to_string()),
        lifecycle: classify(profile, input),
    }
}

// ---------------------------------------------------------------------------
// Agent profiles
// ---------------------------------------------------------------------------

/// What Rakumux knows about one agent.
struct Profile {
    /// Reported agent name.
    label: &'static str,
    /// Executable or script basenames (lowercase, extension stripped).
    names: &'static [&'static str],
    /// Consecutive path components that identify the agent's package, e.g.
    /// `["@anthropic-ai", "claude-code"]` (lowercase).
    packages: &'static [&'static [&'static str]],
    /// Recognises the agent from a terminal title alone.
    title_hint: fn(&str) -> bool,
    blocked: &'static [Marker],
    working: &'static [Marker],
    idle: &'static [Marker],
    /// State when no marker matches and output has settled.
    quiet: Lifecycle,
    /// Recent output without any marker means `Working`.
    output_means_working: bool,
    /// Its hooks report no approval prompts (Cursor has no hook for them), so
    /// one its screen shows (`Blocked`) overrides a hook's busy phase.
    approvals_on_screen: bool,
}

/// Evidence for a state: a test applied to a region.
struct Marker {
    region: Region,
    test: Test,
}

#[derive(Debug, Clone, Copy)]
enum Region {
    /// The whole visible screen.
    Screen,
    /// From the n-th last non-empty line to the bottom.
    Bottom(usize),
    /// Lines below the last horizontal rule (`───…`); the bottom 12
    /// non-empty lines when there is no rule.
    AfterLastRule,
    /// Lines between the last two horizontal rules (an input box drawn with
    /// rules above and below); empty with fewer than two rules.
    PromptBox,
    /// The terminal title, as one line.
    Title,
}

/// Needles are lowercase and matched case-insensitively against the
/// region's lines joined with spaces, so a phrase the program wrapped onto
/// two lines still matches.
enum Test {
    /// Every needle occurs.
    All(&'static [&'static str]),
    /// Some needle occurs.
    Any(&'static [&'static str]),
    /// Some line, trimmed, satisfies the predicate.
    Line(fn(&str) -> bool),
    /// Both tests hold.
    And(&'static Test, &'static Test),
}

const fn marker(region: Region, test: Test) -> Marker {
    Marker { region, test }
}

static PROFILES: &[Profile] = &[
    CLAUDE,
    CODEX,
    OPENCODE,
    PI,
    GEMINI,
    QWEN,
    AIDER,
    CURSOR_AGENT,
    COPILOT,
    GROK,
    KIMI,
    AMP,
    DROID,
    generic("crush", &["crush"], &[&["@charmland", "crush"]]),
    generic("kiro", &["kiro-cli"], &[]),
    generic("cline", &["cline"], &[]),
    generic("auggie", &["auggie"], &[&["@augmentcode", "auggie"]]),
    generic("letta", &["letta"], &[&["@letta-ai", "letta-code"]]),
    generic("devin", &["devin"], &[]),
    generic("qodercli", &["qodercli"], &[]),
];

const GEMINI_BLOCKED: &[Marker] = &[marker(
    Region::Bottom(25),
    Test::Any(&[
        "apply this change?",
        "allow execution",
        "waiting for user confirmation",
        "do you want to proceed",
        "no, suggest changes",
    ]),
)];
const GEMINI_WORKING: &[Marker] = &[marker(Region::Bottom(12), Test::Any(&["esc to cancel"]))];
const GEMINI_IDLE: &[Marker] = &[marker(
    Region::Bottom(12),
    Test::Any(&["type your message"]),
)];

/// Gemini CLI (busy line `… (esc to cancel, 12s)`, tool confirmations
/// "Allow execution of …?" / "Apply this change?", input placeholder "Type
/// your message or @path/to/file").
const GEMINI: Profile = Profile {
    label: "gemini",
    names: &["gemini"],
    packages: &[&["@google", "gemini-cli"]],
    title_hint: |title| has_word(title, "gemini"),
    blocked: GEMINI_BLOCKED,
    working: GEMINI_WORKING,
    idle: GEMINI_IDLE,
    quiet: Lifecycle::Idle,
    output_means_working: false,
    approvals_on_screen: false,
};

/// Qwen Code, a Gemini CLI fork with the same UI.
const QWEN: Profile = Profile {
    label: "qwen",
    names: &["qwen", "qwen-code"],
    packages: &[&["@qwen-code", "qwen-code"]],
    title_hint: |title| has_word(title, "qwen"),
    ..GEMINI
};

/// Aider (prompt `> ` / `architect> `, confirmations ending in
/// `(Y)es/(N)o … [Yes]:`, `Waiting for <model>` spinner). Streamed replies
/// carry no marker, so recent output means working.
const AIDER: Profile = Profile {
    label: "aider",
    names: &["aider"],
    packages: &[&["aider-chat"]],
    title_hint: |title| has_word(title, "aider"),
    blocked: &[marker(Region::Bottom(2), Test::Any(&["(y)es/(n)o"]))],
    working: &[marker(Region::Bottom(2), Test::Any(&["waiting for "]))],
    idle: &[marker(Region::Bottom(1), Test::Line(is_aider_prompt))],
    quiet: Lifecycle::Unknown,
    output_means_working: true,
    approvals_on_screen: false,
};

const GENERIC_BLOCKED: &[Marker] = &[
    marker(
        Region::Bottom(15),
        Test::Any(&[
            "do you want to proceed",
            "waiting for approval",
            "permission required",
            "allow once",
        ]),
    ),
    // Weak: command output can say this too, so only as the last thing.
    marker(Region::Bottom(2), Test::Any(&["[y/n]", "(y/n)"])),
];
const GENERIC_WORKING: &[Marker] = &[
    marker(Region::Title, Test::Line(has_braille_spinner_word)),
    marker(
        Region::Bottom(10),
        Test::Any(&[
            "esc to interrupt",
            "ctrl+c to interrupt",
            "esc interrupt",
            "esc to stop",
            "ctrl+c to stop",
        ]),
    ),
];

/// Agents recognised by name whose screens we have no specific evidence
/// for: common approval and busy phrasing only.
const fn generic(
    label: &'static str,
    names: &'static [&'static str],
    packages: &'static [&'static [&'static str]],
) -> Profile {
    Profile {
        label,
        names,
        packages,
        title_hint: |_| false,
        blocked: GENERIC_BLOCKED,
        working: GENERIC_WORKING,
        idle: &[],
        quiet: Lifecycle::Unknown,
        output_means_working: true,
        approvals_on_screen: false,
    }
}

// ---------------------------------------------------------------------------
// Line predicates
// ---------------------------------------------------------------------------

fn is_braille(c: char) -> bool {
    ('\u{2800}'..='\u{28FF}').contains(&c)
}

/// `glyph text`: the first char and the trimmed rest, when a space follows.
fn split_glyph(line: &str) -> Option<(char, &str)> {
    let mut chars = line.chars();
    let glyph = chars.next()?;
    let rest = chars.as_str();
    rest.starts_with(char::is_whitespace)
        .then(|| (glyph, rest.trim_start()))
        .filter(|(_, rest)| !rest.is_empty())
}

/// A braille spinner frame as a separate word anywhere in the line.
fn has_braille_spinner_word(line: &str) -> bool {
    line.split_whitespace().any(|word| {
        let mut chars = word.chars();
        matches!((chars.next(), chars.next()), (Some(c), None) if is_braille(c) && c != '\u{2800}')
    })
}

/// Aider's input prompt: `>`, `architect>`, `ask> text`.
fn is_aider_prompt(line: &str) -> bool {
    let first = line.split_whitespace().next().unwrap_or("");
    first.strip_suffix('>').is_some_and(|mode| {
        mode.chars()
            .all(|c| c.is_ascii_lowercase() || c == '-' || c == '_')
    })
}

fn contains_ci(haystack: &str, lowercase_needle: &str) -> bool {
    haystack.to_lowercase().contains(lowercase_needle)
}

/// `word` (lowercase) occurs bounded by non-alphanumeric characters.
fn has_word(text: &str, word: &str) -> bool {
    let lower = text.to_lowercase();
    lower.match_indices(word).any(|(at, _)| {
        let before = lower[..at].chars().next_back();
        let after = lower[at + word.len()..].chars().next();
        !before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
    })
}

// ---------------------------------------------------------------------------
// Identification
// ---------------------------------------------------------------------------

/// Programs that run other programs: when what they run isn't an agent we
/// recognise, the title may still say which agent is behind them.
const OPAQUE_WRAPPERS: &[&str] = &[
    "node",
    "nodejs",
    "bun",
    "deno",
    "npx",
    "npm",
    "pnpm",
    "bunx",
    "yarn",
    "uv",
    "uvx",
    "pipx",
    "ssh",
    "mosh",
    "mosh-client",
    "et",
    "docker",
    "podman",
    "kubectl",
    "env",
    "nix",
];

const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "xonsh", "elvish",
];

/// An interactive shell (`bash`, `zsh -l`, …): one that isn't running a
/// script or a `-c` command.
pub fn is_shell(process: &ProcessInfo) -> bool {
    let argv0 = process.cmdline.first().map(String::as_str).unwrap_or("");
    let shell = [process.name.as_str(), argv0]
        .into_iter()
        .any(|name| is_shell_command(&[name.to_string()]));
    shell && is_shell_command(&process.cmdline)
}

/// `argv` starts an interactive shell (`bash`, `/bin/zsh -l`; not
/// `bash script.sh` or `sh -c …`).
pub fn is_shell_command(argv: &[String]) -> bool {
    let Some(program) = argv.first() else {
        return false;
    };
    SHELLS.contains(&normalized_basename(program).as_str())
        && argv[1..]
            .iter()
            .all(|arg| arg.starts_with('-') && arg != "-c")
}

/// Whether `agent`'s (a detected label) approval prompts show only on its
/// screen: a detected `Blocked` then overrides its hooks' busy phase.
pub fn approvals_on_screen(agent: &str) -> bool {
    PROFILES
        .iter()
        .any(|profile| profile.label == agent && profile.approvals_on_screen)
}

/// Whether `agent`'s (a detected label) screen tells its waiting for input
/// from anything else: its profile knows its prompt, and a screen without any
/// of its markers isn't taken for waiting (`quiet` is `Unknown`). Detection
/// reading `Unknown` for it then means its screen shows no prompt: it's still
/// starting (its hooks may say idle already), or shows a view or menu of its
/// own, where what's typed doesn't go in as a prompt.
pub fn knows_its_prompt(agent: &str) -> bool {
    PROFILES.iter().any(|profile| {
        profile.label == agent && !profile.idle.is_empty() && profile.quiet == Lifecycle::Unknown
    })
}

/// The coding agent `process` is ("claude", "codex", …), if Rakumux knows
/// it.
pub fn agent_label(process: &ProcessInfo) -> Option<&'static str> {
    match profile_for_process(process) {
        ProcessMatch::Agent(profile) => Some(profile.label),
        ProcessMatch::Opaque | ProcessMatch::Other => None,
    }
}

fn identify(foreground: Option<&ProcessInfo>, title: &str) -> Option<&'static Profile> {
    match foreground {
        None => profile_for_title(title),
        Some(process) => match profile_for_process(process) {
            ProcessMatch::Agent(profile) => Some(profile),
            ProcessMatch::Opaque => profile_for_title(title),
            ProcessMatch::Other => None,
        },
    }
}

enum ProcessMatch {
    Agent(&'static Profile),
    /// A runtime or wrapper running something unrecognised.
    Opaque,
    /// A shell or any other program.
    Other,
}

fn profile_for_process(process: &ProcessInfo) -> ProcessMatch {
    let argv0 = process.cmdline.first().map(String::as_str).unwrap_or("");
    // Programs that rewrote their argv into one string ("claude --resume").
    let argv0_word = if argv0.contains('/') {
        argv0
    } else {
        argv0.split_whitespace().next().unwrap_or("")
    };
    for candidate in [process.name.as_str(), argv0_word] {
        if let Some(profile) = profile_for_token(candidate) {
            return ProcessMatch::Agent(profile);
        }
    }
    let runtime = [process.name.as_str(), argv0_word]
        .into_iter()
        .map(normalized_basename)
        .find(|name| is_runtime(name) || SHELLS.contains(&name.as_str()));
    if let Some(runtime) = runtime {
        let args = process.cmdline.get(1..).unwrap_or(&[]);
        if let Some(profile) = launched_target(&runtime, args).and_then(profile_for_token) {
            return ProcessMatch::Agent(profile);
        }
        if SHELLS.contains(&runtime.as_str()) {
            return ProcessMatch::Other;
        }
        return ProcessMatch::Opaque;
    }
    // Neither its name nor a runtime's target: the file it runs, for an agent
    // that renamed itself or runs under a shared name (never a runtime's own
    // file, which may be an agent's bundled Node or Python running something
    // else).
    if let Some(profile) = process
        .exe
        .as_deref()
        .and_then(|exe| exe.to_str())
        .and_then(profile_for_token)
    {
        return ProcessMatch::Agent(profile);
    }
    let name = normalized_basename(&process.name);
    if OPAQUE_WRAPPERS.contains(&name.as_str()) {
        ProcessMatch::Opaque
    } else {
        ProcessMatch::Other
    }
}

fn is_runtime(name: &str) -> bool {
    OPAQUE_WRAPPERS.contains(&name) || is_python(name)
}

fn is_python(name: &str) -> bool {
    let rest = name
        .strip_prefix("python")
        .or_else(|| name.strip_prefix("pypy"));
    rest.is_some_and(|version| version.chars().all(|c| c.is_ascii_digit() || c == '.'))
}

/// The script, module, package or command a runtime, launcher or `sh -c`
/// was asked to run.
fn launched_target<'a>(runtime: &str, args: &'a [String]) -> Option<&'a str> {
    if SHELLS.contains(&runtime) {
        // macOS exposes a shebang script as its interpreter plus script
        // path, rather than naming the process after the script as Linux does.
        let mut words = args.iter().map(String::as_str);
        while let Some(word) = words.next() {
            match word {
                "-c" | "-lc" | "-ic" => return first_command_word(words.next()?),
                "--" => return words.next(),
                "-" | "-s" | "--stdin" => return None,
                "--rcfile" | "--init-file" | "-o" | "+o" | "-O" | "+O" => {
                    words.next();
                }
                flag if flag.starts_with('-') || flag.starts_with('+') => {}
                script => return Some(script),
            }
        }
        return None;
    }
    let mut iter = args.iter().map(String::as_str);
    while let Some(arg) = iter.next() {
        match arg {
            // Inline code: nothing to name.
            "-e" | "--eval" | "-p" | "--print" | "-c" => return None,
            // Python modules: `python -m aider`.
            "-m" => return iter.next(),
            // Flags whose value is a separate argument.
            "-r"
            | "--require"
            | "--import"
            | "--loader"
            | "--experimental-loader"
            | "--inspect-port"
            | "--title"
            | "-W"
            | "-X"
            | "--from"
            | "--with"
            | "--package" => {
                iter.next();
            }
            // Launcher subcommands: `deno run`, `uv run`, `npm exec`, `pnpm dlx`.
            "run" | "exec" | "dlx" | "x" | "tool" => {}
            "--" => return iter.next(),
            flag if flag.starts_with('-') => {}
            // `env FOO=1 claude`.
            assignment if is_env_assignment(assignment) => {}
            target => return Some(target),
        }
    }
    None
}

/// First word of a shell command, skipping `VAR=value`, `exec`, `env`,
/// `command` and `nohup`.
fn first_command_word(command: &str) -> Option<&str> {
    command
        .split_whitespace()
        .map(|word| word.trim_matches(|c| c == '"' || c == '\''))
        .find(|word| {
            !is_env_assignment(word) && !matches!(*word, "exec" | "env" | "command" | "nohup" | "")
        })
}

/// `NAME=value` with a shell-identifier name.
fn is_env_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    })
}

/// Lowercase basename with a script/executable extension removed.
fn normalized_basename(token: &str) -> String {
    let token = token.trim_matches(|c| c == '"' || c == '\'');
    let base = token.rsplit('/').next().unwrap_or(token);
    let base = base.strip_prefix('-').unwrap_or(base).to_lowercase();
    for ext in [".js", ".mjs", ".cjs", ".ts", ".py", ".exe", ".cmd", ".sh"] {
        if let Some(stem) = base.strip_suffix(ext) {
            return stem.to_string();
        }
    }
    base
}

/// Agent for a path, executable name or package spec
/// (`@openai/codex@latest`, `opencode-ai@1.2`).
fn profile_for_token(token: &str) -> Option<&'static Profile> {
    let token = token.trim_matches(|c| c == '"' || c == '\'');
    if token.is_empty() || token.starts_with('-') {
        return None;
    }
    let spec = strip_package_version(token);
    let components: Vec<String> = spec
        .split('/')
        .filter(|c| !c.is_empty())
        .map(str::to_lowercase)
        .collect();
    let base = normalized_basename(token);
    PROFILES.iter().find(|profile| {
        profile.names.contains(&base.as_str())
            || profile.packages.iter().any(|package| {
                components
                    .windows(package.len())
                    .any(|window| window.iter().zip(package.iter()).all(|(a, b)| a == b))
            })
    })
}

/// `@scope/name@1.2` → `@scope/name`; `name@latest` → `name`. Anything
/// else with a `/` is a path and left alone.
fn strip_package_version(token: &str) -> &str {
    let scoped = token.starts_with('@');
    let slashes = token.matches('/').count();
    if slashes > usize::from(scoped) {
        return token;
    }
    let search_from = usize::from(scoped);
    match token[search_from..].find('@') {
        Some(at) => &token[..search_from + at],
        None => token,
    }
}

fn profile_for_title(title: &str) -> Option<&'static Profile> {
    let title = title.trim();
    if title.is_empty() {
        return None;
    }
    PROFILES.iter().find(|profile| (profile.title_hint)(title))
}

// ---------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------

fn classify(profile: &Profile, input: &DetectInput<'_>) -> Lifecycle {
    let view = ScreenView::new(input.screen, input.title);
    let any = |markers: &[Marker]| markers.iter().any(|m| view.matches(m));
    if any(profile.blocked) {
        Lifecycle::Blocked
    } else if any(profile.working) {
        Lifecycle::Working
    } else if any(profile.idle) {
        Lifecycle::Idle
    } else if profile.output_means_working
        && input
            .ms_since_output
            .is_some_and(|ms| ms < RECENT_OUTPUT_MS)
    {
        Lifecycle::Working
    } else {
        profile.quiet
    }
}

/// The screen as trimmed lines (original and lowercase) plus the title.
struct ScreenView<'a> {
    lines: Vec<&'a str>,
    lower: Vec<String>,
    title: [&'a str; 1],
    title_lower: [String; 1],
}

impl<'a> ScreenView<'a> {
    fn new(screen: &'a str, title: &'a str) -> Self {
        let mut lines: Vec<&str> = screen.lines().map(str::trim_end).collect();
        while lines.last().is_some_and(|l| l.is_empty()) {
            lines.pop();
        }
        let lower = lines.iter().map(|l| l.to_lowercase()).collect();
        let title = title.trim();
        Self {
            lines,
            lower,
            title: [title],
            title_lower: [title.to_lowercase()],
        }
    }

    fn matches(&self, marker: &Marker) -> bool {
        let (lines, lower): (&[&str], &[String]) = match marker.region {
            Region::Title => (&self.title, &self.title_lower),
            region => {
                let range = self.range(region);
                (&self.lines[range.clone()], &self.lower[range])
            }
        };
        // Phrases may be wrapped by the program itself ("Would you like to
        // ⏎ update now?"), so needles are matched against the region's
        // lines joined with single spaces.
        let joined = lower
            .iter()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        test_matches(&marker.test, lines, &joined)
    }

    fn range(&self, region: Region) -> std::ops::Range<usize> {
        let end = self.lines.len();
        match region {
            Region::Screen => 0..end,
            Region::Bottom(count) => self.bottom_start(count)..end,
            Region::AfterLastRule => match self.rules().last() {
                Some(&rule) => rule + 1..end,
                None => self.bottom_start(12)..end,
            },
            Region::PromptBox => {
                let rules = self.rules();
                match rules.as_slice() {
                    [.., top, bottom] => top + 1..*bottom,
                    _ => 0..0,
                }
            }
            Region::Title => 0..0,
        }
    }

    fn bottom_start(&self, count: usize) -> usize {
        self.lines
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, line)| !line.trim().is_empty())
            .take(count)
            .last()
            .map_or(self.lines.len(), |(index, _)| index)
    }

    fn rules(&self) -> Vec<usize> {
        self.lines
            .iter()
            .enumerate()
            .filter(|(_, line)| line.trim_start().starts_with("───"))
            .map(|(index, _)| index)
            .collect()
    }
}

/// `joined` is the region's lowercase text, lines trimmed and joined with
/// spaces; `lines` are the original lines for line predicates.
fn test_matches(test: &Test, lines: &[&str], joined: &str) -> bool {
    let occurs = |needle: &&str| joined.contains(*needle);
    match test {
        Test::All(needles) => needles.iter().all(occurs),
        Test::Any(needles) => needles.iter().any(occurs),
        Test::Line(predicate) => lines.iter().any(|line| predicate(line.trim())),
        Test::And(a, b) => test_matches(a, lines, joined) && test_matches(b, lines, joined),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn process(name: &str, cmdline: &[&str]) -> ProcessInfo {
        ProcessInfo {
            pid: 4242,
            name: name.to_string(),
            cmdline: cmdline.iter().map(|s| s.to_string()).collect(),
            exe: None,
        }
    }

    fn agent_of(name: &str, cmdline: &[&str]) -> Option<&'static str> {
        identify(Some(&process(name, cmdline)), "").map(|p| p.label)
    }

    fn agent_running(name: &str, cmdline: &[&str], exe: &str) -> Option<&'static str> {
        let process = ProcessInfo {
            exe: Some(PathBuf::from(exe)),
            ..process(name, cmdline)
        };
        identify(Some(&process), "").map(|p| p.label)
    }

    #[test]
    fn renamed_agents_are_named_by_the_file_they_run() {
        let versions = "/home/me/.local/share/cursor-agent/versions/2026.10.22-27dcf5f";
        let script = format!("{versions}/index.js");
        let node = format!("{versions}/node");
        // Cursor started as `agent`: Node 24 renames its main thread on Linux,
        // and macOS names it after argv[0].
        for name in ["MainThread", "agent"] {
            let argv = ["/home/me/.local/bin/agent", "--use-system-ca", &script];
            assert_eq!(agent_of(name, &argv), None, "{name} alone says nothing");
            assert_eq!(
                agent_running(name, &argv, &node),
                Some("cursor-agent"),
                "{name}"
            );
        }
        // Kimi Code's native binary renames itself and wipes its arguments,
        // cut to fit (`kimi-co`): its file still says which agent it is.
        assert_eq!(agent_of("kimi-co", &["kimi-co"]), None);
        assert_eq!(
            agent_running("kimi-co", &["kimi-co"], "/home/me/.kimi-code/bin/kimi"),
            Some("kimi")
        );
        assert_eq!(
            agent_running("kimi-code", &["kimi-code"], "/home/me/.kimi-code/bin/kimi"),
            Some("kimi")
        );
        // A name or command line that says which agent still wins.
        assert_eq!(
            agent_running(
                "claude",
                &["claude"],
                "/home/me/.local/share/claude/versions/2.1.0"
            ),
            Some("claude")
        );
        // Programs whose file says nothing stay what they were.
        assert_eq!(agent_running("bash", &["bash"], "/usr/bin/bash"), None);
        assert_eq!(
            agent_running("node", &["node", "server.js"], "/usr/bin/node"),
            None
        );
        assert_eq!(
            agent_running(
                "node",
                &["node", "/opt/codex/bin/codex.js"],
                "/usr/bin/node"
            ),
            Some("codex")
        );
        // A runtime is named by what it runs, never by its own file.
        assert_eq!(
            agent_running(
                "python3",
                &["python3", "report.py"],
                "/home/me/.local/pipx/venvs/aider-chat/bin/python3"
            ),
            None
        );
    }

    #[test]
    fn kimi_code_is_named_whatever_its_title_was_cut_to() {
        let exe = "/home/me/.kimi-code/bin/kimi";
        // `process.title = "kimi-code"` fits the room its arguments had:
        // bare `kimi` stays `kimi`, `kimi -c` shows as `kimi-co`.
        for name in ["kimi", "kimi-c", "kimi-co", "kimi-cod", "kimi-code"] {
            assert_eq!(agent_running(name, &[name], exe), Some("kimi"), "{name}");
        }
        assert_eq!(agent_of("kimi", &["kimi"]), Some("kimi"));
        assert_eq!(agent_of("kimi-code", &["kimi-code"]), Some("kimi"));
        // An npm install: before the title, and after it (it wipes argv).
        assert_eq!(
            agent_of(
                "node",
                &[
                    "node",
                    "/usr/lib/node_modules/@moonshot-ai/kimi-code/dist/main.mjs"
                ]
            ),
            Some("kimi")
        );
        assert_eq!(
            agent_running("kimi-code", &["kimi-code"], "/usr/bin/node"),
            Some("kimi")
        );
        // The archived Python CLI: its title, its renamed shim, a module.
        assert_eq!(agent_of("Kimi Code", &["Kimi Code"]), Some("kimi"));
        assert_eq!(agent_of("kimi-legacy", &["kimi-legacy"]), Some("kimi"));
        assert_eq!(
            agent_of("python3", &["python3", "-m", "kimi_cli"]),
            Some("kimi")
        );
        // Kimi Code 2.x titles the terminal with the chat's title.
        assert_eq!(identify(None, "Kimi Code").map(|p| p.label), Some("kimi"));
        assert_eq!(identify(None, "fix the parser").map(|p| p.label), None);
    }

    #[test]
    fn kimi_activity_and_context_lines() {
        for line in [
            "⠸ Working… · Tip: /web: use the Web UI for a better experience",
            "⠋ Working…",
            "⠙ Compacting…",
            "🌕 · Tip: ctrl-s to add guidance without waiting for the turn to finish",
            "🌑",
        ] {
            assert!(is_kimi_activity_line(line), "{line}");
        }
        for line in [
            "● Working… on it",
            "⠸ done",
            "🌕 full moon tonight",
            "Working…",
            "",
        ] {
            assert!(!is_kimi_activity_line(line), "{line}");
        }
        assert!(has_kimi_context_usage("context: 0% (0/256k)"));
        assert!(has_kimi_context_usage("context: 12.5% (32k/256k)"));
        assert!(!has_kimi_context_usage("context: high"));
        assert!(!has_kimi_context_usage("the context: 5% (x)"));
    }

    #[test]
    fn only_agents_whose_hooks_miss_approvals_let_the_screen_say_so() {
        assert!(approvals_on_screen("cursor-agent"));
        for agent in ["claude", "codex", "opencode", "pi", "gemini", "unknown"] {
            assert!(!approvals_on_screen(agent), "{agent}");
        }
    }

    #[test]
    fn no_agent_claims_the_bare_name_agent() {
        // Both Cursor and Grok install a command named `agent`.
        assert!(PROFILES
            .iter()
            .all(|profile| !profile.names.contains(&"agent")));
        assert_eq!(agent_of("agent", &["agent"]), None);
        assert_eq!(agent_of("agent", &["/home/me/.local/bin/agent"]), None);
    }

    #[test]
    fn agents_are_named_by_executable() {
        assert_eq!(agent_of("claude", &["claude"]), Some("claude"));
        assert_eq!(
            agent_of("claude", &["/home/me/.local/bin/claude", "--resume"]),
            Some("claude")
        );
        assert_eq!(agent_of("opencode", &["opencode"]), Some("opencode"));
        // OpenCode V2's npm package (`@opencode/cli`) also installs `opencode2`.
        assert_eq!(agent_of("opencode2", &["opencode2"]), Some("opencode"));
        assert_eq!(
            agent_of(
                "opencode.exe",
                &["/usr/lib/node_modules/@opencode/cli/bin/opencode.exe"]
            ),
            Some("opencode")
        );
        assert_eq!(
            agent_of("cursor-agent", &["cursor-agent"]),
            Some("cursor-agent")
        );
        // Node's process.title rewrites argv into one string.
        assert_eq!(agent_of("node", &["claude --resume abc"]), Some("claude"));
    }

    #[test]
    fn interpreters_and_launchers_are_seen_through() {
        let cases: &[(&str, &[&str], &str)] = &[
            (
                "node",
                &["node", "/home/me/.nvm/versions/node/v22/bin/codex"],
                "codex",
            ),
            (
                "node",
                &[
                    "/usr/bin/node",
                    "--no-warnings",
                    "/usr/lib/node_modules/@openai/codex/bin/codex.js",
                ],
                "codex",
            ),
            (
                "node",
                &[
                    "node",
                    "/usr/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                ],
                "claude",
            ),
            (
                "node",
                &[
                    "node",
                    "-r",
                    "/tmp/hook.js",
                    "/opt/node_modules/@google/gemini-cli/dist/index.js",
                ],
                "gemini",
            ),
            ("bun", &["bun", "/home/me/.bun/bin/opencode"], "opencode"),
            ("bun", &["bun", "run", "opencode"], "opencode"),
            ("python3", &["/usr/bin/python3", "-m", "aider"], "aider"),
            (
                "python3.12",
                &["/usr/bin/python3.12", "/home/me/.local/bin/aider", "--model", "x"],
                "aider",
            ),
            ("npm exec @opena", &["npx", "@openai/codex@latest"], "codex"),
            ("npx", &["npx", "-y", "opencode-ai@1.2.3"], "opencode"),
            ("uvx", &["uvx", "--from", "aider-chat", "aider"], "aider"),
            ("sh", &["sh", "-c", "exec claude --continue"], "claude"),
            (
                "sh",
                &["/bin/sh", "/var/folders/tmp/bin/claude", "--model", "opus"],
                "claude",
            ),
            (
                "bash",
                &["bash", "-e", "/opt/bin/codex", "-c", "model=test"],
                "codex",
            ),
            (
                "bash",
                &["bash", "--rcfile", "/tmp/profile", "--", "/opt/bin/claude"],
                "claude",
            ),
            (
                "bash",
                &["bash", "-lc", "FOO=1 /usr/local/bin/pi --model x"],
                "pi",
            ),
            (
                "node",
                &[
                    "/home/me/.local/share/cursor-agent/versions/2025.09.18/node",
                    "/home/me/.local/share/cursor-agent/versions/2025.09.18/index.js",
                ],
                "cursor-agent",
            ),
            (
                "node",
                &[
                    "node",
                    "/home/me/.nvm/versions/node/v22/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js",
                ],
                "pi",
            ),
            (
                "node",
                &["node", "/opt/node_modules/@sourcegraph/amp/dist/main.js"],
                "amp",
            ),
            // Relative script path containing a scoped package.
            (
                "node",
                &["node", "node_modules/@anthropic-ai/claude-code/cli.js"],
                "claude",
            ),
            // `-u` is a flag without a value for python.
            (
                "python3",
                &["python3", "-u", "/home/me/.local/bin/aider"],
                "aider",
            ),
            ("env", &["env", "FOO=1", "BAR=2", "claude"], "claude"),
        ];
        for (name, cmdline, expected) in cases {
            assert_eq!(
                agent_of(name, cmdline),
                Some(*expected),
                "{name} {cmdline:?}"
            );
        }
    }

    #[test]
    fn shells_and_other_programs_are_not_agents() {
        let cases: &[(&str, &[&str])] = &[
            ("bash", &["-bash"]),
            ("zsh", &["/bin/zsh", "-l"]),
            ("fish", &["fish"]),
            ("sh", &["sh", "-c", "make test"]),
            ("sh", &["sh", "/tmp/maintenance.sh", "claude"]),
            ("sh", &["sh", "-s", "claude"]),
            ("bash", &["bash", "--rcfile", "/tmp/claude"]),
            ("nvim", &["nvim", "claude.md"]),
            ("cargo", &["cargo", "test"]),
            ("node", &["node", "server.js"]),
            ("python3", &["python3", "-c", "import aider"]),
            ("node", &["node", "-e", "require('codex')"]),
            ("less", &["less", "/tmp/opencode.log"]),
            ("pip", &["pip", "install", "aider-chat"]),
        ];
        for (name, cmdline) in cases {
            assert_eq!(agent_of(name, cmdline), None, "{name} {cmdline:?}");
        }
    }

    #[test]
    fn titles_identify_agents_only_without_a_recognisable_process() {
        // No process information.
        assert_eq!(
            identify(None, "✳ Claude Code").map(|p| p.label),
            Some("claude")
        );
        assert_eq!(
            identify(None, "OpenCode").map(|p| p.label),
            Some("opencode")
        );
        assert_eq!(identify(None, "π - demo").map(|p| p.label), Some("pi"));
        assert_eq!(identify(None, "~/demo").map(|p| p.label), None);
        assert_eq!(identify(None, "").map(|p| p.label), None);
        // Opaque wrappers: ssh, an unrecognised node script.
        let ssh = process("ssh", &["ssh", "devbox"]);
        assert_eq!(
            identify(Some(&ssh), "◐ Claude Code").map(|p| p.label),
            Some("claude")
        );
        let script = process("node", &["node", "/srv/wrapper.js"]);
        assert_eq!(
            identify(Some(&script), "codex - demo").map(|p| p.label),
            Some("codex")
        );
        // A shell with a stale agent title is a shell.
        let zsh = process("zsh", &["-zsh"]);
        assert_eq!(identify(Some(&zsh), "✳ Claude Code").map(|p| p.label), None);
        // Other programs keep their own identity.
        let nvim = process("nvim", &["nvim", "opencode.json"]);
        assert_eq!(
            identify(Some(&nvim), "opencode.json - NVIM").map(|p| p.label),
            None
        );
    }

    #[test]
    fn plain_shell_detects_nothing() {
        let bash = process("bash", &["-bash"]);
        let detection = detect(&DetectInput {
            screen: "me@box:~/demo$ ls\nCargo.toml  src\nme@box:~/demo$",
            foreground: Some(&bash),
            alternate_screen: false,
            title: "",
            ms_since_output: Some(10),
        });
        assert_eq!(
            detection,
            Detection {
                agent: None,
                lifecycle: Lifecycle::None
            }
        );
    }

    #[test]
    fn claude_activity_lines() {
        for line in [
            "✶ Pondering… (12s · ↓ 1.2k tokens)",
            "* Thinking… (esc to interrupt)",
            "✻ Running PostToolUse hooks…",
            "· Crafting…",
        ] {
            assert!(is_claude_activity_line(line), "{line}");
        }
        for line in [
            "✻ Cogitated for 0s · done 12:53 PM",
            "✻ Worked for 1m 30s",
            "* Updated the tests… and docs",
            "⏺ Read(src/lib.rs)…",
            "✶",
            "",
        ] {
            assert!(!is_claude_activity_line(line), "{line}");
        }
        assert!(claude::is_claude_background_wait_line(
            "✻ Waiting for 1 background agent to finish"
        ));
        assert!(claude::is_claude_background_wait_line(
            "✢ Waiting for 3 background agents to finish"
        ));
        assert!(!claude::is_claude_background_wait_line(
            "⏺ Waiting for 1 background agent to finish"
        ));
        assert!(!claude::is_claude_background_wait_line(
            "I'm waiting for the background agent to finish."
        ));
    }

    #[test]
    fn small_predicates() {
        assert!(is_numbered_yes_option("❯ 1. Yes"));
        assert!(is_numbered_yes_option("2. Yes, and don't ask again"));
        assert!(is_numbered_yes_option("› 1. Yes, proceed (y)"));
        assert!(is_numbered_yes_option("1. Yes"));
        assert!(!is_numbered_yes_option("3. No, and tell Claude what to do"));
        assert!(!is_numbered_yes_option("1. Yesterday's notes"));
        assert!(!is_numbered_yes_option("Yes"));
        assert!(has_opencode_progress_bar("■■■⬝⬝⬝⬝⬝  esc interrupt"));
        assert!(!has_opencode_progress_bar("■■■ done"));
        assert!(is_pi_loader_line("⠇ Running... (escape/ctrl+c to cancel)"));
        assert!(!is_pi_loader_line("⠇ done"));
        assert!(is_pi_rule_loader("── ⠼ Working ─────────"));
        assert!(is_pi_rule_loader(
            "── ⠋ Compacting context… (escape to cancel) ──"
        ));
        assert!(!is_pi_rule_loader("──────────────────────"));
        assert!(!is_pi_rule_loader("⠼ Working"));
        assert!(has_pi_context_usage("0.0%/0 (auto)    unknown"));
        assert!(has_pi_context_usage("↑12k ↓1.2k $0.123 12.3%/200k (auto)"));
        assert!(!has_pi_context_usage("50% done"));
        assert!(!has_pi_context_usage("a%/b"));
        assert!(is_aider_prompt(">"));
        assert!(is_aider_prompt("architect> fix it"));
        assert!(!is_aider_prompt("Tokens: 1.2k sent, 300 received."));
        assert!(!is_aider_prompt("src/lib.rs>"));
        assert!(is_cursor_activity_line("⬡ Generating.."));
        assert!(!is_cursor_activity_line("⬡ Done"));
        assert!(is_cursor_activity_line("⠰⠳ Thinking…"));
        assert!(is_cursor_activity_line("⠀⠞ Running cargo test"));
        assert!(!is_cursor_activity_line("⠰⠳⠘ Running"));
        assert!(!is_cursor_activity_line("ab Running"));
        assert!(has_braille_spinner_word("⠹ demo"));
        assert!(has_braille_spinner_word("demo ⠹"));
        assert!(!has_braille_spinner_word("demo"));
        assert!(has_word("codex - demo", "codex"));
        assert!(!has_word("codexes", "codex"));
        assert_eq!(
            strip_package_version("@openai/codex@0.46.0"),
            "@openai/codex"
        );
        assert_eq!(strip_package_version("opencode-ai@latest"), "opencode-ai");
        assert_eq!(strip_package_version("/a@b/c"), "/a@b/c");
        assert_eq!(
            strip_package_version("node_modules/@openai/codex/bin/codex.js"),
            "node_modules/@openai/codex/bin/codex.js"
        );
        assert!(is_env_assignment("NODE_OPTIONS=--max-old-space-size=4096"));
        assert!(!is_env_assignment("--flag=1"));
        assert!(!is_env_assignment("1X=2"));
    }

    #[test]
    fn recent_output_counts_only_for_agents_without_busy_markers() {
        let aider = process("aider", &["aider"]);
        let screen = "Here is the plan:\n1. Fix the parser\n2. Add tests for";
        let at = |ms| {
            detect(&DetectInput {
                screen,
                foreground: Some(&aider),
                alternate_screen: false,
                title: "",
                ms_since_output: ms,
            })
            .lifecycle
        };
        assert_eq!(at(Some(50)), Lifecycle::Working);
        assert_eq!(at(Some(RECENT_OUTPUT_MS)), Lifecycle::Unknown);
        assert_eq!(at(None), Lifecycle::Unknown);

        // Claude typing echoes output, but its prompt box says idle.
        let claude = process("claude", &["claude"]);
        let typing = "────\n❯ fix the te\n────\n  ⏸ manual mode on";
        let detection = detect(&DetectInput {
            screen: typing,
            foreground: Some(&claude),
            alternate_screen: true,
            title: "",
            ms_since_output: Some(5),
        });
        assert_eq!(detection.lifecycle, Lifecycle::Idle);
    }

    #[test]
    fn marker_needles_are_lowercase() {
        fn check(test: &Test, label: &str) {
            match test {
                Test::All(needles) | Test::Any(needles) => {
                    for needle in *needles {
                        assert_eq!(*needle, needle.to_lowercase(), "{label}: {needle}");
                    }
                }
                Test::Line(_) => {}
                Test::And(a, b) => {
                    check(a, label);
                    check(b, label);
                }
            }
        }
        for profile in PROFILES {
            for marker in profile
                .blocked
                .iter()
                .chain(profile.working)
                .chain(profile.idle)
            {
                check(&marker.test, profile.label);
            }
            for name in profile.names {
                assert_eq!(*name, name.to_lowercase());
            }
        }
    }

    // -- Fixtures ---------------------------------------------------------

    /// A screen with its context and the expected detection. Format:
    /// `# comment` lines, `key: value` headers (`agent`, `expect`,
    /// `process`, `cmdline`, `title`, `alternate_screen`,
    /// `ms_since_output`), a `---` line, then the screen.
    struct Fixture {
        path: PathBuf,
        expected: Detection,
        process: Option<ProcessInfo>,
        title: String,
        alternate_screen: bool,
        ms_since_output: Option<u64>,
        screen: String,
    }

    fn parse_lifecycle(value: &str) -> Lifecycle {
        match value {
            "working" => Lifecycle::Working,
            "blocked" => Lifecycle::Blocked,
            "idle" => Lifecycle::Idle,
            "unknown" => Lifecycle::Unknown,
            "none" => Lifecycle::None,
            other => panic!("unknown lifecycle {other:?}"),
        }
    }

    fn load_fixture(path: &Path) -> Fixture {
        let text = std::fs::read_to_string(path).unwrap();
        let (header, screen) = text
            .split_once("\n---\n")
            .unwrap_or_else(|| panic!("{}: missing --- separator", path.display()));
        let mut agent = None;
        let mut expect = None;
        let mut name = None;
        let mut cmdline = None;
        let mut exe = None;
        let mut title = String::new();
        let mut alternate_screen = false;
        let mut ms_since_output = None;
        for line in header
            .lines()
            .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        {
            let (key, value) = line
                .split_once(':')
                .unwrap_or_else(|| panic!("{}: bad header {line:?}", path.display()));
            let value = value.trim();
            match key {
                "agent" => agent = Some((value != "none").then(|| value.to_string())),
                "expect" => expect = Some(parse_lifecycle(value)),
                "process" => name = Some(value.to_string()),
                "cmdline" => {
                    cmdline = Some(
                        value
                            .split_whitespace()
                            .map(String::from)
                            .collect::<Vec<_>>(),
                    )
                }
                "exe" => exe = Some(PathBuf::from(value)),
                "title" => title = value.to_string(),
                "alternate_screen" => alternate_screen = value == "true",
                "ms_since_output" => ms_since_output = Some(value.parse().unwrap()),
                other => panic!("{}: unknown header {other:?}", path.display()),
            }
        }
        let process = name.map(|name| ProcessInfo {
            pid: 4242,
            cmdline: cmdline.clone().unwrap_or_else(|| vec![name.clone()]),
            name,
            exe: exe.clone(),
        });
        Fixture {
            path: path.to_path_buf(),
            expected: Detection {
                agent: agent.unwrap_or_else(|| panic!("{}: no agent header", path.display())),
                lifecycle: expect.unwrap_or_else(|| panic!("{}: no expect header", path.display())),
            },
            process,
            title,
            alternate_screen,
            ms_since_output,
            screen: screen.to_string(),
        }
    }

    fn fixtures() -> Vec<Fixture> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/detect");
        let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "txt"))
            .collect();
        paths.sort();
        paths.iter().map(|path| load_fixture(path)).collect()
    }

    #[test]
    fn fixtures_detect_as_expected() {
        let fixtures = fixtures();
        assert!(fixtures.len() >= 30, "only {} fixtures", fixtures.len());
        let failures: Vec<String> = fixtures
            .iter()
            .filter_map(|fixture| {
                let got = detect(&DetectInput {
                    screen: &fixture.screen,
                    foreground: fixture.process.as_ref(),
                    alternate_screen: fixture.alternate_screen,
                    title: &fixture.title,
                    ms_since_output: fixture.ms_since_output,
                });
                (got != fixture.expected).then(|| {
                    format!(
                        "{}: expected {:?}/{:?}, got {:?}/{:?}",
                        fixture.path.file_name().unwrap().to_string_lossy(),
                        fixture.expected.agent,
                        fixture.expected.lifecycle,
                        got.agent,
                        got.lifecycle
                    )
                })
            })
            .collect();
        assert!(failures.is_empty(), "\n{}", failures.join("\n"));
    }

    #[test]
    fn agents_whose_screens_show_their_prompt_are_known_by_label() {
        for agent in [
            "claude", "codex", "opencode", "pi", "copilot", "grok", "kimi", "aider",
        ] {
            assert!(knows_its_prompt(agent), "{agent}");
        }
        // A screen without markers reads idle for these, and generic
        // profiles know no prompt: their screen can't say it isn't drawn.
        for agent in [
            "amp",
            "droid",
            "cursor-agent",
            "gemini",
            "qwen",
            "devin",
            "",
            "bash",
        ] {
            assert!(!knows_its_prompt(agent), "{agent}");
        }
    }

    #[test]
    fn main_agents_have_fixtures_for_each_state() {
        use Lifecycle::{Blocked, Idle, Working};
        let fixtures = fixtures();
        let required: &[(&str, &[Lifecycle])] = &[
            ("claude", &[Working, Blocked, Idle]),
            ("codex", &[Working, Blocked, Idle]),
            ("opencode", &[Working, Blocked, Idle]),
            ("pi", &[Working, Blocked, Idle]),
            ("gemini", &[Working, Blocked, Idle]),
            ("aider", &[Working, Blocked, Idle]),
            ("cursor-agent", &[Working, Blocked, Idle]),
            ("copilot", &[Working, Blocked, Idle]),
            ("grok", &[Working, Blocked, Idle]),
            ("kimi", &[Working, Blocked, Idle]),
            ("droid", &[Working, Blocked, Idle]),
            ("amp", &[Working, Blocked, Idle]),
        ];
        for (agent, states) in required {
            for state in *states {
                assert!(
                    fixtures
                        .iter()
                        .any(|f| f.expected.agent.as_deref() == Some(*agent)
                            && f.expected.lifecycle == *state),
                    "no {agent} fixture expecting {state:?}"
                );
            }
        }
        assert!(fixtures
            .iter()
            .any(|f| f.expected.agent.is_none() && f.expected.lifecycle == Lifecycle::None));
    }
}
