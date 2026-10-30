//! Amp's thread resume and its cache directory.
//!
//! A thread (`T-<uuid>`) lives on ampcode.com; `amp threads continue <id>`
//! opens it again in a new Amp. The thread is the one the Rakumux plugin
//! bound to the session (the one Amp shows), else the one the process was
//! started on (`amp threads continue T-…`), else `--last`: Amp's own last
//! thread for the folder, preferring this terminal (a revived session is a
//! new one), as Codex and OpenCode fall back to the folder's newest.
//! `amp --no-tui` serves threads created elsewhere: it's a command, not a
//! conversation.
use super::*;

/// Switches that shape an interactive Amp.
pub(super) const AMP_SWITCHES: &[&str] = &[
    "--dangerously-allow-all",
    "--ide",
    "--no-ide",
    "--notifications",
    "--no-notifications",
    "--remote-control-terminal",
    "--no-remote-control-terminal",
    "--color",
    "--no-color",
    "--fast",
];
/// Options (one value) that shape an interactive Amp.
pub(super) const AMP_OPTIONS: &[&str] = &[
    "-m",
    "--mode",
    "--settings-file",
    "--mcp-config",
    "--log-level",
    "--log-file",
    "--features",
];

/// What to run for an `amp` process: `None` when it isn't a conversation
/// (`--no-tui`, a runner), else its resume command (`Some(None)` when no
/// thread can be named, which can't happen: `--last` always can).
pub(super) fn amp_resume(
    process: &ProcessInfo,
    bound: Option<&str>,
) -> Option<Option<Vec<String>>> {
    let args = program_args(&process.cmdline, "amp");
    if args.iter().any(|arg| arg == "--no-tui") {
        return None;
    }
    let thread = bound
        .filter(|id| is_thread(id))
        .map(String::from)
        .or_else(|| continued_thread(&args));
    let mut argv: Vec<String> = ["amp", "threads", "continue"].map(String::from).into();
    match thread {
        Some(thread) => argv.push(thread),
        None => argv.push("--last".into()),
    }
    argv.extend(keep_flags(&args, AMP_SWITCHES, AMP_OPTIONS, &[]));
    Some(Some(argv))
}

/// An Amp thread id (`T-<uuid>`) that's safe to put on a command line.
fn is_thread(id: &str) -> bool {
    id.strip_prefix("T-").is_some_and(|rest| !rest.is_empty()) && is_conversation_id(id)
}

/// The thread `amp threads continue <id>` (or `amp t c <id>`) was started
/// on: the first id after the subcommand (a URL's last segment).
fn continued_thread(args: &[String]) -> Option<String> {
    let mut words = args.iter().map(String::as_str);
    words.find(|word| matches!(*word, "threads" | "thread" | "t"))?;
    words.find(|word| matches!(*word, "continue" | "c"))?;
    words
        .filter(|word| !word.starts_with('-'))
        .filter_map(|word| word.rsplit('/').next())
        .find(|id| is_thread(id))
        .map(String::from)
}

/// Amp's cache directory: `$XDG_CACHE_HOME/amp`, else `~/.cache/amp`
/// (macOS too). Plugins write under its `plugin-cache/<plugin>/`.
pub(crate) fn amp_cache_dir() -> Option<PathBuf> {
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".cache")))?;
    Some(cache.join("amp"))
}

/// Where Amp keeps itself: its install (`$AMP_HOME`, else `~/.amp`), its
/// cache and its settings. Never places.
pub(crate) fn amp_data_dirs() -> Vec<PathBuf> {
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".config")))
        .map(|config| config.join("amp"));
    let install = std::env::var_os("AMP_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| home_dir().map(|home| home.join(".amp")));
    [install, amp_cache_dir(), config]
        .into_iter()
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn amp(cmdline: &[&str]) -> ProcessInfo {
        ProcessInfo {
            pid: 7,
            name: "amp".into(),
            cmdline: cmdline.iter().map(|word| word.to_string()).collect(),
            exe: None,
        }
    }

    fn plan_for(cmdline: &[&str], bound: Option<&str>) -> Plan {
        plan(&Context {
            process: &amp(cmdline),
            cwd: None,
            bound_conversation: bound,
        })
    }

    fn resume(words: &[&str]) -> Plan {
        Plan::Resume(words.iter().map(|word| word.to_string()).collect())
    }

    const BOUND: &str = "T-5f0c1d2e-3a4b-4c5d-8e9f-0a1b2c3d4e5f";

    #[test]
    fn the_bound_thread_is_continued_with_the_flags_that_shape_amp() {
        assert_eq!(
            plan_for(
                &[
                    "/home/me/.local/bin/amp",
                    "--dangerously-allow-all",
                    "-m",
                    "high",
                    "--visibility",
                    "workspace",
                    "--settings-file=/w/amp.json",
                    "--no-ide",
                ],
                Some(BOUND)
            ),
            resume(&[
                "amp",
                "threads",
                "continue",
                BOUND,
                "--dangerously-allow-all",
                "-m",
                "high",
                "--settings-file=/w/amp.json",
                "--no-ide",
            ])
        );
    }

    #[test]
    fn a_thread_started_elsewhere_or_none_is_named_by_the_process_or_last() {
        let older = "T-11111111-2222-3333-4444-555555555555";
        // The binding wins over the thread the process was started on.
        assert_eq!(
            plan_for(
                &["amp", "threads", "continue", older, "--fast"],
                Some(BOUND)
            ),
            resume(&["amp", "threads", "continue", BOUND, "--fast"])
        );
        // Without one, that thread, by id or URL, through the `t c` alias.
        assert_eq!(
            plan_for(&["amp", "t", "c", older], None),
            resume(&["amp", "threads", "continue", older])
        );
        assert_eq!(
            plan_for(
                &[
                    "amp",
                    "threads",
                    "continue",
                    &format!("https://ampcode.com/threads/{older}")
                ],
                None
            ),
            resume(&["amp", "threads", "continue", older])
        );
        // Otherwise Amp's last thread for the folder.
        assert_eq!(
            plan_for(&["amp", "-m", "low"], None),
            resume(&["amp", "threads", "continue", "--last", "-m", "low"])
        );
        // A binding that isn't a thread id isn't used.
        assert_eq!(
            plan_for(&["amp"], Some("T-1; rm -rf ~")),
            resume(&["amp", "threads", "continue", "--last"])
        );
    }

    #[test]
    fn one_shot_prompts_and_stream_output_resume_interactively() {
        assert_eq!(
            plan_for(
                &[
                    "amp",
                    "-x",
                    "fix the parser",
                    "--stream-json",
                    "--title",
                    "Parser",
                    "-l",
                    "bugfix",
                    "--dangerously-allow-all"
                ],
                Some(BOUND)
            ),
            resume(&[
                "amp",
                "threads",
                "continue",
                BOUND,
                "--dangerously-allow-all"
            ])
        );
        // Node running the npm wrapper.
        let process = ProcessInfo {
            name: "node".into(),
            cmdline: [
                "node",
                "/usr/lib/node_modules/@ampcode/cli/cli-wrapper.cjs",
                "--mode",
                "ultra",
            ]
            .map(String::from)
            .into(),
            ..amp(&[])
        };
        assert_eq!(
            plan(&Context {
                process: &process,
                cwd: None,
                bound_conversation: Some(BOUND),
            }),
            resume(&["amp", "threads", "continue", BOUND, "--mode", "ultra"])
        );
    }

    #[test]
    fn a_runner_is_a_command_not_a_conversation() {
        assert_eq!(
            plan_for(&["amp", "--no-tui", "--share"], Some(BOUND)),
            Plan::Command(["amp", "--no-tui", "--share"].map(String::from).into())
        );
    }
}
